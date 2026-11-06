//! Changing a person's settings from the admin dashboard. The rules are
//! `settings_edit`'s, the same as `/settings` in WhatsApp.
//!
//! Settings belong to a person's DM with the bot. Someone who has never
//! messaged the bot privately has no DM yet, so the first change creates
//! it, under their phone-number ID: the same chat ID their first private
//! message will have (and `chats::find_or_create` finds it even if it
//! doesn't).
//!
//! Changes aren't announced to the person; each one is written to the
//! server log. Every endpoint answers with the person's updated page data.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;
use tracing::info;

use crate::AppState;
use crate::error::AppError;
use crate::prayer;
use crate::repo::settings as stored;
use crate::repo::{admin, chats, todo_reminders};
use crate::routes::admin::{UserDetail, detail};
use crate::settings_edit::{self, Change, Checked, ENDING_MINUTES, Refused};

/// The body of `PATCH /api/users/{id}/settings`: only the settings to
/// change. `deny_unknown_fields` turns a misspelt name into a 4xx error
/// rather than a change that silently does nothing.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    /// A town to look up. Its timezone comes with it, unless `timezone` is
    /// given too.
    city: Option<String>,
    /// The town's country (a 2-letter code works best). Leave it out to
    /// keep the current one.
    country: Option<String>,
    /// An IANA name, e.g. "Europe/London".
    timezone: Option<String>,
    prayer_method: Option<u8>,
    /// 0 = Shafi'i, 1 = Hanafi.
    asr_school: Option<u8>,
    ending_minutes: Option<i64>,
}

/// The body of the reminder switches: `{"on": true}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Switch {
    pub(crate) on: bool,
}

/// `PATCH /api/users/{id}/settings`: change one or more settings. All or
/// nothing: if any change is refused, none are saved, and the 400 says why.
pub async fn change_settings(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(patch): Json<SettingsPatch>,
) -> Result<Json<UserDetail>, AppError> {
    // Checked first, outside any transaction: a new town is looked up over
    // the network, and that mustn't hold the database's write lock. A
    // refusal (400) or no such person (404) stops here, with nothing saved.
    let (changes, checked) = {
        let mut conn = state.db.acquire().await?;
        let dm = existing_dm(&mut conn, id).await?;
        check_patch(&mut conn, &state, dm, patch).await?
    };
    // A *transaction*: every change below is kept only if `commit` is
    // reached. Returning early (with `?`) drops `tx`, which rolls
    // everything back, including a DM created for this.
    // `BEGIN IMMEDIATE` takes the write lock at once (see
    // `routes::webhook::handle_message`).
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let dm = dm_for(&mut tx, id).await?;
    save_patch(&mut tx, &state, dm, checked).await?;
    tx.commit().await?;
    info!("changed user {id}'s settings: {changes:?}");

    let mut conn = state.db.acquire().await?;
    Ok(Json(detail(&mut conn, &state, id).await?))
}

/// `POST /api/users/{id}/settings/reset`: back to the server defaults.
/// Prayer reminders stay on or off, as with `/settings reset`.
pub async fn reset_settings(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<UserDetail>, AppError> {
    let mut conn = state.db.acquire().await?;
    // No DM means nothing was ever set: already the defaults.
    if let Some(dm) = existing_dm(&mut conn, id).await? {
        stored::reset(&mut conn, dm).await?;
        info!("reset user {id}'s settings");
    }
    Ok(Json(detail(&mut conn, &state, id).await?))
}

/// `POST /api/users/{id}/prayer-reminders` with `{"on": true}`: their
/// prayer reminders, in their DM.
pub async fn prayer_reminders(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(switch): Json<Switch>,
) -> Result<Json<UserDetail>, AppError> {
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let dm = dm_for(&mut tx, id).await?;
    stored::set_prayer_reminders(&mut tx, dm, switch.on).await?;
    tx.commit().await?;
    info!("user {id}'s prayer reminders: {}", on_off(switch.on));

    let mut conn = state.db.acquire().await?;
    Ok(Json(detail(&mut conn, &state, id).await?))
}

/// `POST /api/users/{id}/chats/{chat}/todo-reminders` with `{"on": true}`:
/// their todo reminders in one chat. 404 unless it's their DM or a group
/// they're in.
pub async fn todo_reminders(
    State(state): State<Arc<AppState>>,
    // Two `{...}` in the path, so a tuple: in the order they appear.
    Path((id, chat)): Path<(i64, i64)>,
    Json(switch): Json<Switch>,
) -> Result<Json<UserDetail>, AppError> {
    let mut conn = state.db.acquire().await?;
    if !admin::is_in_chat(&mut conn, id, chat).await? {
        return Err(AppError::NotFound);
    }
    if switch.on {
        todo_reminders::enable(&mut conn, chat, id, Utc::now()).await?;
    } else {
        todo_reminders::disable(&mut conn, chat, id).await?;
    }
    info!(
        "user {id}'s todo reminders in chat {chat}: {}",
        on_off(switch.on)
    );
    Ok(Json(detail(&mut conn, &state, id).await?))
}

/// One prayer calculation method, for the dashboard's dropdown.
#[derive(Debug, Serialize)]
pub struct Method {
    id: u8,
    name: &'static str,
}

/// `GET /api/prayer-methods`: the methods AlAdhan knows, as `/settings
/// methods` lists them.
pub async fn prayer_methods() -> Json<Vec<Method>> {
    Json(
        prayer::METHODS
            .iter()
            .map(|&(id, name)| Method { id, name })
            .collect(),
    )
}

// ---- helpers ----

/// The first half of a settings PATCH, run *before* the transaction:
/// turns the patch into changes and checks each one, looking a new town
/// up over the network (see `settings_edit::check`). Stops at the first
/// refusal, with a 400 saying why; nothing has been saved by then.
///
/// `chat_id` is the chat whose settings these are (a DM's or a group's),
/// or `None` for a person with no DM yet, who has the defaults. Returns
/// the changes (for the log) and what to save (for `save_patch`).
pub(crate) async fn check_patch(
    conn: &mut SqliteConnection,
    state: &AppState,
    chat_id: Option<i64>,
    patch: SettingsPatch,
) -> Result<(Vec<Change>, Vec<Checked>), AppError> {
    let defaults = &state.apis.prayer_times.defaults;
    // The chat's own settings (no person on top): they're what's edited.
    let current_country = match chat_id {
        Some(chat_id) => {
            stored::effective(conn, defaults, chat_id, None)
                .await?
                .country
        }
        None => defaults.country.clone(),
    };
    let changes = changes(patch, &current_country)?;
    let mut checked = Vec::new();
    for change in &changes {
        match settings_edit::check(&state.apis.geo, change).await {
            Ok(ok) => checked.push(ok),
            Err(refused) => return Err(AppError::BadRequest(explain(&refused))),
        }
    }
    Ok((changes, checked))
}

/// The second half, inside the transaction: saves what `check_patch`
/// checked to chat `chat_id`'s settings, in order. Only the database.
pub(crate) async fn save_patch(
    conn: &mut SqliteConnection,
    state: &AppState,
    chat_id: i64,
    checked: Vec<Checked>,
) -> Result<(), AppError> {
    let defaults = &state.apis.prayer_times.defaults;
    for change in checked {
        // Each change sees the ones before it (a new town's timezone, say).
        let current = stored::effective(conn, defaults, chat_id, None).await?;
        settings_edit::save(conn, chat_id, &current, change).await?;
    }
    Ok(())
}

/// The person's DM, created if they don't have one. 404 if there's no such
/// person.
pub(crate) async fn dm_for(conn: &mut SqliteConnection, id: i64) -> Result<i64, AppError> {
    if let Some(dm) = existing_dm(conn, id).await? {
        return Ok(dm);
    }
    // `users.wa_jid` is always a phone-number ID: the DM's chat ID.
    let jid = admin::user_jid(conn, id).await?.ok_or(AppError::NotFound)?;
    let now = Utc::now();
    let dm = chats::find_or_create(conn, &jid, Some(id), now).await?;
    chats::add_member(conn, dm, id, now).await?;
    Ok(dm)
}

/// The person's DM if they have one; 404 if there's no such person.
pub(crate) async fn existing_dm(
    conn: &mut SqliteConnection,
    id: i64,
) -> Result<Option<i64>, AppError> {
    if admin::user_jid(conn, id).await?.is_none() {
        return Err(AppError::NotFound);
    }
    Ok(admin::dm_of(conn, id).await?)
}

/// The patch as a list of changes, town first so an explicit timezone
/// wins over the town's.
fn changes(patch: SettingsPatch, current_country: &str) -> Result<Vec<Change>, AppError> {
    let mut changes = Vec::new();
    match (patch.city, patch.country) {
        (Some(city), country) if !city.trim().is_empty() => changes.push(Change::City {
            city: city.trim().to_string(),
            country: country
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| current_country.to_string()),
        }),
        (Some(_), _) => return Err(AppError::BadRequest("The town can't be empty.".to_string())),
        (None, Some(_)) => {
            return Err(AppError::BadRequest(
                "Give the town as well as the country.".to_string(),
            ));
        }
        (None, None) => {}
    }
    if let Some(timezone) = patch.timezone {
        changes.push(Change::Timezone(timezone.trim().to_string()));
    }
    if let Some(method) = patch.prayer_method {
        changes.push(Change::PrayerMethod(method));
    }
    if let Some(school) = patch.asr_school {
        changes.push(Change::PrayerSchool(school));
    }
    if let Some(minutes) = patch.ending_minutes {
        changes.push(Change::PrayerEnding(minutes));
    }
    if changes.is_empty() {
        return Err(AppError::BadRequest("Nothing to change.".to_string()));
    }
    Ok(changes)
}

/// Why a change was refused, for the dashboard.
fn explain(refused: &Refused) -> String {
    match refused {
        Refused::UnknownTown { city, country } => {
            format!("Couldn't find {city}, {country}. Check the spelling, or try a nearby town.")
        }
        Refused::UnknownTimezone(name) => {
            format!("\"{name}\" isn't a timezone. Use a name like Europe/London or Asia/Karachi.")
        }
        Refused::UnknownMethod(method) => format!("There's no prayer method {method}."),
        Refused::UnknownSchool(_) => "Asr must be Shafi'i (0) or Hanafi (1).".to_string(),
        Refused::EndingOutOfRange(_) => format!(
            "The \"ending soon\" reminder must be {} to {} minutes before.",
            ENDING_MINUTES.start(),
            ENDING_MINUTES.end()
        ),
    }
}

pub(crate) fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;
    use serde_json::{Value, json};

    use crate::db::test_pool;
    use crate::repo::{chats, settings as stored, todo_reminders};
    use crate::testkit::{fake_geocoder, seed_family, start_server_with_geocoder};

    async fn send(method: reqwest::Method, url: String, body: Value) -> (StatusCode, Value) {
        let response = reqwest::Client::new()
            .request(method, url)
            .basic_auth("test", Some("test"))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn patch(server: &str, id: i64, body: Value) -> (StatusCode, Value) {
        send(
            reqwest::Method::PATCH,
            format!("{server}/api/users/{id}/settings"),
            body,
        )
        .await
    }

    /// Bilal (who has a DM) and Khadija (who doesn't), both in a group
    /// (`testkit::seed_family`). Returns (Bilal, Khadija, the group, Bilal's DM).
    async fn seed(pool: &sqlx::SqlitePool) -> (i64, i64, i64, i64) {
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, chrono::Utc::now()).await;
        (family.bilal.id, family.khadija.id, family.group, family.dm)
    }

    #[tokio::test]
    async fn changes_are_saved_and_shown() {
        let pool = test_pool().await;
        let (bilal, _, _, dm) = seed(&pool).await;
        let server = start_server_with_geocoder(pool.clone(), &fake_geocoder().await).await;

        let (status, page) = patch(
            &server,
            bilal,
            json!({ "city": "leicester", "country": "gb", "prayer_method": 2, "asr_school": 1, "ending_minutes": 30 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let settings = &page["settings"];
        assert_eq!(
            settings["place"],
            json!({ "value": "Leicester, GB", "own": true })
        );
        assert_eq!(
            settings["timezone"],
            json!({ "value": "Europe/London", "own": true })
        );
        assert_eq!(
            settings["asr_school"],
            json!({ "value": "Hanafi", "own": true })
        );
        assert_eq!(settings["values"]["prayer_method"], 2);
        assert_eq!(settings["values"]["ending_minutes"], 30);
        // The town's coordinates are saved, but never sent.
        assert!(!page.to_string().contains("52.63"), "{page}");

        // What the bot will use: the same row WhatsApp's `/settings` reads.
        let mut conn = pool.acquire().await.unwrap();
        let own = stored::get(&mut conn, dm).await.unwrap().overrides;
        assert_eq!(own.city.as_deref(), Some("Leicester"));
        assert_eq!(own.prayer_ending_minutes, Some(30));
        drop(conn);

        // An explicit timezone wins over the town's.
        let (_, page) = patch(&server, bilal, json!({ "timezone": "Asia/Karachi" })).await;
        assert_eq!(page["settings"]["timezone"]["value"], "Asia/Karachi");
    }

    #[tokio::test]
    async fn refusals_explain_and_save_nothing() {
        let pool = test_pool().await;
        let (bilal, _, _, dm) = seed(&pool).await;
        let server = start_server_with_geocoder(pool.clone(), &fake_geocoder().await).await;

        for (body, says) in [
            (json!({ "timezone": "Mars/Olympus" }), "isn't a timezone"),
            (json!({ "ending_minutes": 121 }), "1 to 120 minutes"),
            (json!({ "ending_minutes": 0 }), "1 to 120 minutes"),
            (json!({ "prayer_method": 6 }), "no prayer method 6"),
            (json!({ "asr_school": 2 }), "Shafi'i (0) or Hanafi (1)"),
            (
                json!({ "city": "Nowhereville" }),
                "Couldn't find Nowhereville, GB",
            ),
            (json!({ "country": "FR" }), "town as well as the country"),
            (json!({}), "Nothing to change"),
            // All or nothing: the good timezone isn't saved either.
            (
                json!({ "timezone": "Asia/Karachi", "ending_minutes": 500 }),
                "1 to 120",
            ),
        ] {
            let (status, answer) = patch(&server, bilal, body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            let error = answer["error"].as_str().unwrap();
            assert!(error.contains(says), "{body}: {error}");
        }
        let mut conn = pool.acquire().await.unwrap();
        assert_eq!(
            stored::get(&mut conn, dm).await.unwrap(),
            stored::ChatSettings::default()
        );
        drop(conn);

        // Not a setting at all, or the wrong type: Axum's own 4xx.
        let (status, _) = patch(&server, bilal, json!({ "colour": "blue" })).await;
        assert!(status.is_client_error());
        let (status, _) = patch(&server, bilal, json!({ "ending_minutes": "ten" })).await;
        assert!(status.is_client_error());
        let (status, _) = patch(&server, 999, json!({ "ending_minutes": 10 })).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_first_change_creates_their_dm_which_whatsapp_then_uses() {
        let pool = test_pool().await;
        let (_, khadija, _, _) = seed(&pool).await;
        let server = start_server_with_geocoder(pool.clone(), &fake_geocoder().await).await;

        // A refused change creates nothing.
        patch(&server, khadija, json!({ "ending_minutes": 999 })).await;
        let mut conn = pool.acquire().await.unwrap();
        assert_eq!(
            crate::repo::admin::dm_of(&mut conn, khadija).await.unwrap(),
            None
        );
        drop(conn);

        let (status, page) = patch(&server, khadija, json!({ "ending_minutes": 15 })).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["user"]["has_dm"], true);

        // Her first private message finds the same chat, settings and all.
        let mut conn = pool.acquire().await.unwrap();
        let dm = crate::repo::admin::dm_of(&mut conn, khadija)
            .await
            .unwrap()
            .unwrap();
        let first_message = chats::find_or_create(
            &mut conn,
            "447700900002@s.whatsapp.net",
            Some(khadija),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(first_message, dm);
        let own = stored::get(&mut conn, dm).await.unwrap().overrides;
        assert_eq!(own.prayer_ending_minutes, Some(15));
    }

    #[tokio::test]
    async fn reset_and_reminder_switches() {
        let pool = test_pool().await;
        let (bilal, khadija, group, dm) = seed(&pool).await;
        let server = start_server_with_geocoder(pool.clone(), &fake_geocoder().await).await;
        let post = |path: String, body: Value| {
            send(reqwest::Method::POST, format!("{server}{path}"), body)
        };

        patch(&server, bilal, json!({ "timezone": "Asia/Karachi" })).await;
        let (status, page) = post(
            format!("/api/users/{bilal}/prayer-reminders"),
            json!({ "on": true }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["settings"]["prayer_reminders"], true);

        let (status, page) = post(format!("/api/users/{bilal}/settings/reset"), json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["settings"]["timezone"]["own"], false);
        // Reset leaves reminders as they were, like `/settings reset`.
        assert_eq!(page["settings"]["prayer_reminders"], true);

        let (status, page) = post(
            format!("/api/users/{bilal}/chats/{group}/todo-reminders"),
            json!({ "on": true }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let group_card = page["chats"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == group)
            .unwrap()
            .clone();
        assert_eq!(group_card["todo_reminders"], true);
        let mut conn = pool.acquire().await.unwrap();
        assert!(
            todo_reminders::is_on(&mut conn, group, bilal)
                .await
                .unwrap()
        );
        drop(conn);

        let (_, page) = post(
            format!("/api/users/{bilal}/chats/{group}/todo-reminders"),
            json!({ "on": false }),
        )
        .await;
        let group_card = page["chats"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == group)
            .unwrap()
            .clone();
        assert_eq!(group_card["todo_reminders"], false);

        // Not her chat: 404, and nothing changes.
        let (status, _) = post(
            format!("/api/users/{khadija}/chats/{dm}/todo-reminders"),
            json!({ "on": true }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = post(
            format!("/api/users/{bilal}/prayer-reminders"),
            json!({ "on": "yes" }),
        )
        .await;
        assert!(status.is_client_error());
    }

    #[tokio::test]
    async fn report_delivery_and_prayer_stats_on_the_pages() {
        use crate::prayer::Prayer;
        use crate::prayer::checkin;

        let pool = test_pool().await;
        let (bilal, khadija, group, _) = seed(&pool).await;
        // Today in London, the test server's default timezone.
        let today = chrono::Utc::now()
            .with_timezone(&chrono_tz::Europe::London)
            .date_naive();
        {
            let mut conn = pool.acquire().await.unwrap();
            let now = chrono::Utc::now();
            checkin::tick(&mut conn, bilal, today, Prayer::Fajr, now)
                .await
                .unwrap();
            checkin::mark_missed(&mut conn, bilal, today, Prayer::Dhuhr, now)
                .await
                .unwrap();
        }
        let server = start_server_with_geocoder(pool.clone(), &fake_geocoder().await).await;
        let post = |path: String, body: Value| {
            send(reqwest::Method::POST, format!("{server}{path}"), body)
        };

        let (_, page) = send(
            reqwest::Method::GET,
            format!("{server}/api/users/{bilal}"),
            json!(null),
        )
        .await;
        assert_eq!(page["reports"]["prayers"], "here", "the default");
        let week = &page["prayers"]["week"];
        assert_eq!(week["prayed"], 1);
        assert_eq!(week["recorded"], 2);
        assert_eq!(week["percent"], 50);
        assert_eq!(week["days"].as_array().unwrap().len(), 7);
        assert_eq!(week["missed"], json!([{ "prayer": "Dhuhr", "count": 1 }]));

        let (status, page) = post(
            format!("/api/users/{bilal}/reports/prayers"),
            json!({ "delivery": "off" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["reports"]["prayers"], "off");

        // A person's reports are already private: "dm" is for groups.
        let (status, page) = post(
            format!("/api/users/{khadija}/reports/prayers"),
            json!({ "delivery": "dm" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(page["error"].as_str().unwrap().contains("only for a group"));
        let (status, _) = post(
            format!("/api/users/{bilal}/reports/weather"),
            json!({ "delivery": "off" }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "no such feature");

        let (status, page) = post(
            format!("/api/groups/{group}/reports/prayers"),
            json!({ "delivery": "dm" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["reports"]["prayers"], "dm");
        let members = page["members"].as_array().unwrap();
        let bilals = members.iter().find(|m| m["id"] == bilal).unwrap();
        assert_eq!(bilals["prayer_week"]["prayed"], 1);
        let (status, _) = post(
            format!("/api/groups/{group}/reports/prayers"),
            json!({ "delivery": "sometimes" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_month_of_prayers_can_be_seen_and_corrected() {
        let pool = test_pool().await;
        let (bilal, _, _, _) = seed(&pool).await;
        let server = start_server_with_geocoder(pool.clone(), &fake_geocoder().await).await;
        let today = chrono::Utc::now()
            .with_timezone(&chrono_tz::Europe::London)
            .date_naive();
        let get = |path: String| send(reqwest::Method::GET, format!("{server}{path}"), json!(null));
        let put =
            |path: String, body: Value| send(reqwest::Method::PUT, format!("{server}{path}"), body);

        let (status, page) = get(format!("/api/users/{bilal}/prayers")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["today"], today.to_string());
        let days = page["month"]["days"].as_array().unwrap();
        assert!(
            days.iter().any(|d| d["date"] == today.to_string()),
            "this month"
        );
        assert_eq!(days[0]["prayers"].as_array().unwrap().len(), 5);

        // Mark today's Asr prayed: the answer is the month, updated.
        let (status, page) = put(
            format!("/api/users/{bilal}/prayers/{today}/asr"),
            json!({ "status": "prayed" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let day = page["month"]["days"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["date"] == today.to_string())
            .unwrap()
            .clone();
        assert_eq!(day["prayed"], 1);
        assert_eq!(
            day["prayers"][2],
            json!({ "prayer": "asr", "status": "prayed" })
        );
        assert_eq!(day["prayers"][0]["status"], Value::Null);

        // Then missed, then cleared.
        let path = format!("/api/users/{bilal}/prayers/{today}/asr");
        let (_, page) = put(path.clone(), json!({ "status": "missed" })).await;
        assert_eq!(page["month"]["recorded"], 1);
        assert_eq!(page["month"]["prayed"], 0);
        let (_, page) = put(path.clone(), json!({ "status": null })).await;
        assert_eq!(page["month"]["recorded"], 0);

        // Another month, and the refusals.
        let (status, page) = get(format!("/api/users/{bilal}/prayers?month=2026-02")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["month"]["first"], "2026-02-01");
        assert_eq!(page["month"]["days"].as_array().unwrap().len(), 28);
        let (status, _) = get(format!("/api/users/{bilal}/prayers?month=feb")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let tomorrow = today.succ_opt().unwrap();
        let (status, _) = put(
            format!("/api/users/{bilal}/prayers/{tomorrow}/asr"),
            json!({ "status": "prayed" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "not yet");
        let (status, _) = put(path.clone(), json!({ "status": "skipped" })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = put(
            format!("/api/users/{bilal}/prayers/{today}/sunrise"),
            json!({ "status": "prayed" }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = get("/api/users/9999/prayers".to_string()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}

//! The admin dashboard's data: who uses the bot, their settings, and the
//! groups' lists. Read-only, and behind the password (`admin_auth`).
//!
//! Two privacy rules are enforced here, not only in the web app:
//! - A DM's lists are counted, never listed: `/api/chats/{id}/...` answers
//!   404 for a DM.
//! - Exact coordinates are never sent, only the town's name.
//!
//! The JSON shapes are their own structs (`UserRow`, `TodoView`, ...)
//! rather than the database rows, so a new column can't end up in the API
//! by accident.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sqlx::SqliteConnection;
use tracing::warn;

use crate::AppState;
use crate::cleanup;
use crate::error::AppError;
use crate::prayer;
use crate::repo::settings::{self as stored, ChatSettings};
use crate::repo::{admin, chats, shop, todos};
use crate::routes::admin_prayers::{PrayerView, prayer_view, today_for};
use crate::routes::admin_reports::{ReportsView, reports_view};
use crate::routes::admin_settings::existing_dm;
use crate::settings::Settings;

/// How far back the users page counts uses.
pub(crate) const RECENT_DAYS: i64 = 30;

// ---- JSON shapes ----
//
// `#[derive(Serialize)]` writes the code that turns a struct into JSON,
// one key per field. `DateTime<Utc>` becomes an RFC 3339 string
// ("2026-11-02T09:30:00Z"), and `Option` becomes the value or `null`.

#[derive(Debug, Serialize)]
pub struct UserList {
    /// The period `recent_uses` covers.
    recent_days: i64,
    cleanup: CleanupView,
    users: Vec<UserRow>,
}

/// How clearing inactive people's data is set up (`INACTIVE_DAYS`).
#[derive(Debug, Serialize)]
pub(crate) struct CleanupView {
    /// Days without using the bot before their data is cleared; 0 = never.
    inactive_days: i64,
    /// How many days before clearing the warning goes.
    warn_days: i64,
}

impl CleanupView {
    pub(crate) fn of(state: &AppState) -> Self {
        CleanupView {
            inactive_days: state.config.inactive_days,
            warn_days: cleanup::WARN_DAYS,
        }
    }
}

#[derive(Debug, Serialize)]
struct UserRow {
    id: i64,
    name: String,
    first_seen: DateTime<Utc>,
    last_seen: Option<DateTime<Utc>>,
    recent_uses: i64,
    groups: i64,
    has_dm: bool,
    /// When they last used the bot (any message in their DM counts too).
    last_active: DateTime<Utc>,
    warned_at: Option<DateTime<Utc>>,
    /// Their data was cleared then; only their name is left.
    cleared_at: Option<DateTime<Utc>>,
    /// When their data will be cleared if they don't use the bot before
    /// then; `null` if it won't be (clearing is off, or already done).
    clears_on: Option<DateTime<Utc>>,
}

/// `From` is the standard way to say "this type can be made from that
/// one". Implementing it gives `.into()` for free: `summary.into()`.
impl From<admin::UserSummary> for UserRow {
    fn from(u: admin::UserSummary) -> Self {
        UserRow {
            id: u.id,
            name: u.name,
            first_seen: u.first_seen,
            last_seen: u.last_seen,
            recent_uses: u.uses_since,
            groups: u.groups,
            has_dm: u.has_dm,
            last_active: u.last_active,
            warned_at: u.warned_at,
            cleared_at: u.cleared_at,
            clears_on: None,
        }
    }
}

/// A person's row, with when their data will be cleared filled in, by the
/// same rules the cleanup itself follows.
fn user_row(u: admin::UserSummary, state: &AppState, now: DateTime<Utc>) -> UserRow {
    let activity = cleanup::Activity {
        last_active: u.last_active,
        warned_at: u.warned_at,
        cleared_at: u.cleared_at,
        can_warn: u.has_dm,
    };
    let clears_on = cleanup::clears_on(&activity, state.config.inactive_days, now);
    UserRow {
        clears_on,
        // `..` fills in every other field from the plain conversion.
        ..UserRow::from(u)
    }
}

#[derive(Debug, Serialize)]
pub struct UserDetail {
    recent_days: i64,
    cleanup: CleanupView,
    user: UserRow,
    settings: SettingsView,
    /// Where each feature's reports go, in their DM: "here" or "off".
    reports: ReportsView,
    /// Their prayers this week and this month (counts, never times).
    prayers: PrayerView,
    chats: Vec<ChatView>,
}

/// One setting: the value in use, and whether it's the person's own choice
/// (`true`) or the server default (`false`).
///
/// `<T>` makes it *generic*: `Setting<String>` and `Setting<i64>` are both
/// this struct, with a different type of `value`.
#[derive(Debug, PartialEq, Serialize)]
struct Setting<T> {
    value: T,
    own: bool,
}

/// A person's settings (from their DM with the bot), defaults filled in.
#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct SettingsView {
    /// "Crawley, GB". The town only: never its coordinates.
    place: Setting<String>,
    timezone: Setting<String>,
    /// "Muslim World League (3)".
    prayer_method: Setting<String>,
    asr_school: Setting<String>,
    /// Minutes before a prayer ends that the "ending soon" reminder goes.
    ending_minutes: Setting<i64>,
    /// `/prayers on` in their DM.
    prayer_reminders: bool,
    /// The same settings as plain values, for the dashboard's forms.
    values: Values,
}

/// The settings in use, as the edit endpoints take them.
#[derive(Debug, PartialEq, Serialize)]
struct Values {
    city: String,
    country: String,
    timezone: String,
    prayer_method: u8,
    asr_school: u8,
    ending_minutes: i64,
}

#[derive(Debug, Serialize)]
struct ChatView {
    id: i64,
    /// `"group"` or `"dm"`.
    kind: String,
    name: Option<String>,
    open_todos: i64,
    done_todos: i64,
    to_buy: i64,
    bought: i64,
    todo_reminders: bool,
    /// For a group: when someone last used the bot there.
    last_active: DateTime<Utc>,
    /// For a group: when the bot posted that it would leave.
    warned_at: Option<DateTime<Utc>>,
    /// For a group: the bot isn't in it any more (seen then); it's deleted
    /// a day after.
    left_at: Option<DateTime<Utc>>,
    /// For a group: when the bot will leave and delete it if nobody uses it
    /// before then; `null` if it won't (clearing is off, or it's a DM).
    leaves_on: Option<DateTime<Utc>>,
}

/// A chat as the user page shows it, with when the bot would leave a quiet
/// group worked out by the same rules the cleanup follows.
fn chat_view(c: admin::UserChat, state: &AppState, now: DateTime<Utc>) -> ChatView {
    let leaves_on = (c.kind == "group")
        .then(|| leaves_on(c.last_active, c.warned_at, c.left_at, state, now))
        .flatten();
    ChatView {
        id: c.id,
        kind: c.kind,
        name: c.name,
        open_todos: c.open_todos,
        done_todos: c.done_todos,
        to_buy: c.to_buy,
        bought: c.bought,
        todo_reminders: c.todo_reminders,
        last_active: c.last_active,
        warned_at: c.warned_at,
        left_at: c.left_at,
        leaves_on,
    }
}

#[derive(Debug, Serialize)]
struct GroupView {
    id: i64,
    name: Option<String>,
    /// The group's timezone, for showing when things were done.
    timezone: String,
}

#[derive(Debug, Serialize)]
pub struct GroupTodos {
    group: GroupView,
    /// In `/todo all` order: open (oldest first), then done.
    todos: Vec<TodoView>,
}

#[derive(Debug, Serialize)]
struct TodoView {
    id: i64,
    title: String,
    assignee: Option<String>,
    /// Who closed it and when: ticked off, or skipped if `skipped`.
    done_by: Option<String>,
    done_at: Option<DateTime<Utc>>,
    /// Closed as "won't be done" rather than done.
    skipped: bool,
}

#[derive(Debug, Serialize)]
pub struct GroupShop {
    group: GroupView,
    /// In `/shop` order: to buy (oldest first), then bought.
    items: Vec<ItemView>,
}

#[derive(Debug, Serialize)]
struct ItemView {
    id: i64,
    name: String,
    bought: bool,
}

// ---- handlers ----

/// `GET /api/users`: everyone, most recently active first.
pub async fn list_users(State(state): State<Arc<AppState>>) -> Result<Json<UserList>, AppError> {
    refresh_group_names(&state).await;
    let mut conn = state.db.acquire().await?;
    let now = Utc::now();
    let since = now - Duration::days(RECENT_DAYS);
    let users = admin::users(&mut conn, None, since).await?;
    Ok(Json(UserList {
        recent_days: RECENT_DAYS,
        cleanup: CleanupView::of(&state),
        // `map` converts each one; `collect` gathers them back into a `Vec`.
        users: users
            .into_iter()
            .map(|u| user_row(u, &state, now))
            .collect(),
    }))
}

/// `GET /api/users/{id}`: one person, their settings and their chats.
///
/// `Path(id)` takes `{id}` from the URL as an `i64`. Axum answers 400 by
/// itself if it isn't a number, so this never runs then.
pub async fn user(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<UserDetail>, AppError> {
    let mut conn = state.db.acquire().await?;
    Ok(Json(detail(&mut conn, &state, id).await?))
}

/// Everything the user page shows about one person. Also what the edit
/// endpoints answer with, so the page shows the result straight away.
pub(crate) async fn detail(
    conn: &mut SqliteConnection,
    state: &AppState,
    id: i64,
) -> Result<UserDetail, AppError> {
    let now = Utc::now();
    let since = now - Duration::days(RECENT_DAYS);
    // `.pop()` takes the one row out of the `Vec`; `ok_or` turns "none"
    // into our 404.
    let summary = admin::users(conn, Some(id), since)
        .await?
        .pop()
        .ok_or(AppError::NotFound)?;
    let personal = stored::personal(conn, id).await?;
    let chats = admin::chats_of(conn, id).await?;
    let defaults = &state.apis.prayer_times.defaults;
    let today = today_for(conn, defaults, id, now).await?;
    let prayers = prayer_view(conn, id, today).await?;
    let dm = existing_dm(conn, id).await?;
    let reports = reports_view(conn, dm).await?;

    Ok(UserDetail {
        recent_days: RECENT_DAYS,
        cleanup: CleanupView::of(state),
        user: user_row(summary, state, now),
        settings: settings_view(personal, defaults),
        reports,
        prayers,
        chats: chats
            .into_iter()
            .map(|c| chat_view(c, state, now))
            .collect(),
    })
}

/// `GET /api/chats/{id}/todos`: a group's todos. 404 for a DM.
pub async fn group_todos(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<GroupTodos>, AppError> {
    let mut conn = state.db.acquire().await?;
    let group = group_view(&mut conn, &state, id).await?;
    let todos = todos::list_all(&mut conn, group.id).await?;
    Ok(Json(GroupTodos {
        group,
        todos: todos
            .into_iter()
            .map(|t| TodoView {
                // Before `title` and `assignee` are moved out of `t`: a
                // method call needs the whole `t`.
                skipped: t.is_skipped(),
                id: t.id,
                title: t.title,
                assignee: t.assignee,
                // `as_ref` borrows inside the `Option`, so `t.closed` can be
                // read twice without being moved out.
                done_by: t.closed.as_ref().map(|d| d.by.clone()),
                done_at: t.closed.as_ref().map(|d| d.at),
            })
            .collect(),
    }))
}

/// `GET /api/chats/{id}/shop`: a group's shopping list. 404 for a DM.
pub async fn group_shop(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<GroupShop>, AppError> {
    let mut conn = state.db.acquire().await?;
    let group = group_view(&mut conn, &state, id).await?;
    let items = shop::list_all(&mut conn, group.id).await?;
    Ok(Json(GroupShop {
        group,
        items: items
            .into_iter()
            .map(|i| ItemView {
                id: i.id,
                name: i.name,
                bought: i.bought,
            })
            .collect(),
    }))
}

// ---- helpers ----

/// The group with this id and its timezone, or `NotFound` if it doesn't
/// exist or is a DM.
async fn group_view(
    conn: &mut SqliteConnection,
    state: &AppState,
    id: i64,
) -> Result<GroupView, AppError> {
    let group = admin::group(conn, id).await?.ok_or(AppError::NotFound)?;
    let defaults = &state.apis.prayer_times.defaults;
    let settings = stored::effective(conn, defaults, group.id, None).await?;
    Ok(GroupView {
        id: group.id,
        name: group.name,
        timezone: settings.timezone.name().to_string(),
    })
}

/// When the bot will leave a quiet group, by the rules the cleanup
/// follows: `None` if it won't (clearing is off) or it's already gone.
pub(crate) fn leaves_on(
    last_active: DateTime<Utc>,
    warned_at: Option<DateTime<Utc>>,
    left_at: Option<DateTime<Utc>>,
    state: &AppState,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if left_at.is_some() {
        return None;
    }
    let activity = cleanup::Activity {
        last_active,
        warned_at,
        cleared_at: None,
        can_warn: true,
    };
    cleanup::clears_on(&activity, state.config.inactive_days, now)
}

/// Saves the groups' names from GOWA, so the dashboard can show them.
/// Best effort: if GOWA can't be reached, the names stay as they were.
pub(crate) async fn refresh_group_names(state: &AppState) {
    let groups = match state.gowa.groups().await {
        Ok(groups) => groups,
        Err(err) => {
            warn!("couldn't get group names from GOWA: {err}");
            return;
        }
    };
    let Ok(mut conn) = state.db.acquire().await else {
        return;
    };
    for group in groups {
        if let Err(err) = chats::set_group_name(&mut conn, &group.jid, &group.name).await {
            warn!("couldn't save a group name: {err}");
        }
    }
}

/// A person's settings as the dashboard shows them: what's in use, and
/// which are their own. `personal` is `None` if they never set anything.
pub(crate) fn settings_view(personal: Option<ChatSettings>, defaults: &Settings) -> SettingsView {
    // `unwrap_or_default` turns "no settings" into all-defaults, so the
    // rest of the code handles one case, not two.
    let personal = personal.unwrap_or_default();
    let own = &personal.overrides;
    let used = own.apply(defaults);

    SettingsView {
        // City and country are only used as a pair (see `Overrides::apply`).
        place: Setting {
            value: format!("{}, {}", used.city, used.country),
            own: own.city.is_some() && own.country.is_some(),
        },
        timezone: Setting {
            value: used.timezone.name().to_string(),
            own: own.timezone.is_some(),
        },
        prayer_method: Setting {
            value: format!(
                "{} ({})",
                prayer::method_name(used.prayer_method).unwrap_or("Unknown method"),
                used.prayer_method
            ),
            own: own.prayer_method.is_some(),
        },
        asr_school: Setting {
            value: prayer::school_name(used.prayer_school).to_string(),
            own: own.prayer_school.is_some(),
        },
        ending_minutes: Setting {
            value: used.prayer_ending_minutes,
            own: own.prayer_ending_minutes.is_some(),
        },
        prayer_reminders: personal.prayer_reminders,
        values: Values {
            timezone: used.timezone.name().to_string(),
            prayer_method: used.prayer_method,
            asr_school: used.prayer_school,
            ending_minutes: used.prayer_ending_minutes,
            // Moved last: `used` gives these two up here.
            city: used.city,
            country: used.country,
        },
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;
    use serde_json::Value;

    use super::*;
    use crate::db::test_pool;
    use crate::geo::Coordinates;
    use crate::settings::Overrides;
    use crate::settings::tests::defaults;
    use crate::testkit::{Family, admin_get_text, seed_family, start_server_with};

    #[test]
    fn settings_show_defaults_then_own_choices() {
        let none = settings_view(None, &defaults());
        assert_eq!(
            none,
            SettingsView {
                place: Setting {
                    value: "Milton Keynes, GB".to_string(),
                    own: false
                },
                timezone: Setting {
                    value: "Europe/London".to_string(),
                    own: false
                },
                prayer_method: Setting {
                    value: "Muslim World League (3)".to_string(),
                    own: false
                },
                asr_school: Setting {
                    value: "Shafi'i".to_string(),
                    own: false
                },
                ending_minutes: Setting {
                    value: 20,
                    own: false
                },
                prayer_reminders: false,
                values: Values {
                    city: "Milton Keynes".to_string(),
                    country: "GB".to_string(),
                    timezone: "Europe/London".to_string(),
                    prayer_method: 3,
                    asr_school: prayer::SHAFII,
                    ending_minutes: 20,
                },
            }
        );

        let crawley = ChatSettings {
            prayer_reminders: true,
            overrides: Overrides {
                city: Some("Crawley".to_string()),
                country: Some("GB".to_string()),
                latitude: Some(51.1195),
                longitude: Some(-0.1855),
                prayer_school: Some(prayer::HANAFI as i64),
                ..Overrides::default()
            },
        };
        let view = settings_view(Some(crawley), &defaults());
        assert_eq!(
            view.place,
            Setting {
                value: "Crawley, GB".to_string(),
                own: true
            }
        );
        assert_eq!(
            view.asr_school,
            Setting {
                value: "Hanafi".to_string(),
                own: true
            }
        );
        assert!(!view.timezone.own);
        assert!(view.prayer_reminders);
    }

    /// Bilal (with a DM, its private lists and a location) and Khadija, in a
    /// group with its own lists. Returns (Bilal, the group, Bilal's DM).
    async fn seed(pool: &sqlx::SqlitePool) -> (i64, i64, i64) {
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        // Destructuring: takes the `Family` apart into four local variables
        // named after its fields.
        let Family {
            bilal,
            khadija,
            group,
            dm,
        } = seed_family(&mut conn, now).await;

        let tap = todos::add(
            &mut conn,
            group,
            "fix the tap",
            bilal.id,
            Some(khadija.id),
            Default::default(),
            now,
        )
        .await
        .unwrap();
        todos::add(
            &mut conn,
            group,
            "bins",
            bilal.id,
            Some(bilal.id),
            Default::default(),
            now,
        )
        .await
        .unwrap();
        todos::complete(&mut conn, tap, group, khadija.id, now)
            .await
            .unwrap();
        shop::add(&mut conn, group, "milk", bilal.id, now)
            .await
            .unwrap();

        // Bilal's private things, which must never be sent.
        todos::add(
            &mut conn,
            dm,
            "SECRET dentist",
            bilal.id,
            Some(bilal.id),
            Default::default(),
            now,
        )
        .await
        .unwrap();
        shop::add(&mut conn, dm, "SECRET gift", bilal.id, now)
            .await
            .unwrap();
        let home = Coordinates {
            latitude: 51.123456,
            longitude: -0.654321,
        };
        stored::set_location(&mut conn, dm, "Crawley", "GB", home, "Europe/London")
            .await
            .unwrap();
        (bilal.id, group, dm)
    }

    #[tokio::test]
    async fn users_list_and_detail() {
        let pool = test_pool().await;
        let (bilal, group, dm) = seed(&pool).await;
        let server = start_server_with(pool).await;

        let (status, body) = admin_get_text(&server, "/api/users").await;
        assert_eq!(status, StatusCode::OK);
        let list: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(list["recent_days"], 30);
        let names: Vec<&str> = list["users"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Bilal", "Khadija"]);

        let (status, body) = admin_get_text(&server, &format!("/api/users/{bilal}")).await;
        assert_eq!(status, StatusCode::OK);
        let detail: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(detail["user"]["name"], "Bilal");
        assert_eq!(detail["user"]["has_dm"], true);
        assert_eq!(detail["settings"]["place"]["value"], "Crawley, GB");
        assert_eq!(detail["settings"]["place"]["own"], true);
        let chats = detail["chats"].as_array().unwrap();
        assert_eq!(chats.len(), 2);
        assert_eq!(chats[0]["id"], dm);
        assert_eq!(chats[0]["kind"], "dm");
        assert_eq!(chats[0]["open_todos"], 1);
        assert_eq!(chats[0]["to_buy"], 1);
        assert_eq!(chats[1]["id"], group);
        assert_eq!(chats[1]["name"], "Family");
        assert_eq!(chats[1]["done_todos"], 1);

        let (status, body) = admin_get_text(&server, "/api/users/999").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, r#"{"error":"not found"}"#);
    }

    #[tokio::test]
    async fn group_lists_match_the_bot() {
        let pool = test_pool().await;
        let (_, group, _) = seed(&pool).await;
        let server = start_server_with(pool).await;

        let (status, body) = admin_get_text(&server, &format!("/api/chats/{group}/todos")).await;
        assert_eq!(status, StatusCode::OK);
        let todos: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(todos["group"]["name"], "Family");
        assert_eq!(todos["group"]["timezone"], "Europe/London");
        // `/todo all` order: open first, then done.
        let list = todos["todos"].as_array().unwrap();
        assert_eq!(list[0]["title"], "bins");
        assert_eq!(list[0]["done_by"], Value::Null);
        assert_eq!(list[1]["title"], "fix the tap");
        assert_eq!(list[1]["assignee"], "Khadija");
        assert_eq!(list[1]["done_by"], "Khadija");
        assert_eq!(list[1]["skipped"], false);
        assert!(list[1]["done_at"].is_string());

        let (status, body) = admin_get_text(&server, &format!("/api/chats/{group}/shop")).await;
        assert_eq!(status, StatusCode::OK);
        let shop: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(shop["items"][0]["name"], "milk");
        assert_eq!(shop["items"][0]["bought"], false);
    }

    #[tokio::test]
    async fn dm_lists_and_coordinates_are_never_sent() {
        let pool = test_pool().await;
        let (bilal, _, dm) = seed(&pool).await;
        let server = start_server_with(pool).await;

        for list in ["todos", "shop"] {
            let (status, body) = admin_get_text(&server, &format!("/api/chats/{dm}/{list}")).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{list}");
            assert_eq!(body, r#"{"error":"not found"}"#);
        }
        let (status, _) = admin_get_text(&server, "/api/chats/999/todos").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Every response the dashboard can get, read as plain text.
        for path in ["/api/users".to_string(), format!("/api/users/{bilal}")] {
            let (_, body) = admin_get_text(&server, &path).await;
            assert!(!body.contains("SECRET"), "{path}: {body}");
            assert!(!body.contains("51.12"), "{path}: {body}");
            assert!(!body.contains("0.65"), "{path}: {body}");
            assert!(!body.contains("latitude"), "{path}: {body}");
        }
    }

    /// The same two rules, for every GET route the API has. Anything
    /// private typed in Bilal's DM has "secret" in it, so one search finds
    /// it anywhere in any answer.
    ///
    /// Adding a GET route to `routes::api::router`? Add it to `paths` here
    /// too, so it gets checked.
    #[tokio::test]
    async fn no_get_route_leaks_dm_contents_or_coordinates() {
        let pool = test_pool().await;
        let (bilal, group, dm) = seed(&pool).await;
        {
            let mut conn = pool.acquire().await.unwrap();
            // The group's own town, with exact coordinates of its own.
            let town = Coordinates {
                latitude: 52.345678,
                longitude: -0.987654,
            };
            stored::set_location(&mut conn, group, "Bedford", "GB", town, "Europe/London")
                .await
                .unwrap();
            // A `+word` the bot didn't understand, typed in the DM. Its
            // first word is all the log keeps, but that word is private too.
            let unknown = crate::metrics::unknown("+secretdmword friday").unwrap();
            crate::repo::metrics::record(
                &mut conn,
                &crate::repo::metrics::Logged {
                    event: &unknown,
                    chat_id: dm,
                    user_id: bilal,
                    is_group: false,
                    ok: false,
                    duration_ms: 1,
                    at: Utc::now(),
                },
            )
            .await
            .unwrap();
        }
        let server = start_server_with(pool).await;

        let paths = [
            "/api/health".to_string(),
            "/api/users".to_string(),
            format!("/api/users/{bilal}"),
            "/api/groups".to_string(),
            format!("/api/groups/{group}"),
            format!("/api/chats/{group}/todos"),
            format!("/api/chats/{group}/shop"),
            format!("/api/chats/{dm}/todos"),
            format!("/api/chats/{dm}/shop"),
            "/api/metrics".to_string(),
            "/api/metrics?days=7".to_string(),
            format!("/api/metrics?user={bilal}"),
            format!("/api/metrics?chat={dm}"),
            format!("/api/metrics?chat={group}"),
            "/api/prayer-methods".to_string(),
            format!("/api/users/{bilal}/prayers"),
            format!("/api/users/{bilal}/prayers?month=2026-11"),
        ];
        for path in paths {
            let (status, body) = admin_get_text(&server, &path).await;
            // A 404 is fine (the DM's lists), but nothing else may fail,
            // or a broken route would pass by sending nothing.
            assert!(
                status == StatusCode::OK || status == StatusCode::NOT_FOUND,
                "{path}: {status}"
            );
            let text = body.to_lowercase();
            assert!(!text.contains("secret"), "{path}: {body}");
            for exact in ["51.12", "0.65", "52.34", "0.98", "latitude", "longitude"] {
                assert!(!text.contains(exact), "{path} has {exact}: {body}");
            }
        }
    }

    #[tokio::test]
    async fn not_a_number_is_a_400() {
        let server = start_server_with(test_pool().await).await;
        let (status, _) = admin_get_text(&server, "/api/users/abc").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}

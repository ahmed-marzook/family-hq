//! The admin dashboard's group pages: every group the bot is in, and one
//! group's settings, members and how much it's used. A group's settings are
//! shared by everyone in it (members' own settings, from their DMs, still
//! win for their prayer times), and are changed by the same rules as
//! `/settings` in the group.
//!
//! Only groups: every endpoint here answers 404 for a DM, so nothing about
//! a private chat can be read or changed through them.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sqlx::SqliteConnection;
use tracing::info;

use crate::AppState;
use crate::error::AppError;
use crate::repo::admin;
use crate::repo::settings as stored;
use crate::reports::Span;
use crate::routes::admin::{
    CleanupView, RECENT_DAYS, SettingsView, leaves_on, refresh_group_names, settings_view,
};
use crate::routes::admin_prayers::{SpanView, span_view, today_for};
use crate::routes::admin_reports::{ReportsView, reports_view};
use crate::routes::admin_settings::{SettingsPatch, Switch, check_patch, on_off, save_patch};

#[derive(Debug, Serialize)]
pub struct GroupList {
    /// The period `recent_uses` covers.
    recent_days: i64,
    cleanup: CleanupView,
    groups: Vec<GroupRow>,
}

#[derive(Debug, Serialize)]
struct GroupRow {
    id: i64,
    name: Option<String>,
    members: i64,
    open_todos: i64,
    done_todos: i64,
    to_buy: i64,
    bought: i64,
    recent_uses: i64,
    /// When someone last used the bot in the group.
    last_active: DateTime<Utc>,
    /// When the bot posted that it would leave.
    warned_at: Option<DateTime<Utc>>,
    /// The bot isn't in it any more (seen then); it's deleted a day after.
    left_at: Option<DateTime<Utc>>,
    /// When the bot will leave and delete it if nobody uses it before then;
    /// `null` if it won't (clearing is off, or it's already gone).
    leaves_on: Option<DateTime<Utc>>,
}

fn group_row(g: admin::GroupSummary, state: &AppState, now: DateTime<Utc>) -> GroupRow {
    GroupRow {
        leaves_on: leaves_on(g.last_active, g.warned_at, g.left_at, state, now),
        id: g.id,
        name: g.name,
        members: g.members,
        open_todos: g.open_todos,
        done_todos: g.done_todos,
        to_buy: g.to_buy,
        bought: g.bought,
        recent_uses: g.uses_since,
        last_active: g.last_active,
        warned_at: g.warned_at,
        left_at: g.left_at,
    }
}

#[derive(Debug, Serialize)]
pub struct GroupDetail {
    recent_days: i64,
    cleanup: CleanupView,
    group: GroupRow,
    /// The group's own settings, defaults filled in.
    settings: SettingsView,
    /// Where each feature's reports go: "here", "dm" or "off".
    reports: ReportsView,
    members: Vec<MemberView>,
}

/// Someone in the group: a name and a link to their page. Never their
/// number.
#[derive(Debug, Serialize)]
struct MemberView {
    id: i64,
    name: String,
    /// Their prayers this week so far, for the group's heatmap. The same
    /// as the group's weekly report shows.
    prayer_week: SpanView,
}

// ---- handlers ----

/// `GET /api/groups`: every group, most recently active first.
pub async fn list_groups(State(state): State<Arc<AppState>>) -> Result<Json<GroupList>, AppError> {
    refresh_group_names(&state).await;
    let mut conn = state.db.acquire().await?;
    let now = Utc::now();
    let groups = admin::groups(&mut conn, None, now - Duration::days(RECENT_DAYS)).await?;
    Ok(Json(GroupList {
        recent_days: RECENT_DAYS,
        cleanup: CleanupView::of(&state),
        groups: groups
            .into_iter()
            .map(|g| group_row(g, &state, now))
            .collect(),
    }))
}

/// `GET /api/groups/{id}`: one group. 404 for a DM.
pub async fn group(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<GroupDetail>, AppError> {
    let mut conn = state.db.acquire().await?;
    Ok(Json(detail(&mut conn, &state, id).await?))
}

/// `PATCH /api/groups/{id}/settings`: change the group's settings, all or
/// nothing, as `PATCH /api/users/{id}/settings` does for a person.
pub async fn change_settings(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(patch): Json<SettingsPatch>,
) -> Result<Json<GroupDetail>, AppError> {
    // Checked (and a new town looked up) before the transaction, as for a
    // person's settings (`admin_settings::change_settings`).
    let (changes, checked) = {
        let mut conn = state.db.acquire().await?;
        ensure_group(&mut conn, id).await?;
        check_patch(&mut conn, &state, Some(id), patch).await?
    };
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    ensure_group(&mut tx, id).await?;
    save_patch(&mut tx, &state, id, checked).await?;
    tx.commit().await?;
    info!("changed group {id}'s settings: {changes:?}");

    let mut conn = state.db.acquire().await?;
    Ok(Json(detail(&mut conn, &state, id).await?))
}

/// `POST /api/groups/{id}/settings/reset`: back to the server defaults.
/// Prayer reminders stay on or off, as with `/settings reset`.
pub async fn reset_settings(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<GroupDetail>, AppError> {
    let mut conn = state.db.acquire().await?;
    ensure_group(&mut conn, id).await?;
    stored::reset(&mut conn, id).await?;
    info!("reset group {id}'s settings");
    Ok(Json(detail(&mut conn, &state, id).await?))
}

/// `POST /api/groups/{id}/prayer-reminders` with `{"on": true}`: the
/// group's prayer reminders (`/prayers on` in the group).
pub async fn prayer_reminders(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(switch): Json<Switch>,
) -> Result<Json<GroupDetail>, AppError> {
    let mut conn = state.db.acquire().await?;
    ensure_group(&mut conn, id).await?;
    stored::set_prayer_reminders(&mut conn, id, switch.on).await?;
    info!("group {id}'s prayer reminders: {}", on_off(switch.on));
    Ok(Json(detail(&mut conn, &state, id).await?))
}

// ---- helpers ----

/// 404 unless `id` is a group.
pub(crate) async fn ensure_group(conn: &mut SqliteConnection, id: i64) -> Result<(), AppError> {
    admin::group(conn, id).await?.ok_or(AppError::NotFound)?;
    Ok(())
}

/// Everything the group page shows. Also what the edit endpoints answer
/// with, so the page shows the result straight away.
pub(crate) async fn detail(
    conn: &mut SqliteConnection,
    state: &AppState,
    id: i64,
) -> Result<GroupDetail, AppError> {
    let now = Utc::now();
    let summary = admin::groups(conn, Some(id), now - Duration::days(RECENT_DAYS))
        .await?
        .pop()
        .ok_or(AppError::NotFound)?;
    let settings = stored::get(conn, id).await?;
    let group_settings = settings.overrides.apply(&state.apis.prayer_times.defaults);
    let reports = reports_view(conn, Some(id)).await?;
    // A `for` loop rather than `map`: each member needs two `.await`s,
    // and a closure can't `.await` inside a plain `map`.
    let mut members = Vec::new();
    for m in admin::members_of(conn, id).await? {
        let today = today_for(conn, m.id, &group_settings).await?;
        members.push(MemberView {
            id: m.id,
            name: m.name,
            prayer_week: span_view(conn, m.id, Span::Week, today).await?,
        });
    }
    Ok(GroupDetail {
        recent_days: RECENT_DAYS,
        cleanup: CleanupView::of(state),
        group: group_row(summary, state, now),
        settings: settings_view(Some(settings), &state.apis.prayer_times.defaults),
        reports,
        members,
    })
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;
    use serde_json::{Value, json};

    use crate::db::test_pool;
    use crate::geo::Coordinates;
    use crate::repo::settings as stored;
    use crate::testkit::{admin_get, fake_geocoder, seed_family, start_server_with_geocoder};

    async fn send(
        method: reqwest::Method,
        url: String,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut request = reqwest::Client::new()
            .request(method, url)
            .basic_auth("test", Some("test"));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    /// Bilal (with a DM and a home location) and Khadija in "Family".
    /// Returns (the group, Bilal's DM).
    async fn seed(pool: &sqlx::SqlitePool) -> (i64, i64) {
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, chrono::Utc::now()).await;
        let (group, dm) = (family.group, family.dm);
        let home = Coordinates {
            latitude: 51.123456,
            longitude: -0.654321,
        };
        stored::set_location(&mut conn, dm, "Crawley", "GB", home, "Europe/London")
            .await
            .unwrap();
        (group, dm)
    }

    #[tokio::test]
    async fn groups_list_and_detail() {
        let pool = test_pool().await;
        let (group, dm) = seed(&pool).await;
        let server = start_server_with_geocoder(pool, &fake_geocoder().await).await;

        let (status, list) = admin_get(&server, "/api/groups").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["recent_days"], 30);
        let groups = list["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "a DM isn't a group: {list}");
        assert_eq!(groups[0]["name"], "Family");
        assert_eq!(groups[0]["members"], 2);
        assert!(
            groups[0]["leaves_on"].is_string(),
            "clearing is on in tests"
        );

        let (status, page) = admin_get(&server, &format!("/api/groups/{group}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["group"]["id"], group);
        assert_eq!(page["settings"]["place"]["own"], false);
        let names: Vec<&str> = page["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Bilal", "Khadija"]);
        // Members' numbers and Bilal's home never leave the server.
        let text = page.to_string();
        assert!(!text.contains("447700"), "{text}");
        assert!(!text.contains("51.12"), "{text}");

        for path in [format!("/api/groups/{dm}"), "/api/groups/999".to_string()] {
            let (status, _) = admin_get(&server, &path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn group_settings_are_changed_and_reset() {
        let pool = test_pool().await;
        let (group, dm) = seed(&pool).await;
        let server = start_server_with_geocoder(pool.clone(), &fake_geocoder().await).await;

        let (status, page) = send(
            reqwest::Method::PATCH,
            format!("{server}/api/groups/{group}/settings"),
            Some(json!({ "city": "leicester", "country": "gb", "asr_school": 1 })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert_eq!(page["settings"]["place"]["value"], "Leicester, GB");
        assert_eq!(page["settings"]["asr_school"]["value"], "Hanafi");

        // A refused change saves nothing, not even the good one before it.
        let (status, body) = send(
            reqwest::Method::PATCH,
            format!("{server}/api/groups/{group}/settings"),
            Some(json!({ "prayer_method": 2, "ending_minutes": 999 })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"].as_str().unwrap().contains("minutes"),
            "{body}"
        );

        let (status, page) = send(
            reqwest::Method::POST,
            format!("{server}/api/groups/{group}/prayer-reminders"),
            Some(json!({ "on": true })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["settings"]["prayer_reminders"], true);

        let (status, page) = send(
            reqwest::Method::POST,
            format!("{server}/api/groups/{group}/settings/reset"),
            Some(json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["settings"]["place"]["own"], false);
        assert_eq!(
            page["settings"]["prayer_reminders"], true,
            "reset keeps reminders"
        );

        // A DM can't be changed through the group endpoints.
        let (status, _) = send(
            reqwest::Method::PATCH,
            format!("{server}/api/groups/{dm}/settings"),
            Some(json!({ "asr_school": 1 })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = send(
            reqwest::Method::POST,
            format!("{server}/api/groups/{dm}/settings/reset"),
            Some(json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Checked last: the test pool's one connection is held from here.
        let mut conn = pool.acquire().await.unwrap();
        let saved = stored::get(&mut conn, group).await.unwrap();
        assert_eq!(saved.overrides.prayer_method, None);
        assert!(saved.prayer_reminders);
        let own = stored::get(&mut conn, dm).await.unwrap();
        assert_eq!(own.overrides.city.as_deref(), Some("Crawley"));
        assert_eq!(own.overrides.prayer_school, None);
    }
}

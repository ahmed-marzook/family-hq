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

use crate::AppState;
use crate::error::AppError;
use crate::prayer;
use crate::repo::settings::{self as stored, ChatSettings};
use crate::repo::{admin, chats, shop, todos};
use crate::settings::Settings;

/// How far back the users page counts uses.
const RECENT_DAYS: i64 = 30;

// ---- JSON shapes ----
//
// `#[derive(Serialize)]` writes the code that turns a struct into JSON,
// one key per field. `DateTime<Utc>` becomes an RFC 3339 string
// ("2026-11-02T09:30:00Z"), and `Option` becomes the value or `null`.

#[derive(Debug, Serialize)]
pub struct UserList {
    /// The period `recent_uses` covers.
    recent_days: i64,
    users: Vec<UserRow>,
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
        }
    }
}

#[derive(Debug, Serialize)]
pub struct UserDetail {
    recent_days: i64,
    user: UserRow,
    settings: SettingsView,
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
struct SettingsView {
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
}

impl From<admin::UserChat> for ChatView {
    fn from(c: admin::UserChat) -> Self {
        ChatView {
            id: c.id,
            kind: c.kind,
            name: c.name,
            open_todos: c.open_todos,
            done_todos: c.done_todos,
            to_buy: c.to_buy,
            bought: c.bought,
            todo_reminders: c.todo_reminders,
        }
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
    done_by: Option<String>,
    done_at: Option<DateTime<Utc>>,
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
    let since = Utc::now() - Duration::days(RECENT_DAYS);
    let users = admin::users(&mut conn, None, since).await?;
    Ok(Json(UserList {
        recent_days: RECENT_DAYS,
        // `map(UserRow::from)` converts each one; `collect` gathers them
        // back into a `Vec`.
        users: users.into_iter().map(UserRow::from).collect(),
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
    let since = Utc::now() - Duration::days(RECENT_DAYS);
    // `.pop()` takes the one row out of the `Vec`; `ok_or` turns "none"
    // into our 404.
    let summary = admin::users(&mut conn, Some(id), since)
        .await?
        .pop()
        .ok_or(AppError::NotFound)?;
    let personal = stored::personal(&mut conn, id).await?;
    let chats = admin::chats_of(&mut conn, id).await?;

    Ok(Json(UserDetail {
        recent_days: RECENT_DAYS,
        user: summary.into(),
        settings: settings_view(personal, &state.apis.prayer_times.defaults),
        chats: chats.into_iter().map(ChatView::from).collect(),
    }))
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
                id: t.id,
                title: t.title,
                assignee: t.assignee,
                // `as_ref` borrows inside the `Option`, so `t.done` can be
                // read twice without being moved out.
                done_by: t.done.as_ref().map(|d| d.by.clone()),
                done_at: t.done.as_ref().map(|d| d.at),
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
    let stored = stored::get(conn, group.id).await?;
    let settings = stored.overrides.apply(&state.apis.prayer_times.defaults);
    Ok(GroupView {
        id: group.id,
        name: group.name,
        timezone: settings.timezone.name().to_string(),
    })
}

/// Saves the groups' names from GOWA, so the dashboard can show them.
/// Best effort: if GOWA can't be reached, the names stay as they were.
async fn refresh_group_names(state: &AppState) {
    let groups = match state.gowa.groups().await {
        Ok(groups) => groups,
        Err(err) => {
            println!("admin: couldn't get group names from GOWA: {err}");
            return;
        }
    };
    let Ok(mut conn) = state.db.acquire().await else {
        return;
    };
    for group in groups {
        if let Err(err) = chats::set_group_name(&mut conn, &group.jid, &group.name).await {
            println!("admin: couldn't save a group name: {err}");
        }
    }
}

/// A person's settings as the dashboard shows them: what's in use, and
/// which are their own. `personal` is `None` if they never set anything.
fn settings_view(personal: Option<ChatSettings>, defaults: &Settings) -> SettingsView {
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
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;
    use serde_json::Value;

    use super::*;
    use crate::db::test_pool;
    use crate::geo::Coordinates;
    use crate::person::Person;
    use crate::routes::api::start_server_with;
    use crate::settings::Overrides;
    use crate::settings::tests::defaults;

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

    /// The login `start_server_with` gives the server.
    const LOGIN: (&str, &str) = ("test", "test");

    async fn get(url: String) -> (StatusCode, String) {
        let response = reqwest::Client::new()
            .get(url)
            .basic_auth(LOGIN.0, Some(LOGIN.1))
            .send()
            .await
            .unwrap();
        (response.status(), response.text().await.unwrap())
    }

    /// Bilal (with a DM, its private lists and a location) and Khadija, in a
    /// group with its own lists. Returns (Bilal, the group, Bilal's DM).
    async fn seed(pool: &sqlx::SqlitePool) -> (i64, i64, i64) {
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let mut add_person = async |name: &str, phone: &str| {
            let person = Person {
                name: name.to_string(),
                jid: format!("{phone}@s.whatsapp.net"),
                lid: None,
            };
            crate::repo::users::find_or_register(&mut conn, &person, now)
                .await
                .unwrap()
                .unwrap()
        };
        let bilal = add_person("Bilal", "447700900001").await;
        let khadija = add_person("Khadija", "447700900002").await;

        let group = chats::find_or_create(&mut conn, "120363000000000001@g.us", None, now)
            .await
            .unwrap();
        chats::set_group_name(&mut conn, "120363000000000001@g.us", "Family")
            .await
            .unwrap();
        for user in [bilal.id, khadija.id] {
            chats::add_member(&mut conn, group, user, now)
                .await
                .unwrap();
        }
        let dm = chats::find_or_create(&mut conn, &bilal.wa_jid, Some(bilal.id), now)
            .await
            .unwrap();

        let tap = todos::add(
            &mut conn,
            group,
            "fix the tap",
            bilal.id,
            Some(khadija.id),
            now,
        )
        .await
        .unwrap();
        todos::add(&mut conn, group, "bins", bilal.id, Some(bilal.id), now)
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

        let (status, body) = get(format!("{server}/api/users")).await;
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

        let (status, body) = get(format!("{server}/api/users/{bilal}")).await;
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

        let (status, body) = get(format!("{server}/api/users/999")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, r#"{"error":"not found"}"#);
    }

    #[tokio::test]
    async fn group_lists_match_the_bot() {
        let pool = test_pool().await;
        let (_, group, _) = seed(&pool).await;
        let server = start_server_with(pool).await;

        let (status, body) = get(format!("{server}/api/chats/{group}/todos")).await;
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
        assert!(list[1]["done_at"].is_string());

        let (status, body) = get(format!("{server}/api/chats/{group}/shop")).await;
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
            let (status, body) = get(format!("{server}/api/chats/{dm}/{list}")).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{list}");
            assert_eq!(body, r#"{"error":"not found"}"#);
        }
        let (status, _) = get(format!("{server}/api/chats/999/todos")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Every response the dashboard can get, read as plain text.
        for path in ["/api/users".to_string(), format!("/api/users/{bilal}")] {
            let (_, body) = get(format!("{server}{path}")).await;
            assert!(!body.contains("SECRET"), "{path}: {body}");
            assert!(!body.contains("51.12"), "{path}: {body}");
            assert!(!body.contains("0.65"), "{path}: {body}");
            assert!(!body.contains("latitude"), "{path}: {body}");
        }
    }

    #[tokio::test]
    async fn not_a_number_is_a_400() {
        let server = start_server_with(test_pool().await).await;
        let (status, _) = get(format!("{server}/api/users/abc")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}

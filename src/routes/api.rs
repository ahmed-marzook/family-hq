//! The admin dashboard's JSON API, everything under `/api`. The dashboard is
//! its own container (`web/`), which passes `/api` on to this server.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::AppState;
use crate::routes::{
    admin, admin_auth, admin_cleanup, admin_groups, admin_metrics, admin_prayers, admin_reports,
    admin_settings, health,
};

/// The routes under `/api`, all behind the admin password.
///
/// A `Router<Arc<AppState>>` is one still waiting for its state: `app` hands
/// it over once, with `with_state`, after nesting this inside the main router.
/// The password check needs the state sooner (to read the login), so it
/// gets its own clone of the `Arc`.
pub fn router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    let routes = Router::new()
        .route("/health", get(health::health))
        .route("/users", get(admin::list_users))
        .route("/users/{id}", get(admin::user))
        .route("/groups", get(admin_groups::list_groups))
        .route("/groups/{id}", get(admin_groups::group))
        .route(
            "/groups/{id}/settings",
            patch(admin_groups::change_settings),
        )
        .route(
            "/groups/{id}/settings/reset",
            post(admin_groups::reset_settings),
        )
        .route(
            "/groups/{id}/prayer-reminders",
            post(admin_groups::prayer_reminders),
        )
        .route(
            "/groups/{id}/reports/{feature}",
            post(admin_reports::group_reports),
        )
        .route("/chats/{id}/todos", get(admin::group_todos))
        .route("/chats/{id}/shop", get(admin::group_shop))
        .route("/metrics", get(admin_metrics::metrics))
        .route("/prayer-methods", get(admin_settings::prayer_methods))
        .route(
            "/users/{id}/settings",
            patch(admin_settings::change_settings),
        )
        .route(
            "/users/{id}/settings/reset",
            post(admin_settings::reset_settings),
        )
        .route(
            "/users/{id}/prayer-reminders",
            post(admin_settings::prayer_reminders),
        )
        .route(
            "/users/{id}/reports/{feature}",
            post(admin_reports::user_reports),
        )
        .route("/users/{id}/prayers", get(admin_prayers::user_month))
        .route(
            "/users/{id}/prayers/{date}/{prayer}",
            put(admin_prayers::set_prayer),
        )
        .route(
            "/users/{id}/chats/{chat}/todo-reminders",
            post(admin_settings::todo_reminders),
        )
        .route("/users/{id}/clear", post(admin_cleanup::clear))
        .route("/chats/{id}/leave", post(admin_cleanup::leave))
        .fallback(not_found);
    admin_auth::protect(routes, state)
}

/// Any other path under `/api`: a 404 the dashboard can read as JSON.
///
/// A handler can return a tuple: Axum uses the `StatusCode` for the status
/// and the rest for the body.
async fn not_found() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" })))
}

/// Runs the real router, on a free port with `pool` as its database, and
/// returns its address. For tests of anything under `/api`. The login is
/// `test:test`, and GOWA is a fake (`fake_gowa`) that says yes to everything.
#[cfg(test)]
pub(crate) async fn start_server_with(pool: sqlx::SqlitePool) -> String {
    serve_for_test(pool, None).await
}

/// The same, with town lookups going to a fake geocoder at `geocoding_url`.
#[cfg(test)]
pub(crate) async fn start_server_with_geocoder(
    pool: sqlx::SqlitePool,
    geocoding_url: &str,
) -> String {
    serve_for_test(pool, Some(geocoding_url)).await
}

/// The app's state for tests: `pool` as its database, GOWA at `gowa_url`
/// (usually a `fake_gowa`'s `url`), login `test:test`, webhook secret
/// `test`, Milton Keynes defaults, clearing after 14 days.
#[cfg(test)]
pub(crate) fn test_state(pool: sqlx::SqlitePool, gowa_url: &str) -> Arc<AppState> {
    use crate::config::Config;
    use crate::settings::Settings;

    let config = Config {
        port: 0,
        webhook_secret: "test".to_string(),
        gowa_url: gowa_url.to_string(),
        gowa_basic_auth: "test:test".to_string(),
        database_url: "sqlite::memory:".to_string(),
        inactive_days: 14,
        backup_dir: None,
        backup_keep: 7,
        push: None,
        defaults: Settings {
            timezone: chrono_tz::Europe::London,
            city: "Milton Keynes".to_string(),
            country: "GB".to_string(),
            coordinates: None,
            prayer_method: 3,
            prayer_school: 0,
            prayer_ending_minutes: 20,
        },
    };
    AppState::new(config, pool)
}

/// Serves the real app for `state` on a free port; returns its address.
#[cfg(test)]
pub(crate) async fn serve_state(state: Arc<AppState>) -> String {
    let app = crate::app(state);
    // Port 0 asks the operating system for any free port.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

#[cfg(test)]
async fn serve_for_test(pool: sqlx::SqlitePool, geocoding_url: Option<&str>) -> String {
    // A fake GOWA that answers at once. An address nothing listens on (like
    // `http://localhost:1`) works too, but Windows takes about 2 seconds to
    // give up on each connection, which made the tests slow. Nobody looks at
    // what this one records, so its handle is dropped; the server itself
    // keeps running, because `tokio::spawn` owns it.
    let gowa = fake_gowa().await;
    let mut state = test_state(pool, &gowa.url);
    if let Some(url) = geocoding_url {
        // `Arc::get_mut` gives a `&mut` only while nothing else shares the
        // `Arc`, which is still true here, before the server starts.
        Arc::get_mut(&mut state).expect("not shared yet").apis.geo =
            crate::geo::Geo::with_geocoding_url(url);
    }
    serve_state(state).await
}

/// A fake GOWA for tests, on a free port: remembers each message the bot
/// sends as (to, text) and each group it leaves, says yes to reactions
/// (without keeping them; no test needs them yet); lists the groups in `listed`; and refuses to leave a
/// group (with a 500, as a broken GOWA would) while `refuse_leave` is set.
///
/// Each field is an `Arc<Mutex<...>>`: the `Arc` lets the test and the
/// fake's handlers share one value, and the `Mutex` lets either change it.
/// A plain `std::sync::Mutex` is fine here, because nobody holds the lock
/// across an `.await`.
#[cfg(test)]
pub(crate) struct FakeGowa {
    pub(crate) url: String,
    pub(crate) sent: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    pub(crate) listed: Arc<std::sync::Mutex<Vec<String>>>,
    pub(crate) left: Arc<std::sync::Mutex<Vec<String>>>,
    pub(crate) refuse_leave: Arc<std::sync::Mutex<bool>>,
}

/// Starts a `FakeGowa`. It answers the paths `crate::whatsapp` calls, with
/// the same reply shapes as the real GOWA.
#[cfg(test)]
pub(crate) async fn fake_gowa() -> FakeGowa {
    use std::sync::Mutex;

    let sent = Arc::new(Mutex::new(Vec::new()));
    let listed = Arc::new(Mutex::new(Vec::<String>::new()));
    let left = Arc::new(Mutex::new(Vec::new()));
    let refuse_leave = Arc::new(Mutex::new(false));
    // Each handler gets its own clone of the `Arc`s it needs: `move`
    // closures take ownership, and the originals go back to the test.
    let (log, groups, leaves, refuse) = (
        sent.clone(),
        listed.clone(),
        left.clone(),
        refuse_leave.clone(),
    );
    let app = Router::new()
        .route(
            "/send/message",
            post(move |Json(body): Json<Value>| async move {
                let to = body["phone"].as_str().unwrap_or_default().to_string();
                let text = body["message"].as_str().unwrap_or_default().to_string();
                log.lock().unwrap().push((to, text));
                Json(json!({ "code": "SUCCESS", "results": { "message_id": "FAKE1" } }))
            }),
        )
        .route(
            "/message/{id}/reaction",
            post(|| async { Json(json!({ "code": "SUCCESS" })) }),
        )
        .route(
            "/user/my/groups",
            get(move || async move {
                let data: Vec<Value> = groups
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|jid| json!({ "JID": jid, "Name": "Family" }))
                    .collect();
                Json(json!({ "code": "SUCCESS", "results": { "data": data } }))
            }),
        )
        .route(
            "/group/leave",
            post(move |Json(body): Json<Value>| async move {
                if *refuse.lock().unwrap() {
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({})));
                }
                let group = body["group_id"].as_str().unwrap_or_default().to_string();
                leaves.lock().unwrap().push(group);
                (StatusCode::OK, Json(json!({ "code": "SUCCESS" })))
            }),
        );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    FakeGowa {
        url: format!("http://{address}"),
        sent,
        listed,
        left,
        refuse_leave,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;

    async fn start_server() -> String {
        start_server_with(test_pool().await).await
    }

    /// The login `start_server` gives the server (its `GOWA_BASIC_AUTH`).
    const USER: &str = "test";
    const PASSWORD: &str = "test";

    async fn get_as(url: &str, password: Option<&str>) -> reqwest::Response {
        let request = reqwest::Client::new().get(url);
        let request = match password {
            Some(password) => request.basic_auth(USER, Some(password)),
            None => request,
        };
        request.send().await.unwrap()
    }

    #[tokio::test]
    async fn health_is_reachable_under_api() {
        let server = start_server().await;
        let response = get_as(&format!("{server}/api/health"), Some(PASSWORD)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["status"], "ok");
        assert_eq!(body["version"], health::VERSION);
    }

    #[tokio::test]
    async fn unknown_api_path_is_a_json_404() {
        let server = start_server().await;
        let response = get_as(&format!("{server}/api/nope"), Some(PASSWORD)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body, json!({ "error": "not found" }));
    }

    #[tokio::test]
    async fn api_needs_the_password() {
        let server = start_server().await;
        // The fallback is behind the password too, so nobody can find out
        // which paths exist without it.
        for path in ["/api/health", "/api/nope"] {
            for password in [None, Some("wrong")] {
                let response = get_as(&format!("{server}{path}"), password).await;
                assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
                let challenge = response.headers()["www-authenticate"].to_str().unwrap();
                assert!(challenge.starts_with("Basic "), "{challenge}");
            }
        }
    }

    #[tokio::test]
    async fn changes_must_be_json() {
        let server = start_server().await;
        let client = reqwest::Client::new();
        let url = format!("{server}/api/health");

        // A form, as another website could make the browser send: refused.
        let form = client
            .post(&url)
            .basic_auth(USER, Some(PASSWORD))
            .header("content-type", "application/x-www-form-urlencoded")
            .body("a=1")
            .send()
            .await
            .unwrap();
        assert_eq!(form.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        // JSON gets through to the router, which has no POST here.
        let json = client
            .post(&url)
            .basic_auth(USER, Some(PASSWORD))
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(json.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn other_routes_need_no_password() {
        let server = start_server().await;
        let response = get_as(&format!("{server}/health"), None).await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::testkit::start_server_with;

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

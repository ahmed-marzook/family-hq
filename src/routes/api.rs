//! The admin dashboard's JSON API, everything under `/api`. The dashboard is
//! its own container (`web/`), which passes `/api` on to this server.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::{Json, Router, routing::get};
use serde_json::{Value, json};

use crate::AppState;
use crate::routes::{admin_auth, health};

/// The routes under `/api`, all behind the admin password.
///
/// A `Router<Arc<AppState>>` is one still waiting for its state: `app` hands
/// it over once, with `with_state`, after nesting this inside the main router.
/// The password check needs the state sooner (to read the login), so it
/// gets its own clone of the `Arc`.
pub fn router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    let routes = Router::new()
        .route("/health", get(health::health))
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
    use crate::config::Config;
    use crate::db::test_pool;
    use crate::settings::Settings;

    /// Runs the real router on a free port, and returns its address.
    async fn start_server() -> String {
        let config = Config {
            port: 0,
            webhook_secret: "test".to_string(),
            gowa_url: "http://localhost:1".to_string(),
            gowa_basic_auth: "test:test".to_string(),
            database_url: "sqlite::memory:".to_string(),
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
        let app = crate::app(AppState::new(config, test_pool().await));
        // Port 0 asks the operating system for any free port.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
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
        assert_eq!(body, json!({ "status": "ok" }));
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

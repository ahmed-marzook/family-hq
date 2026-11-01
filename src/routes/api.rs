//! The admin dashboard's JSON API, everything under `/api`. The dashboard is
//! its own container (`web/`), which passes `/api` on to this server.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::{Json, Router, routing::get};
use serde_json::{Value, json};

use crate::AppState;
use crate::routes::health;

/// The routes under `/api`.
///
/// A `Router<Arc<AppState>>` is one still waiting for its state: `app` hands
/// it over once, with `with_state`, after nesting this inside the main router.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/health", get(health::health))
        .fallback(not_found)
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

    #[tokio::test]
    async fn health_is_reachable_under_api() {
        let server = start_server().await;
        let response = reqwest::get(format!("{server}/api/health")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body, json!({ "status": "ok" }));
    }

    #[tokio::test]
    async fn unknown_api_path_is_a_json_404() {
        let server = start_server().await;
        let response = reqwest::get(format!("{server}/api/nope")).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body, json!({ "error": "not found" }));
    }

    #[tokio::test]
    async fn other_routes_are_untouched() {
        let server = start_server().await;
        let response = reqwest::get(format!("{server}/health")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}

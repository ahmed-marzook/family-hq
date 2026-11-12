use axum::Json;
use serde_json::{Value, json};

/// `GET /` — plain text, to check the server is up from a browser.
pub async fn hello() -> &'static str {
    "Hello, world!"
}

/// The version from Cargo.toml, e.g. "0.18.0".
///
/// Baked in at compile time, so it always matches the release.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `GET /health` — returns `{"status":"ok","version":"0.18.0"}`, so it also
/// shows which release is actually running.
pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": VERSION }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_returns_ok() {
        let Json(body) = health().await;
        assert_eq!(body["status"], "ok");
        // Whatever Cargo.toml says, so this needn't change each release.
        assert_eq!(body["version"], VERSION);
    }

    #[tokio::test]
    async fn hello_returns_greeting() {
        assert_eq!(hello().await, "Hello, world!");
    }
}

use axum::Json;
use serde_json::{Value, json};

/// `GET /` — plain text, to check the server is up from a browser.
///
/// A handler is just an `async fn`. Axum calls it for each request and
/// turns the return value into a response: a `&'static str` becomes a
/// `text/plain` body with status 200.
pub async fn hello() -> &'static str {
    "Hello, world!"
}

/// The version from Cargo.toml, e.g. "0.18.0".
///
/// `env!` reads an environment variable while *compiling*, not at run time,
/// so the number is baked into the program. Cargo sets `CARGO_PKG_VERSION`
/// for every build, which makes this always match the release.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `GET /health` — returns `{"status":"ok","version":"0.18.0"}`, so it also
/// shows which release is actually running.
///
/// Wrapping a value in `Json(...)` makes Axum serialise it and set the
/// `Content-Type: application/json` header. `json!` builds a
/// `serde_json::Value` with JSON-like syntax.
pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": VERSION }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Handlers are ordinary async functions, so tests can call them
    // directly. `#[tokio::test]` gives the test an async runtime.
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

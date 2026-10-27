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

/// `GET /health` — returns `{"status":"ok"}`.
///
/// Wrapping a value in `Json(...)` makes Axum serialise it and set the
/// `Content-Type: application/json` header. `json!` builds a
/// `serde_json::Value` with JSON-like syntax.
pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Handlers are ordinary async functions, so tests can call them
    // directly. `#[tokio::test]` gives the test an async runtime.
    #[tokio::test]
    async fn health_returns_ok() {
        let Json(body) = health().await;
        assert_eq!(body, json!({ "status": "ok" }));
    }

    #[tokio::test]
    async fn hello_returns_greeting() {
        assert_eq!(hello().await, "Hello, world!");
    }
}

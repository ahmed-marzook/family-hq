//! The one error type the app's own functions return.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Everything that can go wrong while handling a request.
///
/// `thiserror` writes the boilerplate: `#[error(...)]` becomes the
/// `Display` text, and `#[from]` adds a `From` conversion, so `?` on a
/// `sqlx::Error` inside a function returning `Result<_, AppError>` turns it
/// into `AppError::Database` automatically.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    /// Any outgoing HTTP call: GOWA or AlAdhan.
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// AlAdhan answered, but with something we couldn't use.
    #[error("prayer times: {0}")]
    PrayerTimes(String),
}

/// Lets a handler return `Result<_, AppError>`: on `Err`, Axum calls this
/// to build the response. The details go to the log, not to the caller.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        println!("error: {self}");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

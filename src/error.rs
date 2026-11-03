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

    /// Reading or writing a file: the daily backup.
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),

    /// Any outgoing HTTP call: GOWA or AlAdhan.
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// AlAdhan answered, but with something we couldn't use.
    #[error("prayer times: {0}")]
    PrayerTimes(String),

    /// A place lookup found nothing usable.
    #[error("location: {0}")]
    Geo(String),

    /// Open-Meteo answered, but with something we couldn't use.
    #[error("weather: {0}")]
    Weather(String),

    /// The admin API was asked for something that isn't there, or that it
    /// won't show (a DM's lists).
    #[error("not found")]
    NotFound,

    /// The admin API was asked for a change that isn't allowed. The text
    /// says why, in words the dashboard shows as they are.
    #[error("{0}")]
    BadRequest(String),
}

/// Lets a handler return `Result<_, AppError>`: on `Err`, Axum calls this
/// to build the response. The details go to the log, not to the caller.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            // Not a fault, so nothing to log. JSON, like the rest of the API.
            AppError::NotFound => (
                StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({ "error": "not found" })),
            )
                .into_response(),
            AppError::BadRequest(reason) => (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": reason })),
            )
                .into_response(),
            _ => {
                println!("error: {self}");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

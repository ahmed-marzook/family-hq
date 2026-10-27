//! Settings read from environment variables.

use std::env;

/// Everything the server needs to know at startup.
#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    /// Shared with GOWA (`GOWA_WEBHOOK_SECRET`). GOWA signs every webhook
    /// with it, and we check the signature before trusting the body.
    pub webhook_secret: String,
    /// Where GOWA's REST API is (`GOWA_URL`). Inside Compose that's
    /// `http://gowa:3000`; from `cargo run` it's the published port.
    pub gowa_url: String,
    /// GOWA's login as `user:password` (`GOWA_BASIC_AUTH`).
    pub gowa_basic_auth: String,
    /// The SQLite database (`DATABASE_URL`), e.g. `sqlite:family-hq.db`.
    /// Same variable name the `sqlx` command-line tool uses.
    pub database_url: String,
}

impl Config {
    /// Reads `PORT` (default 3000), `GOWA_URL` (default
    /// `http://localhost:3001`), `DATABASE_URL` (default
    /// `sqlite:family-hq.db`, next to where you run it), and
    /// `GOWA_WEBHOOK_SECRET` and `GOWA_BASIC_AUTH` (both required).
    ///
    /// `env::var` returns a `Result`: `Err` if the variable isn't set.
    /// `.ok()` turns that into an `Option`, `.and_then(|p| p.parse().ok())`
    /// tries to turn the text into a `u16`, and `.unwrap_or(3000)` falls
    /// back to the default if either step failed.
    pub fn from_env() -> Self {
        let port = env::var("PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(3000);

        let gowa_url = env::var("GOWA_URL").unwrap_or_else(|_| "http://localhost:3001".to_string());
        let database_url =
            env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:family-hq.db".to_string());

        Config {
            port,
            webhook_secret: required("GOWA_WEBHOOK_SECRET"),
            gowa_url,
            gowa_basic_auth: required("GOWA_BASIC_AUTH"),
            database_url,
        }
    }
}

/// Reads a variable that has no sensible default, and stops at startup if
/// it's missing, rather than running a server that fails on every webhook.
/// `.filter` turns an empty string into `None` too.
fn required(name: &str) -> String {
    env::var(name)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| panic!("{name} must be set (same value as in .env)"))
}

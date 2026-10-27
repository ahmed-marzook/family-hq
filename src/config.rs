//! Settings read from environment variables.

use std::env;
use std::str::FromStr;

use crate::aladhan::Location;

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
    /// Where and how prayer times are calculated (`PRAYER_CITY`,
    /// `PRAYER_COUNTRY`, `PRAYER_METHOD`, `PRAYER_SCHOOL`).
    pub prayer_location: Location,
    /// How many minutes before each prayer the reminder goes out
    /// (`PRAYER_REMINDER_MINUTES`).
    pub prayer_reminder_minutes: i64,
}

impl Config {
    /// Reads every setting, using defaults where there's a sensible one:
    ///
    /// | Variable | Default |
    /// |---|---|
    /// | `PORT` | 3000 |
    /// | `GOWA_URL` | `http://localhost:3001` |
    /// | `DATABASE_URL` | `sqlite:family-hq.db` (next to where you run it) |
    /// | `PRAYER_CITY` / `PRAYER_COUNTRY` | Milton Keynes / GB |
    /// | `PRAYER_METHOD` | 3 (Muslim World League) |
    /// | `PRAYER_SCHOOL` | 0 (standard Asr; 1 = Hanafi) |
    /// | `PRAYER_REMINDER_MINUTES` | 10 |
    ///
    /// `GOWA_WEBHOOK_SECRET` and `GOWA_BASIC_AUTH` have no default and must
    /// be set.
    pub fn from_env() -> Self {
        Config {
            port: number_or("PORT", 3000),
            webhook_secret: required("GOWA_WEBHOOK_SECRET"),
            gowa_url: text_or("GOWA_URL", "http://localhost:3001"),
            gowa_basic_auth: required("GOWA_BASIC_AUTH"),
            database_url: text_or("DATABASE_URL", "sqlite:family-hq.db"),
            prayer_location: Location {
                city: text_or("PRAYER_CITY", "Milton Keynes"),
                country: text_or("PRAYER_COUNTRY", "GB"),
                method: number_or("PRAYER_METHOD", 3),
                school: number_or("PRAYER_SCHOOL", 0),
            },
            prayer_reminder_minutes: number_or("PRAYER_REMINDER_MINUTES", 10),
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

/// Reads a text variable, or `default` if it isn't set (or is empty).
fn text_or(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Reads a number, or `default` if the variable isn't set.
///
/// Generic: `T` can be any type that implements `FromStr` (u8, u16, i64,
/// ...), and the caller's expected type picks it. A value that *is* set but
/// isn't a valid number stops the server, because silently using the
/// default for a typo like `PRAYER_METHOD=e` would give wrong prayer times.
fn number_or<T: FromStr>(name: &str, default: T) -> T {
    match env::var(name) {
        Ok(text) if !text.trim().is_empty() => text
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{name} must be a number, got {text:?}")),
        _ => default,
    }
}

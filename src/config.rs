//! Settings read from environment variables.

use std::env;
use std::str::FromStr;

use chrono_tz::Tz;

use crate::settings::Settings;

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
    /// Settings for every chat that hasn't chosen its own (`DEFAULT_*`).
    /// Chats change theirs with `/settings ...`.
    pub defaults: Settings,
}

impl Config {
    /// Reads every setting, using defaults where there's a sensible one:
    ///
    /// | Variable | Default |
    /// |---|---|
    /// | `PORT` | 3000 |
    /// | `GOWA_URL` | `http://localhost:3001` |
    /// | `DATABASE_URL` | `sqlite:family-hq.db` (next to where you run it) |
    /// | `DEFAULT_TIMEZONE` | Europe/London |
    /// | `DEFAULT_CITY` / `DEFAULT_COUNTRY` | Milton Keynes / GB |
    /// | `DEFAULT_PRAYER_METHOD` | 3 (Muslim World League) |
    /// | `DEFAULT_PRAYER_SCHOOL` | 0 (Shafi'i Asr; 1 = Hanafi) |
    /// | `DEFAULT_PRAYER_ENDING_MINUTES` | 20 |
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
            defaults: Settings {
                timezone: timezone_or("DEFAULT_TIMEZONE", chrono_tz::Europe::London),
                city: text_or("DEFAULT_CITY", "Milton Keynes"),
                country: text_or("DEFAULT_COUNTRY", "GB"),
                prayer_method: number_or("DEFAULT_PRAYER_METHOD", 3),
                prayer_school: number_or("DEFAULT_PRAYER_SCHOOL", 0),
                prayer_ending_minutes: number_or("DEFAULT_PRAYER_ENDING_MINUTES", 20),
            },
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
/// default for a typo like `DEFAULT_PRAYER_METHOD=e` would give wrong prayer times.
fn number_or<T: FromStr>(name: &str, default: T) -> T {
    match env::var(name) {
        Ok(text) if !text.trim().is_empty() => text
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{name} must be a number, got {text:?}")),
        _ => default,
    }
}

/// Reads an IANA timezone name like "Europe/London", or `default` if the
/// variable isn't set. A misspelt name stops the server at startup.
fn timezone_or(name: &str, default: Tz) -> Tz {
    match env::var(name) {
        Ok(text) if !text.trim().is_empty() => text.trim().parse().unwrap_or_else(|_| {
            panic!("{name} must be a timezone like Europe/London, got {text:?}")
        }),
        _ => default,
    }
}

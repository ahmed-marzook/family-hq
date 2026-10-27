//! Settings read from environment variables.

use std::env;

/// Everything the server needs to know at startup.
#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
}

impl Config {
    /// Reads `PORT` (default 3000).
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

        Config { port }
    }
}

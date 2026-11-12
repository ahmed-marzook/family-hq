//! Where log lines go, and how much is shown.
//!
//! The rest of the program logs with `tracing`'s macros: `info!` for what
//! happened, `warn!` for something wrong that it carried on past, `error!`
//! for something that failed, and `debug!` for detail that's only worth
//! seeing while chasing a problem. Each line says which module it came from
//! (`family_hq::backup`), so messages don't need an "area: " prefix.
//!
//! `RUST_LOG` picks what's shown, without a rebuild: `info` by default,
//! `debug` for everything, or per module, e.g.
//! `RUST_LOG=info,family_hq::routes::webhook=debug`.

use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

/// Starts printing log lines to stdout. Call once, first thing in `main`.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        // Colours in a terminal (`cargo run`), plain text in Docker's log.
        .with_ansi(std::io::stdout().is_terminal())
        .compact()
        .init();
}

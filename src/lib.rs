//! Family HQ library: all the logic lives here so tests can reach it.
//! `main.rs` only wires things together and starts the server.

// Modules are private (`mod`, not `pub mod`) unless `main.rs` needs them.
// In a crate with both a library and a binary, the compiler assumes anything
// `pub` in the library might be used by some other crate, so it never warns
// that a `pub` item is unused. Keeping modules private lets it point out dead
// code. The tests live inside the crate, so they can still reach everything.
//
// A module can be one file (`help.rs`) or a folder (`bot/`). For a folder,
// `mod bot;` loads `bot/mod.rs`, and that file declares the folder's other
// files as its own submodules (as `repo/mod.rs` does). Code outside still
// writes `crate::bot::...` either way.

mod aladhan;
mod backup;
mod bot;
mod cache;
mod calendar;
mod cleanup;
pub mod config;
pub mod db;
mod error;
mod geo;
mod help;
mod http;
pub mod logging;
mod metrics;
mod moon;
mod parser;
mod person;
mod points;
mod prayer;
mod progress;
mod push;
pub mod reminders;
mod repo;
mod reports;
pub mod routes;
mod settings;
mod shop;
#[cfg(test)]
pub(crate) mod testkit;
mod todo;
mod weather;
mod whatsapp;

use std::sync::Arc;

use axum::{
    Router,
    routing::{get, post},
};

use sqlx::SqlitePool;

use aladhan::PrayerTimes;
use config::Config;
use geo::Geo;
use weather::Weather;
use whatsapp::Gowa;

/// The outside services commands can use, in one place so handlers take
/// one argument rather than one per service.
pub struct Apis {
    /// AlAdhan prayer times (and the default settings).
    pub prayer_times: PrayerTimes,
    /// Places and coordinates (Nominatim, Open-Meteo geocoding).
    pub geo: Geo,
    /// Open-Meteo forecasts.
    pub weather: Weather,
    /// ntfy push notifications, if they're set up (`NTFY_URL`).
    pub push: Option<push::Ntfy>,
}

impl Apis {
    pub fn new(prayer_times: PrayerTimes) -> Self {
        Apis {
            prayer_times,
            geo: Geo::new(),
            weather: Weather::new(),
            push: None,
        }
    }

    /// Where these settings' prayer times are calculated: their stored
    /// coordinates, or (for the `.env` default or an older city setting)
    /// the town's, looked up once by name and remembered.
    pub async fn prayer_location(
        &self,
        settings: &settings::Settings,
    ) -> Result<prayer::Location, error::AppError> {
        let at = match settings.coordinates {
            Some(at) => at,
            None => {
                self.geo
                    .coordinates_of(&settings.city, &settings.country)
                    .await?
            }
        };
        Ok(settings.prayer_location(at))
    }

    /// Where these settings' town is (for the weather), the same way.
    pub async fn coordinates(
        &self,
        settings: &settings::Settings,
    ) -> Result<geo::Coordinates, error::AppError> {
        Ok(self.prayer_location(settings).await?.coordinates())
    }
}

/// Everything the web handlers and the reminder loop share.
pub struct AppState {
    pub config: Config,
    pub gowa: Gowa,
    pub db: SqlitePool,
    pub apis: Apis,
}

impl AppState {
    /// Builds the shared state, wrapped in an `Arc` (atomically
    /// reference-counted pointer) so the web server and the reminder loop
    /// can both hold it. Cloning an `Arc` only bumps a counter; everyone
    /// shares the one `AppState`.
    ///
    /// The pool is passed in rather than opened here, because opening it is
    /// `async` and can fail; `main` deals with that first.
    pub fn new(config: Config, db: SqlitePool) -> Arc<Self> {
        let gowa = Gowa::new(&config.gowa_url, &config.gowa_basic_auth);
        let prayer_times = PrayerTimes::new(config.defaults.clone());
        let mut apis = Apis::new(prayer_times);
        apis.push = config.push.clone().map(push::Ntfy::new);
        Arc::new(AppState {
            config,
            gowa,
            db,
            apis,
        })
    }
}

/// Builds the router: which handler runs for which method and path.
///
/// `with_state` hands the state to every handler that asks for
/// `State<Arc<AppState>>`. Axum clones the `Arc` for each request.
pub fn app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(routes::health::hello))
        .route("/health", get(routes::health::health))
        .route("/webhook/whatsapp", post(routes::webhook::whatsapp))
        // Buttons on push notifications; the only part meant to be public.
        .route("/actions/{*path}", post(routes::actions::action))
        .nest("/api", routes::api::router(state.clone()))
        .with_state(state)
}

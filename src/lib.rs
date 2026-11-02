//! Family HQ library: all the logic lives here so tests can reach it.
//! `main.rs` only wires things together and starts the server.

pub mod aladhan;
pub mod cache;
pub mod calendar;
pub mod cleanup;
pub mod config;
pub mod db;
pub mod error;
pub mod geo;
pub mod handlers;
pub mod help;
pub mod metrics;
pub mod moon;
pub mod parser;
pub mod person;
pub mod prayer;
pub mod prayer_stats;
pub mod prayer_tracking;
pub mod progress;
pub mod reminders;
pub mod repo;
pub mod reports;
pub mod routes;
pub mod settings;
pub mod settings_edit;
pub mod todo_reminders;
pub mod weather;
pub mod whatsapp;

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
}

impl Apis {
    pub fn new(prayer_times: PrayerTimes) -> Self {
        Apis {
            prayer_times,
            geo: Geo::new(),
            weather: Weather::new(),
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
        Arc::new(AppState {
            config,
            gowa,
            db,
            apis: Apis::new(prayer_times),
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
        .nest("/api", routes::api::router(state.clone()))
        .with_state(state)
}

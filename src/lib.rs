//! Family HQ library: all the logic lives here so tests can reach it.
//! `main.rs` only wires things together and starts the server.

pub mod aladhan;
pub mod config;
pub mod db;
pub mod error;
pub mod handlers;
pub mod parser;
pub mod person;
pub mod prayer;
pub mod reminders;
pub mod repo;
pub mod routes;
pub mod whatsapp;

use std::sync::Arc;

use axum::{
    Router,
    routing::{get, post},
};

use sqlx::SqlitePool;

use aladhan::PrayerTimes;
use config::Config;
use whatsapp::Gowa;

/// Everything the web handlers and the reminder loop share.
pub struct AppState {
    pub config: Config,
    pub gowa: Gowa,
    pub db: SqlitePool,
    pub prayer_times: PrayerTimes,
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
        let prayer_times = PrayerTimes::new(config.prayer_location.clone());
        Arc::new(AppState {
            config,
            gowa,
            db,
            prayer_times,
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
        .with_state(state)
}

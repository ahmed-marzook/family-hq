//! Family HQ library: all the logic lives here so tests can reach it.
//! `main.rs` only wires things together and starts the server.

pub mod config;
pub mod db;
pub mod error;
pub mod handlers;
pub mod parser;
pub mod person;
pub mod repo;
pub mod routes;
pub mod whatsapp;

use std::sync::Arc;

use axum::{
    Router,
    routing::{get, post},
};

use sqlx::SqlitePool;

use config::Config;
use whatsapp::Gowa;

/// Everything handlers share.
pub struct AppState {
    pub config: Config,
    pub gowa: Gowa,
    pub db: SqlitePool,
}

/// Builds the router: which handler runs for which method and path.
///
/// `with_state` hands the state to every handler that asks for
/// `State<Arc<AppState>>`. Axum clones the state for each request, and
/// cloning an `Arc` (atomically reference-counted pointer) only bumps a
/// counter, so all requests share one `AppState` instead of copying it.
///
/// The pool is passed in rather than opened here, because opening it is
/// `async` and can fail; `main` deals with that before building the app.
pub fn app(config: Config, db: SqlitePool) -> Router {
    let gowa = Gowa::new(&config.gowa_url, &config.gowa_basic_auth);

    Router::new()
        .route("/", get(routes::health::hello))
        .route("/health", get(routes::health::health))
        .route("/webhook/whatsapp", post(routes::webhook::whatsapp))
        .with_state(Arc::new(AppState { config, gowa, db }))
}

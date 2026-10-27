//! Family HQ library: all the logic lives here so tests can reach it.
//! `main.rs` only wires things together and starts the server.

pub mod config;
pub mod routes;

use axum::{Router, routing::get};

/// Builds the router: which handler runs for which method and path.
pub fn app() -> Router {
    Router::new()
        .route("/", get(routes::health::hello))
        .route("/health", get(routes::health::health))
}

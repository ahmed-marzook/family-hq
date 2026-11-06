//! HTTP handlers, one file per area.

// Private for the same reason as the modules in `lib.rs`: so the compiler
// can warn about dead code. `health` is `pub` because `main.rs` prints its
// `VERSION`; `pub(crate)` ("public inside this crate only") lets `app()` in
// `lib.rs` reach the three handlers it routes to.

pub(crate) mod actions;
mod admin;
mod admin_auth;
mod admin_cleanup;
mod admin_groups;
mod admin_metrics;
mod admin_prayers;
mod admin_reports;
mod admin_settings;
pub(crate) mod api;
pub mod health;
pub(crate) mod webhook;

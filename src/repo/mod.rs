//! All SQL lives in here, one file per area.
//!
//! Every function takes `&mut SqliteConnection` rather than the pool. A
//! transaction (`pool.begin()`) derefs to a connection, so the same
//! function works on its own or as one step of a bigger transaction:
//! `users::find(&mut tx, ...)` or `users::find(&mut *pool.acquire().await?, ...)`.
//!
//! Queries use `sqlx::query!` / `query_as!`. These macros check the SQL
//! against a real database *at compile time* (`DATABASE_URL`, or the saved
//! `.sqlx/` data when `SQLX_OFFLINE=true`), so a typo in a column name or a
//! wrong Rust type is a compile error, not a runtime surprise.

pub mod chats;
pub mod messages;
pub mod prayers;
pub mod todos;
pub mod users;

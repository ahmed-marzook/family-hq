//! Copying the whole database to a file, for `crate::backup`.

use sqlx::SqliteConnection;

/// Writes a copy of the whole database to `path`, which mustn't exist yet
/// (SQLite refuses to write over a file).
///
/// `VACUUM INTO` reads one consistent snapshot, including what's still in
/// the WAL file, so the copy is never half-written even while the bot is
/// busy, and nobody's writes wait for it. The copy is a single ordinary
/// database file, with no `-wal` beside it.
pub async fn vacuum_into(conn: &mut SqliteConnection, path: &str) -> Result<(), sqlx::Error> {
    sqlx::query!("VACUUM INTO ?", path).execute(conn).await?;
    Ok(())
}

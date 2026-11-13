//! The highest level each person has reached per category and Overall
//! (`levels_reached`), so a level-up is announced once. The levels
//! themselves are worked out from the ledger (`points::levels`).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;

use crate::points::levels::Track;

/// `user_id`'s highest level on each track that has one recorded. A track
/// missing from it hasn't gone past Lv 1.
pub async fn reached(
    conn: &mut SqliteConnection,
    user_id: i64,
) -> Result<BTreeMap<Track, i64>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT category, level FROM levels_reached WHERE user_id = ?",
        user_id
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| Some((Track::from_db_str(&r.category)?, r.level)))
        .collect())
}

/// Records that `user_id` reached `level` on `track`. Never lowers it: the
/// highest is what stops a level being announced twice.
pub async fn record(
    conn: &mut SqliteConnection,
    user_id: i64,
    track: Track,
    level: i64,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let category = track.as_db_str();
    sqlx::query!(
        "INSERT INTO levels_reached (user_id, category, level, reached_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT (user_id, category) DO UPDATE
         SET level = excluded.level, reached_at = excluded.reached_at
         WHERE excluded.level > levels_reached.level",
        user_id,
        category,
        level,
        now
    )
    .execute(conn)
    .await?;
    Ok(())
}

//! Which week and month was last announced as each person's best ever
//! (`bests_announced`), so a personal best is announced once. The bests
//! themselves are worked out from the ledger (`points::history`).

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqliteConnection;

use crate::reports::Span;

/// The first day of the `span` (week or month) last announced as
/// `user_id`'s best, if any ever was.
pub async fn announced(
    conn: &mut SqliteConnection,
    user_id: i64,
    span: Span,
) -> Result<Option<NaiveDate>, sqlx::Error> {
    let span = span.as_db_str();
    sqlx::query_scalar!(
        r#"SELECT period_first AS "period_first: NaiveDate"
           FROM bests_announced WHERE user_id = ? AND span = ?"#,
        user_id,
        span
    )
    .fetch_optional(conn)
    .await
}

/// Records that the `span` starting on `period_first` was announced as
/// `user_id`'s best.
pub async fn record(
    conn: &mut SqliteConnection,
    user_id: i64,
    span: Span,
    period_first: NaiveDate,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let span = span.as_db_str();
    sqlx::query!(
        "INSERT INTO bests_announced (user_id, span, period_first, announced_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT (user_id, span) DO UPDATE
         SET period_first = excluded.period_first, announced_at = excluded.announced_at",
        user_id,
        span,
        period_first,
        now
    )
    .execute(conn)
    .await?;
    Ok(())
}

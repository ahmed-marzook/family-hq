//! Ticking a prayer off, and taking the tick back: the one way every path
//! does it. A reaction on a reminder (`routes::webhook`), the button on a
//! push (`routes::actions`) and `+prayed` (`prayer::commands`) all call
//! `tick`, so they always record the same thing.
//!
//! What each path then *counts* differs (a reaction is logged under its
//! chat, a button under the person's DM, a command by the webhook), so
//! that stays with each caller.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqliteConnection;
use tracing::info;

use crate::prayer::stats::Status;
use crate::prayer::{Prayer, Tick};
use crate::repo::prayers;

/// Marks `prayer` on `date` as prayed by `user_id`: on time, or made up if
/// its time had ended and it was marked missed. Returns what changed, or
/// `None` if it was already ticked (a second reaction, a retried webhook,
/// the button tapped twice), so the caller needn't say so twice.
pub async fn tick(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
    prayer: Prayer,
    now: DateTime<Utc>,
) -> Result<Option<Tick>, sqlx::Error> {
    let status = prayers::mark_prayed(conn, user_id, date, prayer, now).await?;
    info!("prayed {prayer} on {date}");
    // Its time had ended and it was marked missed: made up.
    Ok(status.map(|s| match s {
        Status::Late => Tick::MadeUp,
        _ => Tick::Prayed,
    }))
}

/// Takes a tick back (the reaction was removed). Returns `None` if there
/// was nothing to take back.
pub async fn untick(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
    prayer: Prayer,
) -> Result<Option<Tick>, sqlx::Error> {
    let changed = prayers::unmark_prayed(conn, user_id, date, prayer).await?;
    info!("unticked {prayer} on {date}");
    Ok(changed.then_some(Tick::Unticked))
}

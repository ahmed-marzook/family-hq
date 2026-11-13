//! Charging for overdue todos: an open todo past its due date costs the
//! person it's for 1 a day, for up to `OVERDUE_DAYS` days
//! (`points::for_overdue`). Run every minute from the reminder loop.
//!
//! * Whose clock: the owner's own (the chat's settings with theirs on top,
//!   as `/todo` shows them), so a todo due Monday is charged from their
//!   Tuesday 00:00.
//! * Once a day per todo: the ledger's unique index says so, so running
//!   every minute (and after a restart) charges it once.
//! * A day the bot is down for entirely isn't charged afterwards. Only
//!   today is ever charged: catching up could charge a todo for days its
//!   owner couldn't see a reminder about.
//! * Nobody is messaged: the morning todo reminder says what was charged
//!   (`todo::reminders`), and it shows in `/points`.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use sqlx::SqliteConnection;
use tracing::warn;

use crate::AppState;
use crate::error::AppError;
use crate::points::{OVERDUE_DAYS, for_overdue};
use crate::repo::points as stored_points;
use crate::repo::settings as stored_settings;
use crate::repo::todos;
use crate::settings::Settings;

/// Charges every overdue todo due a charge at `now`.
pub async fn check(state: &AppState, now: DateTime<Utc>) -> Result<(), AppError> {
    let mut conn = state.db.acquire().await?;
    charge(&mut conn, &state.apis.prayer_times.defaults, now).await?;
    Ok(())
}

/// The work of `check`, on one connection. Returns how many charges were
/// made just now.
pub async fn charge(
    conn: &mut SqliteConnection,
    defaults: &Settings,
    now: DateTime<Utc>,
) -> Result<usize, AppError> {
    // Somewhere in the world it's the day before or after UTC's, so these
    // are all the todos that can be 1 to OVERDUE_DAYS days late anywhere.
    let utc_today = now.date_naive();
    let first = utc_today - Duration::days(OVERDUE_DAYS + 1);
    let todos = todos::due_between(conn, first, utc_today).await?;

    // One lookup per person and chat, not per todo.
    let mut today_of = HashMap::new();
    let mut charged = 0;
    for todo in todos {
        let key = (todo.chat_id, todo.owner_id);
        let today = match today_of.get(&key) {
            Some(today) => *today,
            None => {
                let settings =
                    stored_settings::effective(conn, defaults, todo.chat_id, Some(todo.owner_id))
                        .await?;
                let today = settings.today(now);
                today_of.insert(key, today);
                today
            }
        };
        let Some(award) = for_overdue(todo.due, today) else {
            continue;
        };
        // One todo's problem mustn't stop the others being charged.
        match stored_points::charge_overdue(
            conn,
            todo.owner_id,
            todo.chat_id,
            todo.id,
            award,
            today,
            now,
        )
        .await
        {
            Ok(true) => charged += 1,
            Ok(false) => {}
            Err(err) => warn!("couldn't charge overdue todo {}: {err}", todo.id),
        }
    }
    Ok(charged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::tests::{setup, typed};
    use crate::db::test_pool;
    use crate::reports::Period;
    use crate::settings::tests::defaults;
    use crate::testkit::{date, utc};

    async fn points_on(conn: &mut SqliteConnection, chat_id: i64, day: &str) -> Vec<(String, i64)> {
        let day = date(day);
        let mut totals = stored_points::totals(
            conn,
            chat_id,
            Period {
                first: day,
                last: day,
            },
        )
        .await
        .unwrap();
        totals.sort();
        totals
    }

    #[tokio::test]
    async fn an_overdue_todo_is_charged_once_a_day_to_whoever_its_for() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        // Monday 9 November 2026: Bilal adds two todos due today, one for
        // himself and one for Khadija.
        ctx.now = utc("2026-11-09T09:00:00Z");
        typed(&mut conn, &ctx, "+todo fix the tap -d today").await;
        typed(&mut conn, &ctx, "+todo @Khadija call gran -d today").await;
        let chat = ctx.chat_id;
        let defaults = defaults();

        // Still Monday: due today isn't overdue.
        let monday = utc("2026-11-09T23:59:00Z");
        assert_eq!(charge(&mut conn, &defaults, monday).await.unwrap(), 0);

        // Tuesday, just after midnight (GMT): both charged, each to its owner.
        let tuesday = utc("2026-11-10T00:01:00Z");
        assert_eq!(charge(&mut conn, &defaults, tuesday).await.unwrap(), 2);
        // A minute later: already charged today.
        let later = tuesday + Duration::minutes(1);
        assert_eq!(charge(&mut conn, &defaults, later).await.unwrap(), 0);
        assert_eq!(
            points_on(&mut conn, chat, "2026-11-10").await,
            vec![("Bilal".to_string(), -1), ("Khadija".to_string(), -1)]
        );

        // Bilal does his on Wednesday morning: it still earns (late), and
        // only Khadija's is charged that day.
        ctx.now = utc("2026-11-11T08:00:00Z");
        typed(&mut conn, &ctx, "+done tap").await;
        charge(&mut conn, &defaults, utc("2026-11-11T08:01:00Z"))
            .await
            .unwrap();
        assert_eq!(
            points_on(&mut conn, chat, "2026-11-11").await,
            vec![("Bilal".to_string(), 1), ("Khadija".to_string(), -1)],
            "done late: 2 − 1 = 1, and no charge for it"
        );

        // A week after it was due is the last charge; the 8th day isn't.
        assert_eq!(
            charge(&mut conn, &defaults, utc("2026-11-16T12:00:00Z"))
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            charge(&mut conn, &defaults, utc("2026-11-17T12:00:00Z"))
                .await
                .unwrap(),
            0,
            "the cap: Khadija's is past its 7 days"
        );
    }

    #[tokio::test]
    async fn the_day_turns_on_the_owners_clock() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        crate::repo::settings::set_timezone(&mut conn, ctx.chat_id, "Asia/Colombo")
            .await
            .unwrap();
        ctx.now = utc("2026-11-09T09:00:00Z");
        typed(&mut conn, &ctx, "+todo fix the tap -d today").await;
        let defaults = defaults();

        // 18:30 UTC Monday is already Tuesday 00:00 in Colombo (UTC+5:30).
        assert_eq!(
            charge(&mut conn, &defaults, utc("2026-11-09T18:29:00Z"))
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            charge(&mut conn, &defaults, utc("2026-11-09T18:30:00Z"))
                .await
                .unwrap(),
            1
        );
    }
}

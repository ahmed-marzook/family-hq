//! The points ledger: one row per award (see `points` for the rules).

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqliteConnection;

use crate::points::{Award, DayPoints, Source};
use crate::prayer::Prayer;
use crate::reports::Period;

/// Records `award` for `user_id`, earned in `chat_id` on their local `day`.
/// A source (one todo) awards once: a second call for the same one changes
/// nothing (`ON CONFLICT DO NOTHING`), so a retried webhook can't pay twice.
#[allow(clippy::too_many_arguments)] // each is a column; a struct wouldn't be clearer
pub async fn award(
    conn: &mut SqliteConnection,
    user_id: i64,
    chat_id: i64,
    source: Source,
    source_id: i64,
    award: Award,
    day: NaiveDate,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let (source, reason) = (source.as_db_str(), award.reason.as_db_str());
    sqlx::query!(
        "INSERT INTO points (user_id, chat_id, source, source_id, reason, points, day, at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (source, source_id) WHERE source NOT IN ('prayer', 'overdue') DO NOTHING",
        user_id,
        chat_id,
        source,
        source_id,
        reason,
        award.points,
        day,
        now
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Charges `user_id` for todo `todo_id` being overdue on their local
/// `day`, in the todo's chat. Once a day: a second call for the same todo
/// and day changes nothing, so the check running every minute charges it
/// once. Returns whether it was charged just now.
#[allow(clippy::too_many_arguments)] // each is a column; a struct wouldn't be clearer
pub async fn charge_overdue(
    conn: &mut SqliteConnection,
    user_id: i64,
    chat_id: i64,
    todo_id: i64,
    award: Award,
    day: NaiveDate,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let (source, reason) = (Source::Overdue.as_db_str(), award.reason.as_db_str());
    let result = sqlx::query!(
        "INSERT INTO points (user_id, chat_id, source, source_id, reason, points, day, at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (source_id, day) WHERE source = 'overdue' DO NOTHING",
        user_id,
        chat_id,
        source,
        todo_id,
        reason,
        award.points,
        day,
        now
    )
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Sets what `user_id`'s `prayer` on `day` is worth, to match the prayer
/// log (see `prayer::checkin`). A prayer has one row: the first award adds
/// it, in `chat_id`, and later ones (made up → corrected to on time)
/// change its points but keep it in the chat it was first earned in.
///
/// With no `chat_id` (the admin dashboard, for someone with no DM), only
/// a row that's already there is changed: a row must belong to a chat.
pub async fn set_prayer(
    conn: &mut SqliteConnection,
    user_id: i64,
    chat_id: Option<i64>,
    day: NaiveDate,
    prayer: Prayer,
    award: Award,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let (source, reason) = (Source::Prayer.as_db_str(), award.reason.as_db_str());
    let number = prayer.number();
    match chat_id {
        // The `WHERE` after the conflict target names the partial unique
        // index it means (`points_once_per_prayer`, see 0011).
        Some(chat_id) => {
            sqlx::query!(
                "INSERT INTO points (user_id, chat_id, source, source_id, reason, points, day, at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (user_id, day, source_id) WHERE source = 'prayer' DO UPDATE
                 SET reason = excluded.reason, points = excluded.points",
                user_id,
                chat_id,
                source,
                number,
                reason,
                award.points,
                day,
                now
            )
            .execute(conn)
            .await?;
        }
        None => {
            sqlx::query!(
                "UPDATE points SET reason = ?, points = ?
                 WHERE source = 'prayer' AND user_id = ? AND day = ? AND source_id = ?",
                reason,
                award.points,
                user_id,
                day,
                number
            )
            .execute(conn)
            .await?;
        }
    }
    Ok(())
}

/// Takes away what `user_id`'s `prayer` on `day` earned: it was unticked,
/// or set to missed. Nothing happens if it had earned nothing.
pub async fn clear_prayer(
    conn: &mut SqliteConnection,
    user_id: i64,
    day: NaiveDate,
    prayer: Prayer,
) -> Result<(), sqlx::Error> {
    let number = prayer.number();
    sqlx::query!(
        "DELETE FROM points
         WHERE source = 'prayer' AND user_id = ? AND day = ? AND source_id = ?",
        user_id,
        day,
        number
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Each person's total in `chat_id` over `period`, as `(name, points)`.
/// Only people with points in it; in no particular order (`points::rank`
/// sorts them).
pub async fn totals(
    conn: &mut SqliteConnection,
    chat_id: i64,
    period: Period,
) -> Result<Vec<(String, i64)>, sqlx::Error> {
    let totals = totals_by_user(conn, chat_id, period).await?;
    Ok(totals
        .into_iter()
        .map(|(_, name, points)| (name, points))
        .collect())
}

/// `totals` with each person's user id first, as `(user_id, name, points)`:
/// for matching people up with other lists by who they are, not by the
/// name they're shown under.
pub async fn totals_by_user(
    conn: &mut SqliteConnection,
    chat_id: i64,
    period: Period,
) -> Result<Vec<(i64, String, i64)>, sqlx::Error> {
    // `BETWEEN` includes both ends. Dates stored as "YYYY-MM-DD" compare
    // correctly as text, so no date maths is needed in SQL.
    let rows = sqlx::query!(
        r#"SELECT p.user_id, u.display_name AS name, SUM(p.points) AS "points!: i64"
           FROM points p
           JOIN users u ON u.id = p.user_id
           WHERE p.chat_id = ? AND p.day BETWEEN ? AND ?
           GROUP BY p.user_id"#,
        chat_id,
        period.first,
        period.last
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.user_id, r.name, r.points))
        .collect())
}

/// `user_id`'s points on each day of `period` that has any, from every
/// chat, oldest first.
pub async fn by_day(
    conn: &mut SqliteConnection,
    user_id: i64,
    period: Period,
) -> Result<Vec<(NaiveDate, i64)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT day AS "day: NaiveDate", SUM(points) AS "points!: i64"
           FROM points
           WHERE user_id = ? AND day BETWEEN ? AND ?
           GROUP BY day
           ORDER BY day"#,
        user_id,
        period.first,
        period.last
    )
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|r| (r.day, r.points)).collect())
}

/// `user_id`'s points over `period` in each chat they earned them in: the
/// group's name, or `None` for a DM. Most points first.
pub async fn by_chat(
    conn: &mut SqliteConnection,
    user_id: i64,
    period: Period,
) -> Result<Vec<(Option<String>, i64)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT c.name AS "name?", SUM(p.points) AS "points!: i64"
           FROM points p
           JOIN chats c ON c.id = p.chat_id
           WHERE p.user_id = ? AND p.day BETWEEN ? AND ?
           GROUP BY p.chat_id
           ORDER BY 2 DESC, c.id"#,
        user_id,
        period.first,
        period.last
    )
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|r| (r.name, r.points)).collect())
}

/// `user_id`'s lifetime XP from each source, from every chat: only
/// positive awards count (see `points::levels`), so a skip never takes XP
/// away. Sources this build doesn't know are left out.
pub async fn xp_by_source(
    conn: &mut SqliteConnection,
    user_id: i64,
) -> Result<Vec<(Source, i64)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT source, SUM(points) AS "xp!: i64"
           FROM points
           WHERE user_id = ? AND points > 0
           GROUP BY source"#,
        user_id
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| Some((Source::from_db_str(&r.source)?, r.xp)))
        .collect())
}

/// The lifetime XP from each source of everyone who earned points in
/// `chat_id` over `period`, as `(user_id, source, xp)`: what the group
/// leaderboard needs to show each person's level. XP counts from every
/// chat, as levels are personal.
pub async fn xp_of_earners(
    conn: &mut SqliteConnection,
    chat_id: i64,
    period: Period,
) -> Result<Vec<(i64, Source, i64)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT p.user_id, p.source, SUM(p.points) AS "xp!: i64"
           FROM points p
           WHERE p.points > 0
             AND p.user_id IN (SELECT user_id FROM points
                               WHERE chat_id = ? AND day BETWEEN ? AND ?)
           GROUP BY p.user_id, p.source"#,
        chat_id,
        period.first,
        period.last
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| Some((r.user_id, Source::from_db_str(&r.source)?, r.xp)))
        .collect())
}

/// `user_id`'s points over `period` from each source, from every chat.
/// Net, like the week's points: a skip's −1 counts here.
pub async fn by_source(
    conn: &mut SqliteConnection,
    user_id: i64,
    period: Period,
) -> Result<Vec<(Source, i64)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT source, SUM(points) AS "points!: i64"
           FROM points
           WHERE user_id = ? AND day BETWEEN ? AND ?
           GROUP BY source"#,
        user_id,
        period.first,
        period.last
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| Some((Source::from_db_str(&r.source)?, r.points)))
        .collect())
}

/// Each person's points from each source in `chat_id` over `period`, as
/// `(name, source, points)`: what the weekly summary's category champions
/// are worked out from (`points::game`). Net, like the week's points.
pub async fn by_person_source(
    conn: &mut SqliteConnection,
    chat_id: i64,
    period: Period,
) -> Result<Vec<(String, Source, i64)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT u.display_name AS name, p.source, SUM(p.points) AS "points!: i64"
           FROM points p
           JOIN users u ON u.id = p.user_id
           WHERE p.chat_id = ? AND p.day BETWEEN ? AND ?
           GROUP BY p.user_id, p.source"#,
        chat_id,
        period.first,
        period.last
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| Some((r.name, Source::from_db_str(&r.source)?, r.points)))
        .collect())
}

/// The first day `user_id` earned (or lost) anything, from any chat, or
/// `None` if they never have: where their history starts.
pub async fn first_day(
    conn: &mut SqliteConnection,
    user_id: i64,
) -> Result<Option<NaiveDate>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT MIN(day) AS "day: NaiveDate" FROM points WHERE user_id = ?"#,
        user_id
    )
    .fetch_one(conn)
    .await
}

/// `user_id`'s points on each day of `period`, from each source and every
/// chat, as `(day, source, points)`, oldest first: what their history
/// (`points::history`) is worked out from. Net, like the week's points.
pub async fn by_day_and_source(
    conn: &mut SqliteConnection,
    user_id: i64,
    period: Period,
) -> Result<Vec<(NaiveDate, Source, i64)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT day AS "day: NaiveDate", source, SUM(points) AS "points!: i64"
           FROM points
           WHERE user_id = ? AND day BETWEEN ? AND ?
           GROUP BY day, source
           ORDER BY day, source"#,
        user_id,
        period.first,
        period.last
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| Some((r.day, Source::from_db_str(&r.source)?, r.points)))
        .collect())
}

/// `xp_by_source`, counting only what was earned on or before `last`: the
/// XP someone had at the end of that day, for the levels they gained in a
/// month or a year.
pub async fn xp_by_source_until(
    conn: &mut SqliteConnection,
    user_id: i64,
    last: NaiveDate,
) -> Result<Vec<(Source, i64)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT source, SUM(points) AS "xp!: i64"
           FROM points
           WHERE user_id = ? AND points > 0 AND day <= ?
           GROUP BY source"#,
        user_id,
        last
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| Some((Source::from_db_str(&r.source)?, r.xp)))
        .collect())
}

/// Each person's points on each day of `period` in `chat_id`, for the
/// weekly summary. Oldest first.
pub async fn by_person_day(
    conn: &mut SqliteConnection,
    chat_id: i64,
    period: Period,
) -> Result<Vec<DayPoints>, sqlx::Error> {
    sqlx::query_as!(
        DayPoints,
        r#"SELECT p.user_id, u.display_name AS name, p.day AS "day: NaiveDate",
                  SUM(p.points) AS "points!: i64"
           FROM points p
           JOIN users u ON u.id = p.user_id
           WHERE p.chat_id = ? AND p.day BETWEEN ? AND ?
           GROUP BY p.user_id, p.day
           ORDER BY p.day, p.user_id"#,
        chat_id,
        period.first,
        period.last
    )
    .fetch_all(conn)
    .await
}

/// A group that may be due a weekly summary.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportGroup {
    pub id: i64,
    pub wa_chat_id: String,
    /// The group's name, if the bot has learnt it.
    pub name: Option<String>,
}

/// The groups the bot is still in where anyone has earned points on or
/// after `since`. Only these can have something to say on Sunday.
pub async fn report_groups(
    conn: &mut SqliteConnection,
    since: NaiveDate,
) -> Result<Vec<ReportGroup>, sqlx::Error> {
    sqlx::query_as!(
        ReportGroup,
        r#"SELECT c.id, c.wa_chat_id, c.name
           FROM chats c
           WHERE c.kind = 'group' AND c.left_at IS NULL
             AND EXISTS (SELECT 1 FROM points p WHERE p.chat_id = c.id AND p.day >= ?)
           ORDER BY c.id"#,
        since
    )
    .fetch_all(conn)
    .await
}

/// A person's DM with the bot, which may be due a monthly or yearly review.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewDm {
    pub chat_id: i64,
    pub wa_chat_id: String,
    pub user_id: i64,
}

/// The DMs the bot still has whose owner earned (or lost) points, in any
/// chat, on or after `since`. Only these can have a review to send.
pub async fn review_dms(
    conn: &mut SqliteConnection,
    since: NaiveDate,
) -> Result<Vec<ReviewDm>, sqlx::Error> {
    sqlx::query_as!(
        ReviewDm,
        r#"SELECT c.id AS chat_id, c.wa_chat_id, c.dm_user_id AS "user_id!: i64"
           FROM chats c
           WHERE c.kind = 'dm' AND c.left_at IS NULL
             AND EXISTS (SELECT 1 FROM points p WHERE p.user_id = c.dm_user_id AND p.day >= ?)
           ORDER BY c.id"#,
        since
    )
    .fetch_all(conn)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::points::Reason;
    use crate::repo::chats;
    use crate::testkit::{date, register};

    fn period(first: &str, last: &str) -> Period {
        Period {
            first: date(first),
            last: date(last),
        }
    }

    fn pts(points: i64) -> Award {
        Award {
            points,
            reason: Reason::Done,
        }
    }

    async fn user(conn: &mut SqliteConnection, name: &str, phone: &str) -> i64 {
        register(conn, name, phone, Utc::now()).await.id
    }

    #[tokio::test]
    async fn a_todo_awards_once_and_totals_add_up() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let bilal = user(&mut conn, "Bilal", "447700900001").await;
        let sara = user(&mut conn, "Sara", "447700900002").await;
        let group = chats::find_or_create(&mut conn, "1@g.us", None, now)
            .await
            .unwrap();
        let other = chats::find_or_create(&mut conn, "2@g.us", None, now)
            .await
            .unwrap();
        let day = date("2026-11-06");

        let todo = Source::Todo;
        award(&mut conn, bilal, group, todo, 1, pts(3), day, now)
            .await
            .unwrap();
        // The same todo again (a retried webhook): ignored.
        award(&mut conn, bilal, group, todo, 1, pts(3), day, now)
            .await
            .unwrap();
        award(&mut conn, bilal, group, todo, 2, pts(-1), day, now)
            .await
            .unwrap();
        award(
            &mut conn,
            sara,
            group,
            todo,
            3,
            pts(4),
            date("2026-11-02"),
            now,
        )
        .await
        .unwrap();
        // Outside the week, and in another group: not counted.
        award(
            &mut conn,
            sara,
            group,
            todo,
            4,
            pts(2),
            date("2026-11-01"),
            now,
        )
        .await
        .unwrap();
        award(&mut conn, sara, other, todo, 5, pts(2), day, now)
            .await
            .unwrap();

        let week = period("2026-11-02", "2026-11-08");
        let mut got = totals(&mut conn, group, week).await.unwrap();
        got.sort();
        assert_eq!(got, vec![("Bilal".to_string(), 2), ("Sara".to_string(), 4)]);

        assert_eq!(
            by_day(&mut conn, sara, period("2026-11-01", "2026-11-08"))
                .await
                .unwrap(),
            vec![(date("2026-11-01"), 2), (date("2026-11-02"), 4), (day, 2)]
        );
        assert_eq!(
            by_chat(&mut conn, sara, week).await.unwrap(),
            vec![(None, 4), (None, 2)],
            "groups made by find_or_create have no name yet"
        );
    }
}

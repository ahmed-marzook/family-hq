//! Reading a person's stats and announcing their level-ups: the glue
//! between the ledger (`repo::points`), what's been announced
//! (`repo::levels`) and the pure rules (`points::levels`).
//!
//! Announcing is separate from awarding on purpose. Points are earned in
//! many places (a reply, a reaction, `+prayed`, a push button, the
//! dashboard, the reminder loop), and not all of them have a reply to add
//! a line to. So any reply to someone who may have earned something calls
//! `lines` once at the end, and it announces whatever is new since last
//! time, however it was earned. A level reached through a push button is
//! announced in the next reply instead of being lost.
//!
//! Personal bests ("🌟 Your best week ever") work the same way: worked out
//! from the ledger (`points::history::new_best`), announced once per week
//! or month (`repo::bests`), in whichever reply comes first.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqliteConnection;

use crate::bot::Context;
use crate::points::commands::ledger;
use crate::points::history::{best_text, new_best};
use crate::points::levels::{LevelUp, Stats, level_up_text, level_ups};
use crate::repo::bests;
use crate::repo::levels as reached;
use crate::repo::points as stored_points;
use crate::repo::users::User;
use crate::reports::Span;

/// `user_id`'s lifetime stats, from every chat.
pub async fn stats(conn: &mut SqliteConnection, user_id: i64) -> Result<Stats, sqlx::Error> {
    let xp = stored_points::xp_by_source(conn, user_id).await?;
    Ok(Stats::from_sources(xp))
}

/// The levels `user_id` has reached for the first time since they were
/// last announced, recorded as reached so they're only ever announced
/// once. Must run in the same transaction as whatever earned them, so a
/// rolled-back award doesn't leave a level recorded.
pub async fn announce(
    conn: &mut SqliteConnection,
    user_id: i64,
    now: DateTime<Utc>,
) -> Result<Vec<LevelUp>, sqlx::Error> {
    let ups = level_ups(
        &reached::reached(conn, user_id).await?,
        &stats(conn, user_id).await?,
    );
    for up in &ups {
        reached::record(conn, user_id, up.track, up.level, now).await?;
    }
    Ok(ups)
}

/// The weeks and months `user_id` has just set a personal best in, on
/// their own `today`, with the points: each only the first time it's
/// found, recorded so it isn't announced again that week or month. Must
/// run in the same transaction as whatever earned it, like `announce`.
pub async fn new_bests(
    conn: &mut SqliteConnection,
    user_id: i64,
    today: NaiveDate,
    now: DateTime<Utc>,
) -> Result<Vec<(Span, i64)>, sqlx::Error> {
    let ledger = ledger(conn, user_id, today).await?;
    let mut bests = Vec::new();
    for span in [Span::Week, Span::Month] {
        let Some((period, points)) = new_best(&ledger, span, today) else {
            continue;
        };
        if bests::announced(conn, user_id, span).await? == Some(period.first) {
            continue;
        }
        bests::record(conn, user_id, span, period.first, now).await?;
        bests.push((span, points));
    }
    Ok(bests)
}

/// The lines to add to a reply to `user`: one per new level, then one per
/// new personal best. In a group each names them, since the reply may sit
/// under someone else's message. `today` is on their own clock.
pub async fn lines(
    conn: &mut SqliteConnection,
    user: &User,
    in_group: bool,
    today: NaiveDate,
    now: DateTime<Utc>,
) -> Result<Vec<String>, sqlx::Error> {
    let who = in_group.then_some(user.display_name.as_str());
    let mut lines: Vec<String> = announce(conn, user.id, now)
        .await?
        .into_iter()
        .map(|up| level_up_text(up, who))
        .collect();
    for (span, points) in new_bests(conn, user.id, today, now).await? {
        lines.push(best_text(span, points, who));
    }
    Ok(lines)
}

/// `lines` for the sender of a command, on their own `today`.
pub async fn for_sender(
    conn: &mut SqliteConnection,
    ctx: &Context,
    today: NaiveDate,
) -> Result<Vec<String>, sqlx::Error> {
    lines(conn, &ctx.sender, ctx.is_group, today, ctx.now).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::points::levels::{Category, Track};
    use crate::points::{Award, Reason, Source, for_todo};
    use crate::prayer::Prayer;
    use crate::prayer::checkin;
    use crate::repo::chats;
    use crate::testkit::{date, register};

    #[tokio::test]
    async fn each_level_up_is_announced_once_even_after_an_untick() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let sara = register(&mut conn, "Sara", "447700900002", now).await;
        let dm =
            chats::find_or_create(&mut conn, "447700900002@s.whatsapp.net", Some(sara.id), now)
                .await
                .unwrap();

        // 34 prayer XP: 17 days of one prayer on time. Still Lv 1. (A
        // steady 14 a week, so no personal best gets in the way.)
        let first = date("2026-10-01");
        let today = date("2026-10-18");
        for d in 0..17 {
            let day = first + chrono::Duration::days(d);
            checkin::tick(&mut conn, sara.id, day, Prayer::Fajr, Some(dm), now)
                .await
                .unwrap();
        }
        assert!(
            lines(&mut conn, &sara, false, today, now)
                .await
                .unwrap()
                .is_empty()
        );

        // The 35th XP: Prayers Lv 2, and Overall 3.
        let day = date("2026-10-18");
        checkin::tick(&mut conn, sara.id, day, Prayer::Fajr, Some(dm), now)
            .await
            .unwrap();
        assert_eq!(
            lines(&mut conn, &sara, true, today, now).await.unwrap(),
            vec!["🎉 Sara: Prayers level 2!", "🏅 Sara: Overall level 3"]
        );
        assert!(
            lines(&mut conn, &sara, false, today, now)
                .await
                .unwrap()
                .is_empty(),
            "only once"
        );

        // Unticked (back to Lv 1) and ticked again: not announced again.
        checkin::untick(&mut conn, sara.id, day, Prayer::Fajr, now)
            .await
            .unwrap();
        assert_eq!(stats(&mut conn, sara.id).await.unwrap().overall(), 2);
        assert!(
            lines(&mut conn, &sara, false, today, now)
                .await
                .unwrap()
                .is_empty()
        );
        checkin::tick(&mut conn, sara.id, day, Prayer::Fajr, Some(dm), now)
            .await
            .unwrap();
        assert!(
            lines(&mut conn, &sara, false, today, now)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            reached::reached(&mut conn, sara.id).await.unwrap()[&Track::Overall],
            3
        );
    }

    #[tokio::test]
    async fn a_personal_best_is_announced_once_in_the_reply_that_sets_it() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let sara = register(&mut conn, "Sara", "447700900002", now).await;
        let group = chats::find_or_create(&mut conn, "1@g.us", None, now)
            .await
            .unwrap();
        let mut todo_id = 0;
        let mut earn = async |conn: &mut SqliteConnection, day: &str, points| {
            todo_id += 1;
            let award = Award {
                points,
                reason: Reason::Done,
            };
            stored_points::award(
                conn,
                sara.id,
                group,
                Source::Todo,
                todo_id,
                award,
                date(day),
                now,
            )
            .await
            .unwrap();
        };
        // 2 in the week of 2 Nov, then 1 so far in the week of 9 Nov.
        // (Under 15 XP throughout, so no level-up joins in.)
        let today = date("2026-11-12");
        earn(&mut conn, "2026-11-03", 2).await;
        earn(&mut conn, "2026-11-10", 1).await;
        let none = lines(&mut conn, &sara, false, today, now).await.unwrap();
        assert!(none.is_empty(), "{none:?}");

        // 3: her best week ever, announced once.
        earn(&mut conn, "2026-11-12", 2).await;
        assert_eq!(
            lines(&mut conn, &sara, false, today, now).await.unwrap(),
            ["🌟 Your best week ever (3)"]
        );
        earn(&mut conn, "2026-11-12", 1).await;
        let again = lines(&mut conn, &sara, false, today, now).await.unwrap();
        assert!(again.is_empty(), "not again that week: {again:?}");

        // The next week beats it: announced again, by name in a group.
        let monday = date("2026-11-16");
        earn(&mut conn, "2026-11-16", 2).await;
        earn(&mut conn, "2026-11-16", 2).await;
        let equal = lines(&mut conn, &sara, true, monday, now).await.unwrap();
        assert!(equal.is_empty(), "4 only equals it: {equal:?}");
        earn(&mut conn, "2026-11-16", 1).await;
        assert_eq!(
            lines(&mut conn, &sara, true, monday, now).await.unwrap(),
            ["🌟 Sara: best week ever!"]
        );
    }

    #[tokio::test]
    async fn a_skip_lowers_the_week_but_not_xp() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let bilal = register(&mut conn, "Bilal", "447700900001", now).await;
        let group = chats::find_or_create(&mut conn, "1@g.us", None, now)
            .await
            .unwrap();
        let day = date("2026-11-09");

        let done = Award {
            points: 3,
            reason: Reason::Done,
        };
        let skip = for_todo(crate::points::Finish::Skipped, None, None, day);
        stored_points::award(&mut conn, bilal.id, group, Source::Todo, 1, done, day, now)
            .await
            .unwrap();
        stored_points::award(&mut conn, bilal.id, group, Source::Todo, 2, skip, day, now)
            .await
            .unwrap();

        let week = crate::reports::Period {
            first: day,
            last: day,
        };
        assert_eq!(
            stored_points::by_source(&mut conn, bilal.id, week)
                .await
                .unwrap(),
            vec![(Source::Todo, 2)]
        );
        assert_eq!(
            stats(&mut conn, bilal.id)
                .await
                .unwrap()
                .xp(Category::Todos),
            3
        );
    }

    /// Migration 0012 seeds `levels_reached` with SQL that copies the level
    /// rules as they were then. Run that SQL again over known XP, just
    /// below and on level boundaries, and it must agree with `Stats`.
    #[tokio::test]
    async fn the_0012_seed_agrees_with_the_level_rules() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = chrono::Utc::now();
        let chat = chats::find_or_create(&mut conn, "120363000000000001@g.us", None, now)
            .await
            .unwrap();
        // (todo XP, prayer XP, skips): todos reach Lv 2 at 15, Lv 3 at 45;
        // prayers Lv 2 at 35, Lv 3 at 105. A skip mustn't lower XP.
        let people = [
            (14, 0, 0),
            (15, 34, 1),
            (44, 35, 2),
            (45, 104, 0),
            (0, 105, 3),
        ];
        let mut todo_id = 0;
        let mut ids = Vec::new();
        for (i, (todos, prayers, skips)) in people.into_iter().enumerate() {
            let user = register(
                &mut conn,
                &format!("P{i}"),
                &format!("4477009001{i:02}"),
                now,
            )
            .await;
            ids.push(user.id);
            let give = |left: &mut i64, max: i64| {
                let points = (*left).min(max);
                *left -= points;
                points
            };
            let mut left = todos;
            while left > 0 {
                todo_id += 1;
                let points = give(&mut left, 10);
                let award = Award {
                    points,
                    reason: Reason::Done,
                };
                stored_points::award(
                    &mut conn,
                    user.id,
                    chat,
                    Source::Todo,
                    todo_id,
                    award,
                    date("2026-11-02"),
                    now,
                )
                .await
                .unwrap();
            }
            for _ in 0..skips {
                todo_id += 1;
                let award = Award {
                    points: -1,
                    reason: Reason::Skipped,
                };
                stored_points::award(
                    &mut conn,
                    user.id,
                    chat,
                    Source::Todo,
                    todo_id,
                    award,
                    date("2026-11-02"),
                    now,
                )
                .await
                .unwrap();
            }
            // Prayers: up to 5 a day, 2 points each (1 for an odd one out).
            let mut left = prayers;
            let mut n = 0;
            while left > 0 {
                let points = give(&mut left, 2);
                let award = Award {
                    points,
                    reason: if points == 2 {
                        Reason::Prayed
                    } else {
                        Reason::MadeUp
                    },
                };
                let day = date("2026-01-01") + chrono::Duration::days(n / 5);
                stored_points::award(
                    &mut conn,
                    user.id,
                    chat,
                    Source::Prayer,
                    n % 5 + 1,
                    award,
                    day,
                    now,
                )
                .await
                .unwrap();
                n += 1;
            }
        }

        let migration = include_str!("../../migrations/0012_levels_reached.sql");
        let seed = &migration[migration.find("WITH RECURSIVE").expect("0012 has a seed")..];
        sqlx::query("DELETE FROM levels_reached")
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(seed))
            .execute(&mut *conn)
            .await
            .unwrap();

        for id in ids {
            let stats = stats(&mut conn, id).await.unwrap();
            let seeded = reached::reached(&mut conn, id).await.unwrap();
            for track in Track::all() {
                assert_eq!(
                    seeded.get(&track).copied(),
                    Some(stats.level_on(track)),
                    "user {id}, {track:?}: {stats:?}"
                );
            }
        }
    }
}

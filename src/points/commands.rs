//! The points commands: `/points` (the week's leaderboard in a group, the
//! sender's own points in a DM), `/points reports`, and the sender's own
//! history (`/points weeks`, `months`, `year`). `bot` passes each
//! one here. What a todo is worth is `points::for_todo`, and saving it is
//! `points::award`.

use chrono::NaiveDate;
use sqlx::SqliteConnection;

use crate::bot::emoji::BELL_OFF;
use crate::bot::{Context, my_settings};
use crate::error::AppError;
use crate::parser::PointsHistory;
use crate::points;
use crate::points::history::{self, Ledger, Progress};
use crate::points::{level_ups, levels};
use crate::repo::points as stored_points;
use crate::repo::reports as stored_reports;
use crate::reports::{Delivery, Feature, Period, Span};
use crate::settings::Settings;

const CHART: &str = "📊";

/// `/points reports [here|dm|off]`, changed first if a new setting is
/// given. In a group: where its Sunday summary goes. In a DM: whether the
/// monthly and yearly reviews (`points::reviews`) come.
pub async fn reports(
    conn: &mut SqliteConnection,
    ctx: &Context,
    change: Option<Delivery>,
) -> Result<String, AppError> {
    let delivery = match change {
        Some(delivery) => {
            // In a DM, "privately" is the same as "here".
            let delivery = if ctx.is_group || delivery != Delivery::Dm {
                delivery
            } else {
                Delivery::Here
            };
            stored_reports::set_delivery(conn, ctx.chat_id, Feature::Points, delivery).await?;
            delivery
        }
        None => stored_reports::delivery(conn, ctx.chat_id, Feature::Points).await?,
    };
    Ok(if ctx.is_group {
        format_points_reports(delivery)
    } else {
        format_reviews(delivery)
    })
}

/// The `/points reports` reply in a DM: whether the reviews come.
fn format_reviews(delivery: Delivery) -> String {
    match delivery {
        Delivery::Off => format!(
            "{BELL_OFF} Your monthly and yearly points reviews are off.\n\
             Turn them back on with `/points reports on`."
        ),
        Delivery::Here | Delivery::Dm => format!(
            "{CHART} Your points review comes here on the 1st of each month at 8pm: \
             the month against the one before and a year before, the levels you \
             gained, your best week and day, and your prayer record. On 1 January, \
             your year in review too.\nTurn them off with `/points reports off`."
        ),
    }
}

/// `/points`. In a group: this week's leaderboard for the group, Monday to
/// Sunday on the reader's clock. In a DM: the sender's own points from
/// every chat they're in.
pub async fn show(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
) -> Result<String, AppError> {
    let today = my_settings(conn, ctx, defaults).await?.today(ctx.now);
    let week = Period::of(Span::Week, today);
    let just_today = Period {
        first: today,
        last: today,
    };
    // The previous Monday to Sunday: the week that the day before this
    // Monday is in.
    let last_week = Period::of(Span::Week, week.first - chrono::Duration::days(1));
    if ctx.is_group {
        let chat = ctx.chat_id;
        let totals = stored_points::totals_by_user(conn, chat, week).await?;
        // Only needed for the new-week text, when nobody has points yet.
        let last_week = if totals.is_empty() {
            stored_points::totals(conn, chat, last_week).await?
        } else {
            Vec::new()
        };
        let board = points::GroupBoard {
            week,
            today: stored_points::totals(conn, chat, just_today).await?,
            levels: levels::overall_by_person(
                stored_points::xp_of_earners(conn, chat, week).await?,
            ),
            totals,
            last_week,
        };
        return Ok(points::group_text(&board));
    }

    let user = ctx.sender.id;
    let last_7 = Period {
        first: today - chrono::Duration::days(6),
        last: today,
    };
    let sum = |days: &[(NaiveDate, i64)]| days.iter().map(|(_, p)| p).sum::<i64>();

    let earned = stored_points::by_day(conn, user, last_7).await?;
    // Every one of the 7 days, with 0 for a day that earned nothing.
    let last_7_days = last_7
        .days()
        .map(|day| {
            let points = earned
                .iter()
                .find(|(d, _)| *d == day)
                .map_or(0, |(_, p)| *p);
            (day, points)
        })
        .collect();
    let mine = points::Mine {
        last_7_days,
        this_week: sum(&stored_points::by_day(conn, user, week).await?),
        last_week: sum(&stored_points::by_day(conn, user, last_week).await?),
        by_chat: stored_points::by_chat(conn, user, week).await?,
        stats: level_ups::stats(conn, user).await?,
        this_week_by_category: levels::by_category(
            stored_points::by_source(conn, user, week).await?,
        ),
    };
    Ok(points::mine_text(&mine))
}

/// `/points weeks`, `/points months`, `/points year` (or `/points 2025`):
/// the sender's own history, from every chat. Private, so in a group it
/// only says where to ask.
pub async fn show_history(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    which: PointsHistory,
) -> Result<String, AppError> {
    if ctx.is_group {
        return Ok(history::private_text());
    }
    let today = my_settings(conn, ctx, defaults).await?.today(ctx.now);
    let user = ctx.sender.id;
    let ledger = ledger(conn, user, today).await?;
    Ok(match which {
        PointsHistory::Weeks => history::weeks_text(&ledger, today),
        PointsHistory::Months => history::months_text(&ledger, today),
        PointsHistory::Year(year) => {
            let year = year.unwrap_or(chrono::Datelike::year(&today));
            let progress = match history::year(year) {
                Some(whole) if whole.first <= today => {
                    progress(conn, user, whole.first, whole.last.min(today)).await?
                }
                _ => Progress::none(),
            };
            history::year_text(&ledger, year, today, &progress)
        }
    })
}

/// Everything `user_id` has ever earned, up to `today`: their whole
/// history, a few thousand rows at most (see `points_by_user_day`).
pub async fn ledger(
    conn: &mut SqliteConnection,
    user_id: i64,
    today: NaiveDate,
) -> Result<Ledger, sqlx::Error> {
    let Some(first) = stored_points::first_day(conn, user_id).await? else {
        return Ok(Ledger::default());
    };
    let all = Period {
        first: first.min(today),
        last: today,
    };
    Ok(Ledger::from_rows(
        stored_points::by_day_and_source(conn, user_id, all).await?,
    ))
}

/// `user_id`'s stats at the start of `first` and at the end of `last`,
/// for the levels and XP they gained in between.
pub async fn progress(
    conn: &mut SqliteConnection,
    user_id: i64,
    first: NaiveDate,
    last: NaiveDate,
) -> Result<Progress, sqlx::Error> {
    let before = match first.pred_opt() {
        Some(day_before) => stored_points::xp_by_source_until(conn, user_id, day_before).await?,
        None => Vec::new(),
    };
    let after = stored_points::xp_by_source_until(conn, user_id, last).await?;
    Ok(Progress {
        before: levels::Stats::from_sources(before),
        after: levels::Stats::from_sources(after),
    })
}

/// The `/points reports` reply, in a group: where the summary goes now.
fn format_points_reports(delivery: Delivery) -> String {
    let what = "every Sunday at 8pm, the week's points: the leaderboard, how \
                each person did against last week, the champion of each \
                category, and the best day";
    let now = match delivery {
        Delivery::Off => format!("{BELL_OFF} The Sunday points summary is off in this group."),
        Delivery::Here => format!("{CHART} The points summary comes to this group: {what}."),
        Delivery::Dm => format!(
            "{CHART} The points summary goes to each member privately, just their own \
             week. (Only to people who have messaged me privately.)"
        ),
    };
    format!("{now}\nChange with `/points reports here`, `dm` or `off`.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::tests::{setup, typed};
    use crate::db::test_pool;

    /// `/points` in the group at `now` (UTC), as Bilal.
    async fn board_at(conn: &mut SqliteConnection, ctx: &Context, now: &str) -> String {
        let at = Context {
            chat_id: ctx.chat_id,
            is_group: ctx.is_group,
            sender: ctx.sender.clone(),
            now: now.parse().unwrap(),
        };
        typed(conn, &at, "/points").await.into_text()
    }

    #[tokio::test]
    async fn group_totals_start_from_zero_each_monday_on_the_readers_clock() {
        use crate::points::{Award, Reason, Source};
        use crate::testkit::date;

        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        let award = Award {
            points: 3,
            reason: Reason::Done,
        };
        // Points on the Sundays the clocks change in London (the default
        // timezone): 29 March (to BST) and 25 October (back to GMT).
        for (id, day) in [(1, "2026-03-29"), (2, "2026-10-25")] {
            let day = date(day);
            stored_points::award(
                &mut conn,
                ctx.sender.id,
                ctx.chat_id,
                Source::Todo,
                id,
                award,
                day,
                ctx.now,
            )
            .await
            .unwrap();
        }

        // Sunday 23:30 BST: still that week.
        let text = board_at(&mut conn, &ctx, "2026-03-29T22:30:00Z").await;
        assert!(text.contains("🥇 Bilal · Lv 2 · 3"), "{text}");
        // Monday 00:30 BST, though still Sunday in UTC: a new week, with
        // last week's winner.
        let text = board_at(&mut conn, &ctx, "2026-03-29T23:30:00Z").await;
        assert!(text.starts_with("🏁 A new week"), "{text}");
        assert!(text.contains("Bilal (3)"), "{text}");

        // Back to GMT: Sunday 23:30 is 23:30 UTC, and Monday starts at
        // midnight UTC.
        let text = board_at(&mut conn, &ctx, "2026-10-25T23:30:00Z").await;
        assert!(text.contains("🥇 Bilal"), "{text}");
        let text = board_at(&mut conn, &ctx, "2026-10-26T00:30:00Z").await;
        assert!(text.starts_with("🏁 A new week"), "{text}");
    }

    #[tokio::test]
    async fn each_member_on_the_board_shows_their_own_level() {
        use crate::points::{Award, Reason, Source};
        use crate::repo::chats;
        use crate::testkit::register;

        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        // Registered as "Bilal" too: names are unique, so they're shown as
        // "Bilal-0009", but their level must still come from their own XP.
        let other = register(&mut conn, "Bilal", "447700900009", ctx.now).await;
        chats::add_member(&mut conn, ctx.chat_id, other.id, ctx.now)
            .await
            .unwrap();
        let award = Award {
            points: 3,
            reason: Reason::Done,
        };
        let today = crate::settings::tests::defaults().today(ctx.now);
        // The sender earns 3 XP (Todos Lv 1, Overall 2); the other Bilal 60
        // (Todos Lv 3, Overall 4).
        let mut todo_id = 0;
        for (user, times) in [(ctx.sender.id, 1), (other.id, 20)] {
            for _ in 0..times {
                todo_id += 1;
                stored_points::award(
                    &mut conn,
                    user,
                    ctx.chat_id,
                    Source::Todo,
                    todo_id,
                    award,
                    today,
                    ctx.now,
                )
                .await
                .unwrap();
            }
        }

        let text = typed(&mut conn, &ctx, "/points").await.into_text();
        assert!(text.contains("🥇 Bilal-0009 · Lv 4 · 60"), "{text}");
        assert!(text.contains("🥈 Bilal · Lv 2 · 3"), "{text}");
    }

    #[tokio::test]
    async fn history_is_shown_in_a_dm_and_only_pointed_to_in_a_group() {
        use crate::points::{Award, Reason, Source};
        use crate::testkit::{date, utc};

        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let group = setup(&mut conn).await;
        let award = |points| Award {
            points,
            reason: Reason::Done,
        };
        // 3 last week and 5 this week (Fri 13 Nov), in the group.
        for (id, day, points) in [(1, "2026-11-04", 3), (2, "2026-11-12", 5)] {
            stored_points::award(
                &mut conn,
                group.sender.id,
                group.chat_id,
                Source::Todo,
                id,
                award(points),
                date(day),
                group.now,
            )
            .await
            .unwrap();
        }
        let at = |is_group| Context {
            chat_id: group.chat_id,
            is_group,
            sender: group.sender.clone(),
            now: utc("2026-11-13T12:00:00Z"),
        };
        let (in_group, dm) = (at(true), at(false));

        for text in [
            "/points weeks",
            "/points months",
            "/points year",
            "/points 2025",
        ] {
            let reply = typed(&mut conn, &in_group, text).await.into_text();
            assert!(
                reply.starts_with("🔒 Your history is just for you"),
                "{reply}"
            );
        }

        let weeks = typed(&mut conn, &dm, "/points weeks").await.into_text();
        assert!(
            weeks.starts_with(
                "📈 *Your last 2 weeks*
2 Nov"
            ),
            "{weeks}"
        );
        assert!(
            weeks.contains("This week 5 · average 3 ⬆️ · best 5 (w/c 9 Nov)"),
            "{weeks}"
        );
        let months = typed(&mut conn, &dm, "/points months").await.into_text();
        assert!(months.starts_with("📈 *Your first month*"), "{months}");
        let year = typed(&mut conn, &dm, "/points year").await.into_text();
        assert!(
            year.starts_with(
                "🗓️ *Your 2026 so far*
8 points"
            ),
            "{year}"
        );
        // 8 todo XP: still Todos Lv 1, so no levels gained.
        assert!(!year.contains("Levels gained"), "{year}");
        let before = typed(&mut conn, &dm, "/points 2025").await.into_text();
        assert_eq!(before, "🗓️ Nothing earned in 2025.");
    }

    #[tokio::test]
    async fn the_year_shows_the_levels_gained_in_it() {
        use crate::points::{Award, Reason, Source};
        use crate::testkit::{date, utc};

        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let group = setup(&mut conn).await;
        let dm = Context {
            chat_id: group.chat_id,
            is_group: false,
            sender: group.sender.clone(),
            now: utc("2027-03-01T12:00:00Z"),
        };
        // 10 todo XP in 2026, then 10 more in 2027: Todos Lv 1 → 2 (15 XP)
        // in 2027, and Overall 2 → 3.
        let award = Award {
            points: 2,
            reason: Reason::Done,
        };
        for id in 0..10 {
            let day = if id < 5 { "2026-12-01" } else { "2027-01-10" };
            stored_points::award(
                &mut conn,
                dm.sender.id,
                dm.chat_id,
                Source::Todo,
                id,
                award,
                date(day),
                dm.now,
            )
            .await
            .unwrap();
        }
        let this_year = typed(&mut conn, &dm, "/points year").await.into_text();
        assert!(
            this_year.contains("🎉 Levels gained: 📝 Todos 1 → 2 · 🏅 Overall 2 → 3"),
            "{this_year}"
        );
        assert!(this_year.contains("Against 2026: 10, flat"), "{this_year}");
        let last_year = typed(&mut conn, &dm, "/points 2026").await.into_text();
        assert!(
            last_year.starts_with(
                "🗓️ *Your 2026*
10 points"
            ),
            "{last_year}"
        );
        assert!(!last_year.contains("Levels gained"), "{last_year}");
    }

    #[tokio::test]
    async fn points_reports_are_set_per_group() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;

        let reply = typed(&mut conn, &ctx, "/points reports").await.into_text();
        assert!(
            reply.contains("comes to this group"),
            "here by default: {reply}"
        );
        let reply = typed(&mut conn, &ctx, "/points reports dm")
            .await
            .into_text();
        assert!(reply.contains("each member privately"), "{reply}");
        assert_eq!(
            stored_reports::delivery(&mut conn, ctx.chat_id, Feature::Points)
                .await
                .unwrap(),
            Delivery::Dm
        );
        // The prayer reports are a separate setting.
        assert_eq!(
            stored_reports::delivery(&mut conn, ctx.chat_id, Feature::Prayers)
                .await
                .unwrap(),
            Delivery::Here
        );
    }

    #[tokio::test]
    async fn points_reports_in_a_dm_turn_the_reviews_on_and_off() {
        use crate::repo::chats;

        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        let jid = "447700900001@s.whatsapp.net";
        let dm_chat = chats::find_or_create(&mut conn, jid, Some(ctx.sender.id), ctx.now)
            .await
            .unwrap();
        let dm = Context {
            chat_id: dm_chat,
            is_group: false,
            sender: ctx.sender.clone(),
            now: ctx.now,
        };
        let stored = async |conn: &mut SqliteConnection, chat| {
            stored_reports::delivery(conn, chat, Feature::Points)
                .await
                .unwrap()
        };

        let reply = typed(&mut conn, &dm, "/points reports").await.into_text();
        assert!(
            reply.starts_with("📊 Your points review comes here on the 1st"),
            "on by default: {reply}"
        );
        let reply = typed(&mut conn, &dm, "/points reports off")
            .await
            .into_text();
        assert!(reply.contains("reviews are off"), "{reply}");
        assert_eq!(stored(&mut conn, dm_chat).await, Delivery::Off);
        assert_eq!(
            stored(&mut conn, ctx.chat_id).await,
            Delivery::Here,
            "the group's summary is a separate setting"
        );
        // "Privately" in a DM is just "here".
        typed(&mut conn, &dm, "/points reports dm").await;
        assert_eq!(stored(&mut conn, dm_chat).await, Delivery::Here);
    }
}

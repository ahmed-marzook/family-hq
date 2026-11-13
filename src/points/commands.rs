//! The points commands: `/points` (the week's leaderboard in a group, the
//! sender's own points in a DM) and `/points reports`. `bot` passes each
//! one here. What a todo is worth is `points::for_todo`, and saving it is
//! `points::award`.

use chrono::NaiveDate;
use sqlx::SqliteConnection;

use crate::bot::emoji::BELL_OFF;
use crate::bot::{Context, my_settings};
use crate::error::AppError;
use crate::points;
use crate::points::{level_ups, levels};
use crate::repo::points as stored_points;
use crate::repo::reports as stored_reports;
use crate::reports::{Delivery, Feature, Period, Span};
use crate::settings::Settings;

const CHART: &str = "📊";

/// `/points reports [here|dm|off]`: where this group's Sunday summary
/// goes, changed first if a new place is given. Groups only.
pub async fn reports(
    conn: &mut SqliteConnection,
    ctx: &Context,
    change: Option<Delivery>,
) -> Result<String, AppError> {
    if !ctx.is_group {
        return Ok(format!(
            "{CHART} The Sunday points summary is for groups. Here, `/points` shows \
             your own week any time."
        ));
    }
    let delivery = match change {
        Some(delivery) => {
            stored_reports::set_delivery(conn, ctx.chat_id, Feature::Points, delivery).await?;
            delivery
        }
        None => stored_reports::delivery(conn, ctx.chat_id, Feature::Points).await?,
    };
    Ok(format_points_reports(delivery))
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
    if ctx.is_group {
        let totals = stored_points::totals(conn, ctx.chat_id, week).await?;
        let today_totals = stored_points::totals(conn, ctx.chat_id, just_today).await?;
        let xp = stored_points::xp_of_earners(conn, ctx.chat_id, week).await?;
        let levels = levels::overall_by_person(xp);
        return Ok(points::group_text(week, &totals, &today_totals, &levels));
    }

    let user = ctx.sender.id;
    let last_7 = Period {
        first: today - chrono::Duration::days(6),
        last: today,
    };
    // The previous Monday to Sunday: the week that the day before this
    // Monday is in.
    let last_week = Period::of(Span::Week, week.first - chrono::Duration::days(1));
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

/// The `/points reports` reply, in a group: where the summary goes now.
fn format_points_reports(delivery: Delivery) -> String {
    let what = "every Sunday at 8pm, the week's points: the leaderboard, how \
                each person did against last week, and the best day";
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

        let dm = Context {
            chat_id: ctx.chat_id,
            is_group: false,
            sender: ctx.sender.clone(),
            now: ctx.now,
        };
        let reply = typed(&mut conn, &dm, "/points reports off")
            .await
            .into_text();
        assert!(reply.contains("for groups"), "{reply}");
    }
}

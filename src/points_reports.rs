//! The Sunday weekly points summary, in each group where anyone earned
//! points that week: the leaderboard, each person's change from last week,
//! and the best day.
//!
//! * When: Sunday at `SUMMARY_HOUR` (20:00) on the group's own clock, or up
//!   to `reports::LATE_REPORT_GRACE` later if the bot was down just then.
//!   Evening rather than midnight, so nobody is woken by it.
//! * Where: `/points reports here|dm|off` (`Feature::Points`). `dm` sends
//!   each person only their own week, privately.
//! * Groups only. In a DM, `/points` shows your own week any time.
//!
//! Claiming, sending and the grace period are the shared `reports` module's.

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use tracing::warn;

use crate::AppState;
use crate::error::AppError;
use crate::points::{self, DayPoints};
use crate::repo::points::{self as stored_points, ReportGroup};
use crate::repo::reports as stored_reports;
use crate::repo::settings as stored_settings;
use crate::reports::{self, Delivery, Feature, Outgoing, Period, Report, ReportKind, Span, To};

/// The hour on Sunday, local to the group, when the week's summary goes.
pub const SUMMARY_HOUR: u32 = 20;

/// Sends every weekly summary that's due at `now` and not sent yet.
pub async fn check(state: &AppState, now: DateTime<Utc>) -> Result<(), AppError> {
    // Any group with points in the last 8 days: enough to cover this week
    // in every timezone, without looking at groups that went quiet.
    let since = now.date_naive() - Duration::days(8);
    let groups = {
        let mut conn = state.db.acquire().await?;
        stored_points::report_groups(&mut conn, since).await?
    };
    for group in groups {
        // One group's problem must not stop the others' summaries.
        if let Err(err) = check_group(state, &group, now).await {
            warn!("points summary for {} failed: {err}", group.wa_chat_id);
        }
    }
    Ok(())
}

async fn check_group(
    state: &AppState,
    group: &ReportGroup,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    let mut conn = state.db.acquire().await?;
    // The whole group's summary, so the group's clock: no one person's.
    let defaults = &state.apis.prayer_times.defaults;
    let settings = stored_settings::effective(&mut conn, defaults, group.id, None).await?;
    let today = settings.today(now);
    let Some(at) = summary_time(settings.timezone, today) else {
        return Ok(());
    };
    // `due` also lists the day's report (and the month's at a month end);
    // points only has a weekly one. On any day but Sunday this is empty.
    let due = reports::due(today, at, now);
    if !due.contains(&ReportKind::Summary(Span::Week)) {
        return Ok(());
    }
    let delivery = stored_reports::delivery(&mut conn, group.id, Feature::Points).await?;
    let week = Period::of(Span::Week, today);
    let last_week = Period::of(Span::Week, week.first - Duration::days(1));
    let days = stored_points::by_person_day(&mut conn, group.id, week).await?;
    let before = stored_points::totals(&mut conn, group.id, last_week).await?;
    // Back to the pool before sending, which waits on GOWA.
    drop(conn);

    let outgoing = messages(delivery, group.name.as_deref(), week, &days, &before);
    let report = Report {
        feature: Feature::Points,
        kind: ReportKind::Summary(Span::Week),
        date: today,
        chat_id: group.id,
        wa_chat_id: &group.wa_chat_id,
        place: "",
    };
    reports::send(state, &report, outgoing, now).await
}

/// `SUMMARY_HOUR` on `date` in `timezone`, as a moment. `earliest()`
/// because a local time can be missing or happen twice when the clocks
/// change (not at 20:00 in the UK, but it's not worth assuming).
pub fn summary_time(timezone: Tz, date: NaiveDate) -> Option<DateTime<Utc>> {
    let local = date.and_time(NaiveTime::from_hms_opt(SUMMARY_HOUR, 0, 0)?);
    Some(
        timezone
            .from_local_datetime(&local)
            .earliest()?
            .with_timezone(&Utc),
    )
}

/// What a group's weekly summary sends, as `delivery` says:
///
/// * `Here`: one message in the group, about everyone.
/// * `Dm`: one message per person who earned points, to their DM.
/// * `Off`, or nobody earned anything: nothing.
///
/// `last_week` is `(name, points)` for the week before. Pure, so tested
/// directly.
pub fn messages(
    delivery: Delivery,
    group_name: Option<&str>,
    week: Period,
    days: &[DayPoints],
    last_week: &[(String, i64)],
) -> Vec<Outgoing> {
    match delivery {
        Delivery::Off => Vec::new(),
        // `Option::into_iter`: no text → no message, some → one.
        Delivery::Here => points::week_text(week, days, last_week)
            .into_iter()
            .map(|text| Outgoing {
                to: To::Chat,
                text,
                mentions: Vec::new(),
            })
            .collect(),
        Delivery::Dm => points::totals_by_person(days)
            .into_iter()
            .map(|(user_id, name, _)| {
                let theirs: Vec<DayPoints> = days
                    .iter()
                    .filter(|d| d.user_id == user_id)
                    .cloned()
                    .collect();
                let before = last_week.iter().find(|(n, _)| *n == name).map(|(_, p)| *p);
                Outgoing {
                    to: To::Dm(user_id),
                    text: points::own_week_text(group_name, week, &theirs, before),
                    mentions: Vec::new(),
                }
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::date;

    fn week() -> Period {
        Period::of(Span::Week, date("2026-11-08"))
    }

    fn days() -> Vec<DayPoints> {
        let day = |user_id, name: &str, d: &str, points| DayPoints {
            user_id,
            name: name.to_string(),
            day: date(d),
            points,
        };
        vec![
            day(1, "Khadija", "2026-11-03", 6),
            day(2, "Bilal", "2026-11-04", 4),
            day(1, "Khadija", "2026-11-05", 2),
        ]
    }

    #[test]
    fn here_sends_one_leaderboard_to_the_group() {
        let sent = messages(Delivery::Here, Some("Family"), week(), &days(), &[]);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].to, To::Chat);
        assert!(
            sent[0].text.contains("🥇 Khadija 8\n🥈 Bilal 4"),
            "{}",
            sent[0].text
        );
    }

    #[test]
    fn dm_sends_each_person_their_own() {
        let last_week = vec![("Bilal".to_string(), 6)];
        let sent = messages(Delivery::Dm, Some("Family"), week(), &days(), &last_week);
        let to: Vec<&To> = sent.iter().map(|m| &m.to).collect();
        assert_eq!(to, vec![&To::Dm(1), &To::Dm(2)]);
        assert!(sent[0].text.contains("Your week in Family"));
        assert!(sent[0].text.contains("\n8 points\n"), "{}", sent[0].text);
        assert!(!sent[0].text.contains("Bilal"), "only her own");
        assert!(
            sent[1].text.contains("4 points ⬇️ 2 on last week"),
            "{}",
            sent[1].text
        );
    }

    #[test]
    fn off_or_nothing_earned_sends_nothing() {
        assert!(messages(Delivery::Off, None, week(), &days(), &[]).is_empty());
        assert!(messages(Delivery::Here, None, week(), &[], &[]).is_empty());
        assert!(messages(Delivery::Dm, None, week(), &[], &[]).is_empty());
    }

    #[test]
    fn summary_time_is_8pm_local_in_summer_and_winter() {
        let london: Tz = "Europe/London".parse().unwrap();
        // British Summer Time (UTC+1): 19:00 UTC.
        assert_eq!(
            summary_time(london, date("2026-07-05")),
            Some("2026-07-05T19:00:00Z".parse().unwrap())
        );
        // GMT: 20:00 UTC.
        assert_eq!(
            summary_time(london, date("2026-11-08")),
            Some("2026-11-08T20:00:00Z".parse().unwrap())
        );
        let colombo: Tz = "Asia/Colombo".parse().unwrap();
        assert_eq!(
            summary_time(colombo, date("2026-11-08")),
            Some("2026-11-08T14:30:00Z".parse().unwrap())
        );
    }

    #[tokio::test]
    async fn the_summary_goes_once_on_sunday_evening() {
        use crate::db::test_pool;
        use crate::points::{Award, Reason, Source};
        use crate::repo::chats;
        use crate::testkit::register;

        const GROUP: &str = "120363000000000001@g.us";
        let pool = test_pool().await;
        // The shared fake GOWA; `sent` is what it was asked to send.
        let gowa = crate::testkit::fake_gowa().await;
        let sent = gowa.sent.clone();
        let state = crate::testkit::test_state(pool.clone(), &gowa.url);
        {
            let mut conn = pool.acquire().await.unwrap();
            let now = Utc::now();
            let group = chats::find_or_create(&mut conn, GROUP, None, now)
                .await
                .unwrap();
            for (i, (name, points)) in [("Bilal", 3), ("Khadija", 5)].into_iter().enumerate() {
                let user = register(&mut conn, name, &format!("44770090000{i}"), now).await;
                let award = Award {
                    points,
                    reason: Reason::Done,
                };
                let day = date("2026-11-05");
                stored_points::award(
                    &mut conn,
                    user.id,
                    group,
                    Source::Todo,
                    i as i64,
                    award,
                    day,
                    now,
                )
                .await
                .unwrap();
            }
        }

        // Saturday evening, and Sunday before 8pm: nothing.
        for now in ["2026-11-07T20:30:00Z", "2026-11-08T19:59:00Z"] {
            check(&state, now.parse().unwrap()).await.unwrap();
        }
        assert!(sent.lock().unwrap().is_empty());

        // Sunday 8pm in London (GMT): one leaderboard to the group.
        check(&state, "2026-11-08T20:01:00Z".parse().unwrap())
            .await
            .unwrap();
        {
            let sent = sent.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0].0, GROUP);
            assert!(
                sent[0].1.contains("🥇 Khadija 5\n🥈 Bilal 3"),
                "{}",
                sent[0].1
            );
        }

        // A minute later (the next tick): already claimed, not sent again.
        check(&state, "2026-11-08T20:02:00Z".parse().unwrap())
            .await
            .unwrap();
        assert_eq!(sent.lock().unwrap().len(), 1);
    }

    #[test]
    fn due_only_on_sunday_evening_within_the_grace() {
        let london: Tz = "Europe/London".parse().unwrap();
        let week_due = |today: &str, now: &str| {
            let at = summary_time(london, date(today)).unwrap();
            reports::due(date(today), at, now.parse().unwrap())
                .contains(&ReportKind::Summary(Span::Week))
        };
        assert!(week_due("2026-11-08", "2026-11-08T20:00:00Z"));
        assert!(
            week_due("2026-11-08", "2026-11-08T22:59:00Z"),
            "a late restart"
        );
        assert!(!week_due("2026-11-08", "2026-11-08T19:59:00Z"), "too early");
        assert!(!week_due("2026-11-08", "2026-11-08T23:00:00Z"), "too late");
        assert!(
            !week_due("2026-11-07", "2026-11-07T20:30:00Z"),
            "a Saturday"
        );
    }
}

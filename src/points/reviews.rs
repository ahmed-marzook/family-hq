//! The monthly and yearly points reviews (Stage P4, part 4), each in the
//! person's own DM: last month on the 1st, and the year in review on
//! 1 January (as well as December's). What they say is
//! `points::review_text`.
//!
//! * When: the 1st at `SUMMARY_HOUR` (20:00) on the person's own clock,
//!   the same hour as the Sunday summary, or up to
//!   `reports::LATE_REPORT_GRACE` later if the bot was down just then.
//! * Who: everyone with a DM with the bot who earned (or lost) points
//!   that month, from any chat. The bot doesn't start a conversation, so
//!   someone who never messaged it privately gets none.
//! * Opting out: `/points reports off` in the DM (`Feature::Points`, on
//!   the DM itself). Off is still claimed, just with nothing to send.
//!
//! The grace period and the claim are the shared `reports` module's;
//! claiming and sending are the reminder engine's (`reminders::engine`).

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use sqlx::SqliteConnection;
use tracing::warn;

use crate::AppState;
use crate::error::AppError;
use crate::points::commands::{ledger, progress};
use crate::points::history::Ledger;
use crate::points::reports::summary_time;
use crate::points::review_text::{self, PrayerRecord};
use crate::prayer::stats::Summary;
use crate::reminders::engine::{self, Planned};
use crate::repo::points::{self as stored_points, ReviewDm};
use crate::repo::prayers;
use crate::repo::reports as stored_reports;
use crate::repo::settings as stored_settings;
use crate::reports::{self, Delivery, Draft, Feature, Period, Report, ReportKind, Span, To};

/// How far back someone's points are looked for, to decide who may be due
/// a review: a little over a year, so the year in review reaches someone
/// who earned in March but not since.
const LOOK_BACK: Duration = Duration::days(400);

/// Sends every monthly and yearly review that's due at `now` and not sent
/// yet.
pub async fn check(state: &AppState, now: DateTime<Utc>) -> Result<(), AppError> {
    if !near_a_month_start(now.date_naive()) {
        return Ok(());
    }
    let people = {
        let mut conn = state.db.acquire().await?;
        stored_points::review_dms(&mut conn, now.date_naive() - LOOK_BACK).await?
    };
    let mut planned = Vec::new();
    for dm in people {
        // One person's problem must not stop everyone else's review.
        match plan_person(state, &dm, now).await {
            Ok(plans) => planned.extend(plans),
            Err(err) => warn!("points review for {} failed: {err}", dm.wa_chat_id),
        }
    }
    engine::deliver(
        &state.gowa,
        &state.db,
        state.apis.push.as_ref(),
        planned,
        now,
    )
    .await;
    Ok(())
}

/// Whether the UTC date can be the 1st of a month somewhere: the 1st
/// itself, or the day either side. Clocks run from UTC−12 to UTC+14, and a
/// review can go up to `LATE_REPORT_GRACE` after 20:00, so every review
/// falls on one of these. On any other day `check` stops without reading
/// anything.
pub fn near_a_month_start(utc_date: NaiveDate) -> bool {
    utc_date.day() <= 2 || utc_date.succ_opt().is_none_or(|next| next.day() == 1)
}

/// One person's reviews that are due: last month's, and on 1 January the
/// year's as well. Read from the database, nothing sent or claimed.
async fn plan_person(
    state: &AppState,
    dm: &ReviewDm,
    now: DateTime<Utc>,
) -> Result<Vec<Planned>, AppError> {
    // Only for reading; back to the pool before the engine sends anything.
    let mut conn = state.db.acquire().await?;
    let defaults = &state.apis.prayer_times.defaults;
    let settings =
        stored_settings::effective(&mut conn, defaults, dm.chat_id, Some(dm.user_id)).await?;
    let today = settings.today(now);
    // A review covers a period that ended yesterday: `due` is asked about
    // that day, with today's 20:00 as the moment it's over, so it lists the
    // month only on the 1st (and the year only on 1 January).
    let (Some(yesterday), Some(at)) = (today.pred_opt(), summary_time(settings.timezone, today))
    else {
        return Ok(Vec::new());
    };
    let due: Vec<ReportKind> = reports::due(yesterday, at, now)
        .into_iter()
        .filter(|kind| matches!(kind, ReportKind::Summary(Span::Month | Span::Year)))
        .collect();
    if due.is_empty() {
        return Ok(Vec::new());
    }

    let delivery = stored_reports::delivery(&mut conn, dm.chat_id, Feature::Points).await?;
    // Up to yesterday, so a point earned this morning isn't in last
    // month's numbers.
    let ledger = ledger(&mut conn, dm.user_id, yesterday).await?;
    let mut planned = Vec::new();
    for kind in due {
        let text = match delivery {
            Delivery::Off => None,
            // In a DM, "privately" is the same as "here".
            Delivery::Here | Delivery::Dm => {
                review(&mut conn, dm.user_id, kind, yesterday, &ledger).await?
            }
        };
        let drafts = text
            .into_iter()
            .map(|text| Draft {
                to: To::Chat,
                text,
                mentions: Vec::new(),
            })
            .collect();
        let report = Report {
            feature: Feature::Points,
            kind,
            date: yesterday,
            chat_id: dm.chat_id,
            wa_chat_id: &dm.wa_chat_id,
            place: "",
        };
        planned.push(reports::plan(&mut conn, &report, drafts).await?);
    }
    Ok(planned)
}

/// The text of one review of the period that ended on `last_day`, or
/// `None` if there's nothing to say.
async fn review(
    conn: &mut SqliteConnection,
    user_id: i64,
    kind: ReportKind,
    last_day: NaiveDate,
    ledger: &Ledger,
) -> Result<Option<String>, AppError> {
    let period = Period::for_report(kind, last_day);
    let progress = progress(conn, user_id, period.first, period.last).await?;
    Ok(match kind {
        ReportKind::Summary(Span::Year) => {
            review_text::year_review_text(ledger, period.first.year(), &progress)
        }
        _ => {
            let log = prayers::log_between(conn, user_id, period.first, period.last).await?;
            let record = PrayerRecord::of(&Summary::new(period, &log));
            review_text::month_text(ledger, period, &progress, record)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::points::{Award, Reason, Source};
    use crate::reminders::engine::Claim;
    use crate::repo::chats;
    use crate::testkit::{date, register, test_state, utc};

    #[test]
    fn only_days_near_a_month_start_are_looked_at() {
        for day in [
            "2026-10-31",
            "2026-11-01",
            "2026-11-02",
            "2028-02-29",
            "2026-12-31",
        ] {
            assert!(near_a_month_start(date(day)), "{day}");
        }
        for day in ["2026-11-03", "2026-11-15", "2026-11-29", "2028-02-28"] {
            assert!(!near_a_month_start(date(day)), "{day}");
        }
    }

    /// A person with a DM, and points on each `(day, points)`.
    async fn person_with_points(state: &AppState, days: &[(&str, i64)]) -> (ReviewDm, i64) {
        let mut conn = state.db.acquire().await.unwrap();
        let now = utc("2026-01-01T12:00:00Z");
        let sara = register(&mut conn, "Sara", "447700900002", now).await;
        let jid = "447700900002@s.whatsapp.net";
        let dm = chats::find_or_create(&mut conn, jid, Some(sara.id), now)
            .await
            .unwrap();
        for (id, (day, points)) in days.iter().enumerate() {
            let award = Award {
                points: *points,
                reason: Reason::Done,
            };
            stored_points::award(
                &mut conn,
                sara.id,
                dm,
                Source::Todo,
                id as i64,
                award,
                date(day),
                now,
            )
            .await
            .unwrap();
        }
        let dms = stored_points::review_dms(&mut conn, date("2025-01-01"))
            .await
            .unwrap();
        assert_eq!(dms.len(), 1);
        (dms.into_iter().next().unwrap(), sara.id)
    }

    fn claim(dm: &ReviewDm, span: Span, last_day: &str) -> Claim {
        Claim::Report {
            chat_id: dm.chat_id,
            feature: Feature::Points,
            kind: ReportKind::Summary(span),
            date: date(last_day),
            place: String::new(),
        }
    }

    #[tokio::test]
    async fn the_monthly_review_goes_on_the_1st_at_8pm_local() {
        let state = test_state(test_pool().await, "http://localhost:1");
        let (dm, _) = person_with_points(&state, &[("2026-10-12", 3), ("2026-11-01", 2)]).await;

        // 31 October, and 1 November before 8pm (GMT): nothing.
        for now in ["2026-10-31T20:30:00Z", "2026-11-01T19:59:00Z"] {
            let plans = plan_person(&state, &dm, utc(now)).await.unwrap();
            assert!(plans.is_empty(), "{now}");
        }

        // 1 November at 8pm: October's review, without this morning's 2.
        let plans = plan_person(&state, &dm, utc("2026-11-01T20:00:00Z"))
            .await
            .unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].claim, claim(&dm, Span::Month, "2026-10-31"));
        assert_eq!(plans[0].messages.len(), 1);
        assert_eq!(plans[0].messages[0].to, dm.wa_chat_id);
        let text = &plans[0].messages[0].text;
        assert!(
            text.starts_with("🗓️ *Your October 2026*\n3 points\n"),
            "{text}"
        );

        // Too late: past the grace.
        let late = plan_person(&state, &dm, utc("2026-11-01T23:00:00Z"))
            .await
            .unwrap();
        assert!(late.is_empty());
    }

    #[tokio::test]
    async fn on_1_january_the_year_comes_after_december() {
        let state = test_state(test_pool().await, "http://localhost:1");
        let (dm, _) = person_with_points(&state, &[("2026-03-10", 4), ("2026-12-02", 5)]).await;

        let plans = plan_person(&state, &dm, utc("2027-01-01T20:05:00Z"))
            .await
            .unwrap();
        let claims: Vec<&Claim> = plans.iter().map(|p| &p.claim).collect();
        assert_eq!(
            claims,
            [
                &claim(&dm, Span::Month, "2026-12-31"),
                &claim(&dm, Span::Year, "2026-12-31")
            ]
        );
        assert!(
            plans[0].messages[0]
                .text
                .starts_with("🗓️ *Your December 2026*")
        );
        let year = &plans[1].messages[0].text;
        assert!(
            year.starts_with("🎆 *Your 2026 in review*\n9 points\n"),
            "{year}"
        );
    }

    #[tokio::test]
    async fn the_review_is_on_the_persons_own_clock() {
        let state = test_state(test_pool().await, "http://localhost:1");
        let (dm, _) = person_with_points(&state, &[("2026-10-12", 3)]).await;
        {
            let mut conn = state.db.acquire().await.unwrap();
            stored_settings::set_timezone(&mut conn, dm.chat_id, "Asia/Colombo")
                .await
                .unwrap();
        }
        // 8pm in Colombo (UTC+5:30) is 14:30 UTC.
        let early = plan_person(&state, &dm, utc("2026-11-01T14:29:00Z"))
            .await
            .unwrap();
        assert!(early.is_empty());
        let plans = plan_person(&state, &dm, utc("2026-11-01T14:30:00Z"))
            .await
            .unwrap();
        assert_eq!(plans.len(), 1);
    }

    #[tokio::test]
    async fn off_is_claimed_with_nothing_to_send_and_a_quiet_month_says_nothing() {
        let state = test_state(test_pool().await, "http://localhost:1");
        // Points in September only: October was quiet.
        let (dm, _) = person_with_points(&state, &[("2026-09-12", 3)]).await;
        let at = utc("2026-11-01T20:00:00Z");
        let quiet = plan_person(&state, &dm, at).await.unwrap();
        assert_eq!(quiet[0].claim, claim(&dm, Span::Month, "2026-10-31"));
        assert!(quiet[0].messages.is_empty());

        // September's review, with `/points reports off`.
        {
            let mut conn = state.db.acquire().await.unwrap();
            stored_reports::set_delivery(&mut conn, dm.chat_id, Feature::Points, Delivery::Off)
                .await
                .unwrap();
        }
        let off = plan_person(&state, &dm, utc("2026-10-01T19:00:00Z"))
            .await
            .unwrap();
        assert_eq!(off[0].claim, claim(&dm, Span::Month, "2026-09-30"));
        assert!(off[0].messages.is_empty());
    }

    #[tokio::test]
    async fn the_review_goes_once() {
        // The shared fake GOWA; `sent` is what it was asked to send.
        let gowa = crate::testkit::fake_gowa().await;
        let sent = gowa.sent.clone();
        let state = test_state(test_pool().await, &gowa.url);
        let (dm, _) = person_with_points(&state, &[("2026-10-12", 3)]).await;

        check(&state, utc("2026-11-01T20:01:00Z")).await.unwrap();
        check(&state, utc("2026-11-01T20:02:00Z")).await.unwrap();
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, dm.wa_chat_id);
        assert!(
            sent[0].1.starts_with("🗓️ *Your October 2026*"),
            "{}",
            sent[0].1
        );
    }
}

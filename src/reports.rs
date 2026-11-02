//! Reports: the messages that sum up a feature (prayers now, habits later)
//! for a day, a week or a month, and the rules every feature shares.
//!
//! * When they're due: once the feature's day is over (for prayers, when
//!   Isha ends), a day's report; on a Sunday the week's too; on a month's
//!   last day the month's. Only for `LATE_REPORT_GRACE` after that.
//! * Where they go: each chat chooses, per feature (`Delivery`).
//! * Never twice: each one is claimed in `reports_sent` before it's sent.
//!
//! A feature decides what its reports say, as a list of `Outgoing`
//! messages; `send` does the rest.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc, Weekday};

use crate::AppState;
use crate::error::AppError;
use crate::repo::reports;

/// Which feature a report is for. The schema's CHECK allows exactly these
/// (and 'habits', ready for Stage 2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Prayers,
}

impl Feature {
    /// The exact text stored in `report_settings` and `reports_sent`, and
    /// the name in the admin API's paths.
    pub fn as_db_str(self) -> &'static str {
        match self {
            Feature::Prayers => "prayers",
        }
    }
}

impl FromStr for Feature {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        [Feature::Prayers]
            .into_iter()
            .find(|f| f.as_db_str() == s)
            .ok_or_else(|| format!("unknown report feature '{s}'"))
    }
}

/// Where a chat's reports for a feature go (`/prayers reports here|dm|off`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Delivery {
    /// In the chat itself: a group gets everyone's, a DM its owner's.
    #[default]
    Here,
    /// A group's go to each member privately, in their DM with the bot.
    Dm,
    Off,
}

impl Delivery {
    /// The exact text stored in `report_settings.delivery`.
    pub fn as_db_str(self) -> &'static str {
        match self {
            Delivery::Here => "here",
            Delivery::Dm => "dm",
            Delivery::Off => "off",
        }
    }
}

impl FromStr for Delivery {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        [Delivery::Here, Delivery::Dm, Delivery::Off]
            .into_iter()
            .find(|d| d.as_db_str() == s)
            .ok_or_else(|| format!("unknown report delivery '{s}'"))
    }
}

/// What a week or month summary covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    Week,
    Month,
}

/// The reports a chat can be sent, one of each per day, week or month.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportKind {
    /// The day's wrap-up.
    Day,
    /// A week's or a month's summary.
    Summary(Span),
}

impl ReportKind {
    /// The exact text stored in `reports_sent.kind`.
    pub fn as_db_str(self) -> &'static str {
        match self {
            ReportKind::Day => "day",
            ReportKind::Summary(Span::Week) => "week",
            ReportKind::Summary(Span::Month) => "month",
        }
    }
}

/// For logs: "day", "week", "month".
impl fmt::Display for ReportKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.pad(self.as_db_str())
    }
}

/// How long after a day is over its reports can still go out (say the
/// server was restarting just then). Any later and they're skipped, so a
/// restart on Wednesday can't send last Sunday's week, and the first start
/// after an upgrade doesn't send yesterday's.
pub const LATE_REPORT_GRACE: Duration = Duration::hours(3);

/// The reports due at `now` for `date`, whose day is over at `day_over`:
/// the day's, plus the week's on a Sunday and the month's on its last day.
/// Nothing before `day_over`, or once `LATE_REPORT_GRACE` has passed.
///
/// `day_over` can be after midnight on the clock (Isha ends at Islamic
/// midnight), so `date` can be yesterday.
pub fn due(date: NaiveDate, day_over: DateTime<Utc>, now: DateTime<Utc>) -> Vec<ReportKind> {
    if now < day_over || now >= day_over + LATE_REPORT_GRACE {
        return Vec::new();
    }
    let mut due = vec![ReportKind::Day];
    if date.weekday() == Weekday::Sun {
        due.push(ReportKind::Summary(Span::Week));
    }
    // The last day of a month is the one whose next day is the 1st.
    // (`is_none_or`: the very last date chrono can hold counts too.)
    if date.succ_opt().is_none_or(|next| next.day() == 1) {
        due.push(ReportKind::Summary(Span::Month));
    }
    due
}

/// The days a report covers, both ends included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Period {
    pub first: NaiveDate,
    pub last: NaiveDate,
}

impl Period {
    /// The week (Monday to Sunday) or calendar month that `date` is in.
    pub fn of(span: Span, date: NaiveDate) -> Self {
        match span {
            Span::Week => {
                let back = i64::from(date.weekday().num_days_from_monday());
                let first = date - Duration::days(back);
                Period {
                    first,
                    last: first + Duration::days(6),
                }
            }
            Span::Month => {
                // `with_day(1)` can't fail: every month has a 1st. The
                // last day is the day before next month's 1st.
                let first = date.with_day(1).expect("every month has a 1st");
                let next_month = first
                    .checked_add_months(chrono::Months::new(1))
                    .unwrap_or(NaiveDate::MAX);
                Period {
                    first,
                    last: next_month.pred_opt().unwrap_or(next_month),
                }
            }
        }
    }

    /// What a report covers: the day itself, or its week or month.
    pub fn for_report(kind: ReportKind, date: NaiveDate) -> Self {
        match kind {
            ReportKind::Day => Period {
                first: date,
                last: date,
            },
            ReportKind::Summary(span) => Period::of(span, date),
        }
    }

    /// Each day in order. `move` copies `self` into the closure, so the
    /// iterator doesn't borrow the `Period` it came from.
    pub fn days(self) -> impl Iterator<Item = NaiveDate> {
        self.first
            .iter_days()
            .take_while(move |day| *day <= self.last)
    }
}

/// Who a report message goes to.
#[derive(Debug, Clone, PartialEq)]
pub enum To {
    /// The chat the report is for.
    Chat,
    /// One member's DM with the bot (a group's reports, sent privately).
    Dm(i64),
}

/// One message to send.
#[derive(Debug, Clone, PartialEq)]
pub struct Outgoing {
    pub to: To,
    pub text: String,
    /// Phone numbers @mentioned in `text`.
    pub mentions: Vec<String>,
}

/// One report for one chat, as `send` needs it.
#[derive(Debug, Clone)]
pub struct Report<'a> {
    pub feature: Feature,
    pub kind: ReportKind,
    /// The last day it covers.
    pub date: NaiveDate,
    pub chat_id: i64,
    pub wa_chat_id: &'a str,
    /// Which part of a group it's for, if the feature splits them (prayers
    /// do, by place); '' otherwise.
    pub place: &'a str,
}

/// Claims the report, then sends its messages. Nothing is sent if it was
/// already claimed (sent before, or being sent by another check).
///
/// Claimed even when there's nothing to say, so the feature doesn't work
/// it out again every minute for the rest of the grace period.
pub async fn send(
    state: &AppState,
    report: &Report<'_>,
    outgoing: Vec<Outgoing>,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    let Report {
        feature,
        kind,
        date,
        ..
    } = *report;
    {
        // A block, so the connection goes back to the pool before sending.
        let mut conn = state.db.acquire().await?;
        let claimed = reports::claim(
            &mut conn,
            report.chat_id,
            feature,
            kind,
            date,
            report.place,
            now,
        )
        .await?;
        if !claimed {
            return Ok(());
        }
    }

    for message in outgoing {
        let wa_chat_id = match message.to {
            To::Chat => report.wa_chat_id.to_string(),
            To::Dm(user_id) => {
                let mut conn = state.db.acquire().await?;
                match reports::dm_chat(&mut conn, user_id).await? {
                    Some(dm) => dm,
                    // Never messaged the bot privately: the bot doesn't
                    // start a conversation with someone.
                    None => continue,
                }
            }
        };
        // Not retried, like reminders: the claim stays.
        let feature = feature.as_db_str();
        match state
            .gowa
            .send_text(&wa_chat_id, &message.text, None, &message.mentions)
            .await
        {
            Ok(_) => {
                println!("reports: sent the {feature} {kind} report for {date} to {wa_chat_id}")
            }
            Err(err) => {
                println!(
                    "reports: couldn't send the {feature} {kind} report to {wa_chat_id}: {err}"
                )
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn utc(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn delivery_round_trips_and_defaults_to_here() {
        for delivery in [Delivery::Here, Delivery::Dm, Delivery::Off] {
            assert_eq!(delivery.as_db_str().parse::<Delivery>(), Ok(delivery));
        }
        assert!("sometimes".parse::<Delivery>().is_err());
        assert_eq!(Delivery::default(), Delivery::Here, "no row means 'here'");
    }

    #[test]
    fn a_days_report_comes_once_the_day_is_over_and_not_hours_later() {
        // A Tuesday, over at 23:46.
        let tuesday = date("2026-10-27");
        let over = utc("2026-10-27T23:46:00Z");
        assert_eq!(due(tuesday, over, utc("2026-10-27T23:45:59Z")), []);
        assert_eq!(due(tuesday, over, over), [ReportKind::Day]);
        assert_eq!(
            due(tuesday, over, utc("2026-10-28T02:45:59Z")),
            [ReportKind::Day]
        );
        assert_eq!(due(tuesday, over, utc("2026-10-28T02:46:00Z")), []);
    }

    #[test]
    fn sunday_adds_the_week_and_a_months_last_day_adds_the_month() {
        let week = ReportKind::Summary(Span::Week);
        let month = ReportKind::Summary(Span::Month);
        let at = |day: &str, over: &str| due(date(day), utc(over), utc(over));
        // Sunday 1 Nov: the week, but not the month.
        assert_eq!(
            at("2026-11-01", "2026-11-01T23:40:00Z"),
            [ReportKind::Day, week]
        );
        // Saturday 31 Oct: the month, but not the week.
        assert_eq!(
            at("2026-10-31", "2026-10-31T23:40:00Z"),
            [ReportKind::Day, month]
        );
        // Sunday 28 Feb 2027: both.
        assert_eq!(
            at("2027-02-28", "2027-02-28T23:40:00Z"),
            [ReportKind::Day, week, month]
        );
    }

    #[test]
    fn a_day_over_after_midnight_still_reports_as_its_own_day() {
        // A summer Sunday: Isha ends at 00:59 London time on Monday.
        let due = due(
            date("2026-06-28"),
            utc("2026-06-28T23:59:00Z"),
            utc("2026-06-29T00:10:00Z"),
        );
        assert_eq!(due, [ReportKind::Day, ReportKind::Summary(Span::Week)]);
    }

    #[test]
    fn weeks_run_monday_to_sunday_and_months_whole() {
        let week = Period::of(Span::Week, date("2026-10-28"));
        assert_eq!(
            (week.first, week.last),
            (date("2026-10-26"), date("2026-11-01"))
        );
        assert_eq!(Period::of(Span::Week, date("2026-11-01")), week, "Sunday");
        assert_eq!(Period::of(Span::Week, date("2026-10-26")), week, "Monday");
        assert_eq!(week.days().count(), 7);

        let feb = Period::of(Span::Month, date("2028-02-10"));
        assert_eq!(
            (feb.first, feb.last),
            (date("2028-02-01"), date("2028-02-29"))
        );
        let dec = Period::of(Span::Month, date("2026-12-31"));
        assert_eq!(dec.last, date("2026-12-31"), "the year end");
        assert_eq!(dec.days().count(), 31);

        let day = Period::for_report(ReportKind::Day, date("2026-10-28"));
        assert_eq!(
            (day.first, day.last),
            (date("2026-10-28"), date("2026-10-28"))
        );
    }
}

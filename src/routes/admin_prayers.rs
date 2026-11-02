//! Prayer tracking on the admin dashboard: each person's week and month
//! (completion, longest streak, which prayers were missed, a heatmap).
//! Where the reports go is `admin_reports`.
//!
//! Counts and dates only, worked out by `prayer_stats` exactly as the
//! WhatsApp reports are.

use chrono::{NaiveDate, Utc};
use serde::Serialize;
use sqlx::SqliteConnection;

use crate::prayer::Prayer;
use crate::prayer_stats::{Entry, Summary};
use crate::repo::prayers;
use crate::repo::settings as stored;
use crate::reports::{Period, Span};
use crate::settings::Settings;

/// One person's prayers this week and this month, so far.
#[derive(Debug, Serialize)]
pub(crate) struct PrayerView {
    week: SpanView,
    month: SpanView,
}

/// One week or month, as `Summary` has it.
#[derive(Debug, Serialize)]
pub(crate) struct SpanView {
    first: NaiveDate,
    last: NaiveDate,
    /// Prayed, on time or made up.
    prayed: usize,
    /// Prayed, made up or missed. Prayers with no record don't count.
    recorded: usize,
    /// `prayed` out of `recorded`, rounded; 0 with nothing recorded.
    percent: i64,
    /// The most on-time prayers in a row.
    longest_streak: usize,
    /// Every day of the period, for the heatmap.
    days: Vec<DayView>,
    /// How many of each prayer were missed, in day order; only those with
    /// any.
    missed: Vec<PrayerCount>,
    made_up: Vec<PrayerCount>,
}

#[derive(Debug, Serialize)]
struct DayView {
    date: NaiveDate,
    /// How many of the five were prayed; `null` for no record.
    prayed: Option<usize>,
}

#[derive(Debug, Serialize)]
struct PrayerCount {
    /// "Fajr", "Dhuhr", ...
    prayer: String,
    count: usize,
}

impl From<Summary> for SpanView {
    fn from(s: Summary) -> Self {
        SpanView {
            first: s.period.first,
            last: s.period.last,
            prayed: s.prayed,
            recorded: s.recorded,
            percent: crate::progress::percent(s.prayed as i64, s.recorded as i64),
            longest_streak: s.longest_streak,
            days: s
                .period
                .days()
                .zip(&s.days)
                .map(|(date, prayed)| DayView {
                    date,
                    prayed: *prayed,
                })
                .collect(),
            missed: counts(&s.missed),
            made_up: counts(&s.made_up),
        }
    }
}

/// How many entries there are of each prayer, in day order.
fn counts(entries: &[Entry]) -> Vec<PrayerCount> {
    Prayer::ALL
        .into_iter()
        .map(|prayer| PrayerCount {
            prayer: prayer.to_string(),
            count: entries.iter().filter(|e| e.prayer == prayer).count(),
        })
        .filter(|c| c.count > 0)
        .collect()
}

/// `user_id`'s week or month that `today` is in.
pub(crate) async fn span_view(
    conn: &mut SqliteConnection,
    user_id: i64,
    span: Span,
    today: NaiveDate,
) -> Result<SpanView, sqlx::Error> {
    let period = Period::of(span, today);
    let log = prayers::log_between(conn, user_id, period.first, period.last).await?;
    Ok(Summary::new(period, &log).into())
}

/// `user_id`'s week and month. `today` is in their own timezone.
pub(crate) async fn prayer_view(
    conn: &mut SqliteConnection,
    user_id: i64,
    today: NaiveDate,
) -> Result<PrayerView, sqlx::Error> {
    Ok(PrayerView {
        week: span_view(conn, user_id, Span::Week, today).await?,
        month: span_view(conn, user_id, Span::Month, today).await?,
    })
}

/// Today for this person: on their own timezone if they set one,
/// otherwise `base`'s (the server default, or their group's).
pub(crate) async fn today_for(
    conn: &mut SqliteConnection,
    user_id: i64,
    base: &Settings,
) -> Result<NaiveDate, sqlx::Error> {
    let settings = match stored::personal(conn, user_id).await? {
        Some(own) => own.overrides.apply(base),
        None => base.clone(),
    };
    Ok(settings.today(Utc::now()))
}

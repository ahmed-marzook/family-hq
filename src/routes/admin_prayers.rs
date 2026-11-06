//! Prayer tracking on the admin dashboard: each person's week and month
//! (completion, longest streak, which prayers were missed, a heatmap), any
//! month's calendar, and corrections to a day's prayers. Where the reports
//! go is `admin_reports`.
//!
//! Dates and statuses only, worked out by `prayer_stats` exactly as the
//! WhatsApp reports are; never a check-in's time.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;
use tracing::info;

use crate::AppState;
use crate::error::AppError;
use crate::prayer::Prayer;
use crate::prayer_stats::{Entry, Status, Summary};
use crate::repo::settings as stored;
use crate::repo::{admin, prayers};
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
    /// All five in day order, each with how it went.
    prayers: Vec<PrayerMark>,
}

#[derive(Debug, Serialize)]
struct PrayerMark {
    /// "fajr", "dhuhr", ...: the name the edit endpoint takes.
    prayer: &'static str,
    /// "prayed", "late" (made up) or "missed"; `null` for no record.
    status: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct PrayerCount {
    /// "Fajr", "Dhuhr", ...
    prayer: String,
    count: usize,
}

/// A period's view, from its log.
fn span_from(period: Period, log: &[Entry]) -> SpanView {
    let s = Summary::new(period, log);
    let marks = |date: NaiveDate| {
        Prayer::ALL
            .into_iter()
            .map(|prayer| PrayerMark {
                prayer: prayer.as_db_str(),
                status: log
                    .iter()
                    .find(|e| e.date == date && e.prayer == prayer)
                    .map(|e| e.status.as_db_str()),
            })
            .collect()
    };
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
                prayers: marks(date),
            })
            .collect(),
        missed: counts(&s.missed),
        made_up: counts(&s.made_up),
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

/// `user_id`'s week or month that `date` is in.
pub(crate) async fn span_view(
    conn: &mut SqliteConnection,
    user_id: i64,
    span: Span,
    date: NaiveDate,
) -> Result<SpanView, sqlx::Error> {
    let period = Period::of(span, date);
    let log = prayers::log_between(conn, user_id, period.first, period.last).await?;
    Ok(span_from(period, &log))
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

/// One month of someone's prayers, for the calendar.
#[derive(Debug, Serialize)]
pub struct PrayerMonth {
    /// Today in their timezone: the calendar marks it, and nothing after it
    /// can be changed.
    today: NaiveDate,
    month: SpanView,
}

/// `?month=2026-11`; without it, this month.
#[derive(Debug, Deserialize)]
pub struct MonthQuery {
    month: Option<String>,
}

/// "2026-11" → 1 Nov 2026. `None` if it isn't a real month.
fn parse_month(text: &str) -> Option<NaiveDate> {
    let (year, month) = text.split_once('-')?;
    NaiveDate::from_ymd_opt(year.parse().ok()?, month.parse().ok()?, 1)
}

/// The month `date` is in, with `today` for this person.
async fn month_of(
    conn: &mut SqliteConnection,
    state: &AppState,
    id: i64,
    date: Option<NaiveDate>,
) -> Result<PrayerMonth, AppError> {
    // 404 for someone who isn't there.
    admin::user_jid(conn, id).await?.ok_or(AppError::NotFound)?;
    let today = today_for(conn, id, &state.apis.prayer_times.defaults).await?;
    let month = span_view(conn, id, Span::Month, date.unwrap_or(today)).await?;
    Ok(PrayerMonth { today, month })
}

/// `GET /api/users/{id}/prayers?month=2026-11`: one month's calendar.
pub async fn user_month(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(query): Query<MonthQuery>,
) -> Result<Json<PrayerMonth>, AppError> {
    let date = match &query.month {
        Some(text) => Some(parse_month(text).ok_or_else(|| {
            AppError::BadRequest(format!("month must look like 2026-11, not \"{text}\""))
        })?),
        None => None,
    };
    let mut conn = state.db.acquire().await?;
    Ok(Json(month_of(&mut conn, &state, id, date).await?))
}

/// The body of `PUT /api/users/{id}/prayers/{date}/{prayer}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusBody {
    /// "prayed", "late" or "missed"; `null` takes it out of the log.
    status: Option<String>,
}

/// `PUT /api/users/{id}/prayers/2026-11-02/asr` with `{"status": "prayed"}`:
/// corrects one prayer. Answers with that month, so the calendar updates.
/// Not for days after today (in their timezone), and not announced to them.
pub async fn set_prayer(
    State(state): State<Arc<AppState>>,
    Path((id, date, prayer)): Path<(i64, String, String)>,
    Json(body): Json<StatusBody>,
) -> Result<Json<PrayerMonth>, AppError> {
    let date: NaiveDate = date
        .parse()
        .map_err(|_| AppError::BadRequest(format!("\"{date}\" isn't a date like 2026-11-02")))?;
    let prayer: Prayer = prayer.parse().map_err(|_| AppError::NotFound)?;
    let status: Option<Status> = match &body.status {
        Some(text) => Some(text.parse().map_err(|_| {
            AppError::BadRequest(format!(
                "status must be \"prayed\", \"late\", \"missed\" or null, not \"{text}\""
            ))
        })?),
        None => None,
    };

    let mut conn = state.db.acquire().await?;
    admin::user_jid(&mut conn, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let today = today_for(&mut conn, id, &state.apis.prayer_times.defaults).await?;
    if date > today {
        return Err(AppError::BadRequest(
            "that day hasn't happened yet for them".into(),
        ));
    }
    prayers::set_status(&mut conn, id, date, prayer, status, Utc::now()).await?;
    info!(
        "set user {id}'s {prayer} on {date} to {}",
        status.map_or("no record", |s| s.as_db_str())
    );
    Ok(Json(month_of(&mut conn, &state, id, Some(date)).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    #[test]
    fn months_are_parsed_strictly() {
        let first = |y, m| NaiveDate::from_ymd_opt(y, m, 1);
        assert_eq!(parse_month("2026-11"), first(2026, 11));
        assert_eq!(parse_month("2027-1"), first(2027, 1));
        assert_eq!(parse_month("2026-13"), None);
        assert_eq!(parse_month("November"), None);
        assert_eq!(parse_month("2026-11-02"), None);
        // `Datelike` gives `.day()`: always the 1st.
        assert_eq!(parse_month("2026-02").map(|d| d.day()), Some(1));
    }

    // ---- whose settings apply (`today_for`) ----
    //
    // Pinning tests: what the code does today. `today_for` reads the clock
    // itself, so these use two timezones more than 24 hours apart (UTC-11
    // and UTC+14): their dates are never the same, whatever the time is,
    // so "which date came back" says which timezone was used.

    use chrono_tz::Tz;

    use crate::db::test_pool;
    use crate::settings::tests::defaults;
    use crate::testkit::{seed_family, utc};

    /// UTC-11.
    const PAGO_PAGO: Tz = chrono_tz::Pacific::Pago_Pago;
    /// UTC+14.
    const KIRITIMATI: Tz = chrono_tz::Pacific::Kiritimati;

    /// The usual defaults, on Pago Pago's clock.
    fn pago_pago_defaults() -> Settings {
        Settings {
            timezone: PAGO_PAGO,
            ..defaults()
        }
    }

    /// Checks `today_for` gives today on `expected`'s clock, not `other`'s.
    /// The clock is read before and after, in case midnight falls between.
    async fn assert_today_on(
        conn: &mut SqliteConnection,
        user_id: i64,
        base: &Settings,
        expected: Tz,
        other: Tz,
    ) {
        let before = Utc::now();
        let today = today_for(conn, user_id, base).await.unwrap();
        let after = Utc::now();
        let on = |tz: Tz| [before, after].map(|now| now.with_timezone(&tz).date_naive());
        assert!(
            on(expected).contains(&today),
            "{today} isn't today on {expected}"
        );
        assert!(!on(other).contains(&today), "{today} is today on {other}");
    }

    #[tokio::test]
    async fn today_for_uses_personal_timezone_over_the_base() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T12:00:00Z")).await;
        stored::set_timezone(&mut conn, family.dm, "Pacific/Kiritimati")
            .await
            .unwrap();

        let base = pago_pago_defaults();
        assert_today_on(&mut conn, family.bilal.id, &base, KIRITIMATI, PAGO_PAGO).await;
    }

    #[tokio::test]
    async fn today_for_with_the_defaults_ignores_the_groups_timezone() {
        // The user page and the prayer calendar pass the server defaults
        // as `base`, so a group's timezone never counts there, even for
        // someone whose only chat with the bot is that group. In the group
        // itself, `my_settings` would use the group's (Kiritimati).
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T12:00:00Z")).await;
        stored::set_timezone(&mut conn, family.group, "Pacific/Kiritimati")
            .await
            .unwrap();

        let base = pago_pago_defaults();
        assert_today_on(&mut conn, family.bilal.id, &base, PAGO_PAGO, KIRITIMATI).await;
        assert_today_on(&mut conn, family.khadija.id, &base, PAGO_PAGO, KIRITIMATI).await;
    }

    #[tokio::test]
    async fn today_for_with_the_groups_settings_is_the_groups_unless_personal() {
        // The group page passes the group's settings as `base`.
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T12:00:00Z")).await;
        let group = Settings {
            timezone: KIRITIMATI,
            ..defaults()
        };
        // Khadija has no DM; Bilal has one with nothing changed.
        assert_today_on(&mut conn, family.khadija.id, &group, KIRITIMATI, PAGO_PAGO).await;
        assert_today_on(&mut conn, family.bilal.id, &group, KIRITIMATI, PAGO_PAGO).await;

        // Bilal's own timezone wins over the group's.
        stored::set_timezone(&mut conn, family.dm, "Pacific/Pago_Pago")
            .await
            .unwrap();
        assert_today_on(&mut conn, family.bilal.id, &group, PAGO_PAGO, KIRITIMATI).await;
    }

    #[tokio::test]
    async fn today_for_still_uses_a_dm_marked_left() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T12:00:00Z")).await;
        stored::set_timezone(&mut conn, family.dm, "Pacific/Kiritimati")
            .await
            .unwrap();
        // The bot only ever marks groups as left; done by hand here.
        let left = Some(utc("2026-11-01T13:00:00Z"));
        crate::repo::cleanup::set_left(&mut conn, family.dm, left)
            .await
            .unwrap();

        let base = pago_pago_defaults();
        assert_today_on(&mut conn, family.bilal.id, &base, KIRITIMATI, PAGO_PAGO).await;
    }

    // ---- the admin calendar, for someone with no chat in the request ----
    //
    // The calendar has a person but no chat, so it uses their one active
    // group, if they're in exactly one, and the defaults otherwise. These
    // go through the real API, with the server's defaults on Pago Pago's
    // clock and the group on Kiritimati's (always a day or two ahead).

    use crate::repo::chats;
    use crate::testkit::{
        ADMIN_PASSWORD, ADMIN_USER, admin_get, fake_gowa, serve_state, test_state,
    };

    /// A second group, for people in more than one.
    const OTHER_GROUP: &str = "120363000000000002@g.us";

    /// The real app on `pool`, with Pago Pago defaults. `Arc::get_mut`
    /// works because nothing else holds the state yet.
    async fn serve_on_pago_pago(pool: sqlx::SqlitePool) -> String {
        let mut state = test_state(pool, &fake_gowa().await.url);
        let inner = Arc::get_mut(&mut state).expect("not shared yet");
        inner.config.defaults = pago_pago_defaults();
        inner.apis.prayer_times = crate::aladhan::PrayerTimes::new(pago_pago_defaults());
        serve_state(state).await
    }

    /// Today on `tz`'s clock, read before and after a request.
    fn today_on(tz: Tz, before: chrono::DateTime<Utc>) -> [NaiveDate; 2] {
        [before, Utc::now()].map(|now| now.with_timezone(&tz).date_naive())
    }

    /// The calendar's `today` for `user_id`, and when the clock was read.
    async fn calendar_today(server: &str, user_id: i64) -> (NaiveDate, chrono::DateTime<Utc>) {
        let before = Utc::now();
        let (status, page) = admin_get(server, &format!("/api/users/{user_id}/prayers")).await;
        assert_eq!(status, 200, "{page}");
        (page["today"].as_str().unwrap().parse().unwrap(), before)
    }

    #[tokio::test]
    async fn admin_calendar_uses_the_one_group_timezone() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T12:00:00Z")).await;
        stored::set_timezone(&mut conn, family.group, "Pacific/Kiritimati")
            .await
            .unwrap();
        // Another group the bot has left doesn't count.
        let old = chats::find_or_create(&mut conn, OTHER_GROUP, None, utc("2026-11-01T12:00:00Z"))
            .await
            .unwrap();
        chats::add_member(
            &mut conn,
            old,
            family.khadija.id,
            utc("2026-11-01T12:00:00Z"),
        )
        .await
        .unwrap();
        let left = Some(utc("2026-11-01T13:00:00Z"));
        crate::repo::cleanup::set_left(&mut conn, old, left)
            .await
            .unwrap();
        drop(conn);
        let server = serve_on_pago_pago(pool).await;

        // Khadija has no settings of her own: the group's clock.
        let (today, before) = calendar_today(&server, family.khadija.id).await;
        assert!(today_on(KIRITIMATI, before).contains(&today), "{today}");
    }

    #[tokio::test]
    async fn admin_calendar_with_several_groups_uses_the_defaults() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T12:00:00Z")).await;
        stored::set_timezone(&mut conn, family.group, "Pacific/Kiritimati")
            .await
            .unwrap();
        let other =
            chats::find_or_create(&mut conn, OTHER_GROUP, None, utc("2026-11-01T12:00:00Z"))
                .await
                .unwrap();
        stored::set_timezone(&mut conn, other, "Pacific/Kiritimati")
            .await
            .unwrap();
        chats::add_member(
            &mut conn,
            other,
            family.khadija.id,
            utc("2026-11-01T12:00:00Z"),
        )
        .await
        .unwrap();
        drop(conn);
        let server = serve_on_pago_pago(pool).await;

        // Two groups: no single one to pick, so the defaults.
        let (today, before) = calendar_today(&server, family.khadija.id).await;
        assert!(today_on(PAGO_PAGO, before).contains(&today), "{today}");
    }

    #[tokio::test]
    async fn admin_can_correct_a_prayer_on_the_groups_today() {
        // The bot logs Khadija's ticks on the group's date, so the admin
        // must be able to correct that date, even when it's already
        // "tomorrow" on the server's default clock.
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T12:00:00Z")).await;
        stored::set_timezone(&mut conn, family.group, "Pacific/Kiritimati")
            .await
            .unwrap();
        drop(conn);
        let server = serve_on_pago_pago(pool).await;

        let today = Utc::now().with_timezone(&KIRITIMATI).date_naive();
        let response = reqwest::Client::new()
            .put(format!(
                "{server}/api/users/{}/prayers/{today}/asr",
                family.khadija.id
            ))
            .basic_auth(ADMIN_USER, Some(ADMIN_PASSWORD))
            .json(&serde_json::json!({ "status": "prayed" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    }
}

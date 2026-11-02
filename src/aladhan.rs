//! Prayer times from the AlAdhan API (<https://aladhan.com/prayer-times-api>).

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::error::AppError;
use chrono_tz::Tz;

use crate::geo::Coordinates;
use crate::prayer::{DayTimes, HijriDate, Location, Prayer, Window};
use crate::settings::Settings;

/// Fetches prayer times and remembers them per location and day, so the
/// reminder loop (every minute, for every chat) and `/prayers` call the
/// API at most once per location per day.
pub struct PrayerTimes {
    http: reqwest::Client,
    base_url: String,
    /// The server-wide settings (`.env`), for chats that haven't chosen
    /// their own.
    pub defaults: Settings,
    /// Fetched days. A `tokio::sync::Mutex` (not `std::sync`) because it's
    /// held across an `.await` (the HTTP call): a std mutex held over an
    /// await can block the whole thread, Tokio's can't.
    cache: Mutex<HashMap<(Location, NaiveDate), DayTimes>>,
}

impl PrayerTimes {
    pub fn new(defaults: Settings) -> Self {
        PrayerTimes {
            http: reqwest::Client::new(),
            base_url: "https://api.aladhan.com/v1".to_string(),
            defaults,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// For tests: a source that already holds `day` for the default
    /// location, so asking for that never touches the network.
    #[cfg(test)]
    pub(crate) fn with_cached(defaults: Settings, day: DayTimes) -> Self {
        Self::with_cached_days(defaults, vec![day])
    }

    /// The same, with several days in memory.
    #[cfg(test)]
    pub(crate) fn with_cached_days(defaults: Settings, days: Vec<DayTimes>) -> Self {
        let at = defaults
            .coordinates
            .expect("test defaults have coordinates");
        let location = defaults.prayer_location(at);
        let cache = days
            .into_iter()
            .map(|day| ((location.clone(), day.date), day))
            .collect();
        let source = PrayerTimes::new(defaults);
        PrayerTimes {
            cache: Mutex::new(cache),
            ..source
        }
    }

    /// The times at `location` for `date` (a day in the location's
    /// timezone), fetched only if they aren't cached yet.
    pub async fn for_date(
        &self,
        location: &Location,
        date: NaiveDate,
    ) -> Result<DayTimes, AppError> {
        // `lock().await` waits its turn; the guard unlocks when dropped at
        // the end of this function.
        let mut cache = self.cache.lock().await;
        let key = (location.clone(), date);
        if let Some(day) = cache.get(&key) {
            return Ok(day.clone());
        }

        let day = self.fetch(location, date).await?;
        let summary: Vec<String> = day
            .windows
            .iter()
            .map(|w| {
                let start = w.start.with_timezone(&location.timezone);
                format!("{} {}", w.prayer, start.format("%H:%M"))
            })
            .collect();
        println!(
            "prayer times for {}, {} (method {}) on {date}: {}",
            location.city,
            location.country,
            location.method,
            summary.join(", ")
        );

        // Forget days before yesterday, so the cache can't grow forever.
        // `retain` keeps only the entries the closure says yes to.
        if let Some(yesterday) = date.pred_opt() {
            cache.retain(|(_, day), _| *day >= yesterday);
        }
        cache.insert(key, day.clone());
        Ok(day)
    }

    /// Always by coordinates (`/timings`), never by city name
    /// (`/timingsByCity`): the by-name lookup goes through AlAdhan's own
    /// geocoder, which is rate-limited and answers 503 for towns it hasn't
    /// seen before. Coordinates need no lookup.
    async fn fetch(&self, loc: &Location, date: NaiveDate) -> Result<DayTimes, AppError> {
        let at = loc.coordinates();
        let response: ApiResponse = self
            .http
            .get(format!(
                "{}/timings/{}",
                self.base_url,
                date.format("%d-%m-%Y")
            ))
            .query(&[
                ("latitude", at.latitude.to_string()),
                ("longitude", at.longitude.to_string()),
                ("method", loc.method.to_string()),
                ("school", loc.school.to_string()),
                ("timezonestring", loc.timezone.name().to_string()),
                ("iso8601", "true".to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let mut day = to_day_times(date, &response.data.timings).map_err(AppError::PrayerTimes)?;
        day.hijri = response.data.date.as_ref().and_then(ApiDate::hijri);
        Ok(day)
    }

    /// The timezone at `at`, as AlAdhan works it out from the coordinates
    /// (no geocoding involved). Used for a shared location.
    ///
    /// `date` only has to be a real day; today is the natural choice.
    pub async fn timezone_at(&self, at: Coordinates, date: NaiveDate) -> Result<Tz, AppError> {
        let response: ApiResponse = self
            .http
            .get(format!(
                "{}/timings/{}",
                self.base_url,
                date.format("%d-%m-%Y")
            ))
            // No `timezonestring`: AlAdhan then works it out from the point.
            .query(&[
                ("latitude", at.latitude.to_string()),
                ("longitude", at.longitude.to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let name = response.data.meta.timezone;
        name.parse().map_err(|_| {
            AppError::PrayerTimes(format!("AlAdhan gave an unknown timezone {name:?}"))
        })
    }
}

/// The part of AlAdhan's reply we use:
/// `{"code":200,"data":{"timings":{"Fajr":"2026-10-27T04:55:00+00:00",...}}}`.
#[derive(Deserialize)]
struct ApiResponse {
    data: ApiData,
}

#[derive(Deserialize)]
struct ApiData {
    timings: ApiTimings,
    meta: ApiMeta,
    /// `Option`, so a reply without it still gives prayer times.
    date: Option<ApiDate>,
}

/// `"date":{"hijri":{"day":"16","month":{"number":5,...},"year":"1448"}}`.
/// AlAdhan sends the day and year as strings but the month as a number.
#[derive(Deserialize)]
struct ApiDate {
    hijri: ApiHijri,
}

#[derive(Deserialize)]
struct ApiHijri {
    day: String,
    month: ApiHijriMonth,
    year: String,
}

#[derive(Deserialize)]
struct ApiHijriMonth {
    number: u8,
}

impl ApiDate {
    /// The Hijri date, or `None` if any part doesn't make sense. It's only
    /// shown in `/prayers`, so a bad one is left out rather than an error.
    fn hijri(&self) -> Option<HijriDate> {
        let h = &self.hijri;
        let date = HijriDate {
            day: h.day.parse().ok()?,
            month: h.month.number,
            year: h.year.parse().ok()?,
        };
        let valid = (1..=30).contains(&date.day) && (1..=12).contains(&date.month);
        valid.then_some(date)
    }
}

/// `"meta":{"timezone":"Europe/London",...}`: the timezone AlAdhan says the
/// city is in, when we don't tell it one.
#[derive(Deserialize)]
struct ApiMeta {
    timezone: String,
}

/// `rename_all = "PascalCase"`: the JSON keys are `Fajr`, `Sunrise`, ...
/// while Rust fields are lower case. Imsak, Sunset etc. are ignored.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ApiTimings {
    fajr: String,
    /// When Fajr's time ends.
    sunrise: String,
    dhuhr: String,
    asr: String,
    maghrib: String,
    isha: String,
    /// Islamic midnight, when Isha's time ends. In summer it's after
    /// 00:00, and AlAdhan dates it on the next day.
    midnight: String,
}

/// Turns AlAdhan's ISO 8601 times into a `DayTimes`. Pure, so it's
/// tested without the network.
fn to_day_times(date: NaiveDate, t: &ApiTimings) -> Result<DayTimes, String> {
    let parse = |name: &str, text: &str| {
        DateTime::parse_from_rfc3339(text)
            .map(|time| time.with_timezone(&Utc))
            .map_err(|e| format!("bad {name} time {text:?}: {e}"))
    };
    let fajr = parse("Fajr", &t.fajr)?;
    let sunrise = parse("Sunrise", &t.sunrise)?;
    let dhuhr = parse("Dhuhr", &t.dhuhr)?;
    let asr = parse("Asr", &t.asr)?;
    let maghrib = parse("Maghrib", &t.maghrib)?;
    let isha = parse("Isha", &t.isha)?;
    let mut midnight = parse("Midnight", &t.midnight)?;
    // Safety net in case midnight ever comes back dated on the same day.
    if midnight <= isha {
        midnight += chrono::Duration::days(1);
    }

    let window = |prayer, start, end| Window { prayer, start, end };
    Ok(DayTimes {
        date,
        hijri: None,
        windows: [
            window(Prayer::Fajr, fajr, sunrise),
            window(Prayer::Dhuhr, dhuhr, asr),
            window(Prayer::Asr, asr, maghrib),
            window(Prayer::Maghrib, maghrib, isha),
            window(Prayer::Isha, isha, midnight),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed from a real reply for Milton Keynes, method 3, 27-10-2026.
    const REPLY: &str = r#"{"code":200,"status":"OK","data":{"timings":{
        "Fajr":"2026-10-27T04:55:00+00:00","Sunrise":"2026-10-27T06:49:00+00:00",
        "Dhuhr":"2026-10-27T11:47:00+00:00","Asr":"2026-10-27T14:16:00+00:00",
        "Sunset":"2026-10-27T16:44:00+00:00","Maghrib":"2026-10-27T16:44:00+00:00",
        "Isha":"2026-10-27T18:31:00+00:00","Imsak":"2026-10-27T04:45:00+00:00",
        "Midnight":"2026-10-27T23:46:00+00:00"},
        "date":{"hijri":{"date":"16-05-1448","day":"16","year":"1448",
            "month":{"number":5,"en":"Jumādá al-ūlá","days":30}}},
        "meta":{"timezone":"Europe/London"}}}"#;

    fn utc(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn parse(reply: &ApiResponse) -> Result<DayTimes, String> {
        let date = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();
        to_day_times(date, &reply.data.timings)
    }

    #[test]
    fn each_prayer_ends_when_the_next_time_begins() {
        let reply: ApiResponse = serde_json::from_str(REPLY).unwrap();
        let day = parse(&reply).unwrap();
        let w = &day.windows;

        assert_eq!(w[0].prayer, Prayer::Fajr);
        assert_eq!(
            (w[0].start, w[0].end),
            (utc("2026-10-27T04:55:00Z"), utc("2026-10-27T06:49:00Z"))
        );
        assert_eq!(w[1].end, w[2].start, "Dhuhr ends when Asr starts");
        assert_eq!(w[2].end, w[3].start, "Asr ends at Maghrib");
        assert_eq!(w[3].end, w[4].start, "Maghrib ends at Isha");
        assert_eq!(
            w[4].end,
            utc("2026-10-27T23:46:00Z"),
            "Isha ends at midnight"
        );
    }

    #[test]
    fn summer_times_after_midnight_are_on_the_next_day() {
        let mut reply: ApiResponse = serde_json::from_str(REPLY).unwrap();
        reply.data.timings.isha = "2026-06-21T23:30:00+01:00".to_string();
        reply.data.timings.midnight = "2026-06-22T01:05:00+01:00".to_string();
        let isha = parse(&reply).unwrap().windows[4];
        assert_eq!(
            isha.start,
            utc("2026-06-21T22:30:00Z"),
            "BST converted to UTC"
        );
        assert_eq!(isha.end, utc("2026-06-22T00:05:00Z"));
    }

    #[test]
    fn midnight_dated_on_the_same_day_is_moved_to_the_next() {
        let mut reply: ApiResponse = serde_json::from_str(REPLY).unwrap();
        reply.data.timings.midnight = "2026-10-27T00:05:00+00:00".to_string();
        let isha = parse(&reply).unwrap().windows[4];
        assert_eq!(isha.end, utc("2026-10-28T00:05:00Z"));
    }

    #[test]
    fn a_bad_time_is_an_error_naming_it() {
        let mut reply: ApiResponse = serde_json::from_str(REPLY).unwrap();
        reply.data.timings.sunrise = "06:49".to_string();
        let err = parse(&reply).unwrap_err();
        assert!(err.contains("Sunrise"), "{err}");
    }

    #[test]
    fn hijri_date_is_read_from_the_reply() {
        let reply: ApiResponse = serde_json::from_str(REPLY).unwrap();
        let hijri = reply.data.date.as_ref().and_then(ApiDate::hijri);
        assert_eq!(
            hijri,
            Some(HijriDate {
                day: 16,
                month: 5,
                year: 1448
            })
        );
    }

    #[test]
    fn a_bad_or_missing_hijri_date_is_left_out() {
        let mut reply: ApiResponse = serde_json::from_str(REPLY).unwrap();
        let date = reply.data.date.as_mut().unwrap();
        date.hijri.month.number = 13;
        assert_eq!(date.hijri(), None, "month 13");
        date.hijri.month.number = 5;
        date.hijri.day = "first".to_string();
        assert_eq!(date.hijri(), None, "day isn't a number");

        let without = REPLY.replace(r#""date":"#, r#""unused":"#);
        let reply: ApiResponse = serde_json::from_str(&without).unwrap();
        assert!(reply.data.date.is_none(), "still parses without it");
    }

    #[tokio::test]
    async fn cached_day_is_returned_without_fetching() {
        let defaults = Settings {
            city: "Nowhere".to_string(),
            country: "ZZ".to_string(),
            ..crate::settings::tests::defaults()
        };
        let nowhere = defaults.prayer_location(defaults.coordinates.unwrap());
        let reply: ApiResponse = serde_json::from_str(REPLY).unwrap();
        let day = parse(&reply).unwrap();
        let source = PrayerTimes::with_cached(defaults, day.clone());

        // "Nowhere, ZZ" would fail if it went to the network.
        assert_eq!(source.for_date(&nowhere, day.date).await.unwrap(), day);
    }
}

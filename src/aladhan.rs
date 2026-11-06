//! Prayer times from the AlAdhan API (<https://aladhan.com/prayer-times-api>).

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use tracing::info;

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
    /// Fetched days. A plain `std::sync::Mutex` (not Tokio's): it's only
    /// ever held for a quick look or insert, never across an `.await`, so
    /// it can't stall the thread while waiting on the network. Tokio's
    /// async mutex is only needed when a lock must stay held over an await,
    /// and it's slower. The compiler helps keep it that way: a std guard
    /// isn't `Send`, so holding one over an await in a spawned task fails
    /// to build.
    cache: Mutex<HashMap<(Location, NaiveDate), DayTimes>>,
}

impl PrayerTimes {
    pub fn new(defaults: Settings) -> Self {
        PrayerTimes {
            http: crate::http::client(),
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
        let key = (location.clone(), date);
        // Look in the cache, then let go of the lock *before* fetching.
        // Holding it over the HTTP call would make every other lookup,
        // even a cached one for another place, wait for that one fetch.
        // `self.cache()` gives a guard that unlocks when it's dropped,
        // which here is the end of this statement.
        if let Some(day) = self.cache().get(&key) {
            return Ok(day.clone());
        }

        // No lock held here. Two lookups for the same uncached day can now
        // both fetch it; that's rare (only the first minute of a new day)
        // and harmless, since the second insert just writes the same times.
        let day = self.fetch(location, date).await?;
        let summary: Vec<String> = day
            .windows
            .iter()
            .map(|w| {
                let start = w.start.with_timezone(&location.timezone);
                format!("{} {}", w.prayer, start.format("%H:%M"))
            })
            .collect();
        info!(
            "prayer times for {}, {} (method {}) on {date}: {}",
            location.city,
            location.country,
            location.method,
            summary.join(", ")
        );

        // Lock again, briefly, to store it.
        let mut cache = self.cache();
        // Forget this location's days before the one before `date`, so the
        // cache can't grow forever. Only this location's: other places can
        // be on a different day (another timezone), or a `/prayers` lookup
        // may ask for a future date, and neither should throw away days
        // the reminder loop still needs elsewhere.
        // `retain` keeps only the entries the closure says yes to.
        if let Some(yesterday) = date.pred_opt() {
            cache.retain(|(loc, day), _| loc != location || *day >= yesterday);
        }
        cache.insert(key, day.clone());
        Ok(day)
    }

    /// Locks the cache. A std mutex is "poisoned" if a thread panicked
    /// while holding it, in case the data was left half-changed. Nothing
    /// here can be left half-changed (a map lookup, `retain`, `insert`), so
    /// `PoisonError::into_inner` just carries on with the data instead of
    /// panicking too.
    fn cache(&self) -> MutexGuard<'_, HashMap<(Location, NaiveDate), DayTimes>> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Always by coordinates (`/timings`), never by city name
    /// (`/timingsByCity`): the by-name lookup goes through AlAdhan's own
    /// geocoder, which is rate-limited and answers 503 for towns it hasn't
    /// seen before. Coordinates need no lookup.
    async fn fetch(&self, loc: &Location, date: NaiveDate) -> Result<DayTimes, AppError> {
        let at = loc.coordinates();
        let response: ApiResponse = crate::http::get_json(
            self.http
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
                ]),
        )
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
        let response: ApiResponse = crate::http::get_json(
            self.http
                .get(format!(
                    "{}/timings/{}",
                    self.base_url,
                    date.format("%d-%m-%Y")
                ))
                // No `timezonestring`: AlAdhan then works it out from the point.
                .query(&[
                    ("latitude", at.latitude.to_string()),
                    ("longitude", at.longitude.to_string()),
                ]),
        )
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

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

    /// A fake AlAdhan that answers every request with `REPLY`, after
    /// waiting `delay`. Returns its base URL and a count of the requests
    /// it has had.
    ///
    /// The counter is an `Arc<AtomicUsize>`: `Arc` so the test and the
    /// server (running in its own task) can both hold it, and an atomic
    /// number so both can touch it without a lock.
    async fn fake_aladhan(delay: Duration) -> (String, Arc<AtomicUsize>) {
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        // `fallback` answers every path, so `/timings/27-10-2026` and any
        // other date all get the same reply.
        let app = axum::Router::new().fallback(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(delay).await;
                REPLY
            }
        });
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), requests)
    }

    /// The reply's day, moved to `date`.
    fn day_on(date: NaiveDate) -> DayTimes {
        let reply: ApiResponse = serde_json::from_str(REPLY).unwrap();
        DayTimes {
            date,
            ..parse(&reply).unwrap()
        }
    }

    /// Somewhere far from the test defaults, in another timezone.
    fn auckland() -> Location {
        let at = Coordinates {
            latitude: -36.8485,
            longitude: 174.7633,
        };
        Location::new("Auckland", "NZ", at, 3, 0, chrono_tz::Pacific::Auckland)
    }

    #[tokio::test]
    async fn cache_keeps_yesterday_when_another_timezone_asks_for_tomorrow() {
        let defaults = crate::settings::tests::defaults();
        let london = defaults.prayer_location(defaults.coordinates.unwrap());
        let today = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();
        let yesterday = today.pred_opt().unwrap();
        let (url, requests) = fake_aladhan(Duration::ZERO).await;
        let source = PrayerTimes {
            base_url: url,
            ..PrayerTimes::with_cached_days(defaults, vec![day_on(yesterday), day_on(today)])
        };

        // Auckland is already a day or more ahead; this asks for two days on.
        let later = today + chrono::Duration::days(2);
        source.for_date(&auckland(), later).await.unwrap();
        // The reminder loop asks for London's yesterday every minute.
        source.for_date(&london, yesterday).await.unwrap();

        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "only Auckland is fetched; London's days stay cached"
        );
    }

    #[tokio::test]
    async fn a_slow_fetch_does_not_block_cached_lookups() {
        let defaults = crate::settings::tests::defaults();
        let london = defaults.prayer_location(defaults.coordinates.unwrap());
        let today = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();
        let (url, _) = fake_aladhan(Duration::from_secs(2)).await;
        // `Arc` so the spawned task and this test can share one source.
        let source = Arc::new(PrayerTimes {
            base_url: url,
            ..PrayerTimes::with_cached(defaults, day_on(today))
        });

        let slow = {
            let source = source.clone();
            tokio::spawn(async move { source.for_date(&auckland(), today).await })
        };
        // Give the slow lookup time to start its fetch.
        tokio::time::sleep(Duration::from_millis(100)).await;

        let cached =
            tokio::time::timeout(Duration::from_millis(200), source.for_date(&london, today)).await;
        slow.abort();
        assert!(
            cached.is_ok(),
            "a cached lookup waited behind another place's fetch"
        );
    }
}

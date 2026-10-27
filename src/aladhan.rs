//! Prayer times from the AlAdhan API (<https://aladhan.com/prayer-times-api>).

use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::error::AppError;
use crate::prayer::{DayTimes, Prayer};

/// Where to calculate prayer times for, and how.
#[derive(Debug, Clone)]
pub struct Location {
    pub city: String,
    /// Country name or 2-letter code, e.g. "GB".
    pub country: String,
    /// AlAdhan calculation method: 3 = Muslim World League.
    pub method: u8,
    /// Asr calculation: 0 = standard (Shafi'i), 1 = Hanafi.
    pub school: u8,
}

/// Fetches prayer times and remembers the most recent day, so the
/// reminder loop (every minute) and `/prayers` don't call the API each time.
pub struct PrayerTimes {
    http: reqwest::Client,
    base_url: String,
    pub location: Location,
    /// The last day fetched. A `tokio::sync::Mutex` (not `std::sync`)
    /// because it's held across an `.await` (the HTTP call): a std mutex
    /// held over an await can block the whole thread, Tokio's can't.
    cached: Mutex<Option<DayTimes>>,
}

impl PrayerTimes {
    pub fn new(location: Location) -> Self {
        PrayerTimes {
            http: reqwest::Client::new(),
            base_url: "https://api.aladhan.com/v1".to_string(),
            location,
            cached: Mutex::new(None),
        }
    }

    /// For tests: a source that already holds `day`, so asking for that
    /// date never touches the network.
    #[cfg(test)]
    pub(crate) fn with_cached(location: Location, day: DayTimes) -> Self {
        let source = PrayerTimes::new(location);
        PrayerTimes {
            cached: Mutex::new(Some(day)),
            ..source
        }
    }

    /// The times for `date` (a Europe/London day), from the cache if it's
    /// the day we fetched last.
    pub async fn for_date(&self, date: NaiveDate) -> Result<DayTimes, AppError> {
        // `lock().await` waits its turn; the guard unlocks when dropped at
        // the end of this function.
        let mut cached = self.cached.lock().await;
        if let Some(day) = cached.as_ref().filter(|day| day.date == date) {
            return Ok(day.clone());
        }

        let day = self.fetch(date).await?;
        let summary: Vec<String> = day
            .times
            .iter()
            .map(|(prayer, start)| format!("{prayer} {}", crate::prayer::london_hhmm(*start)))
            .collect();
        println!("prayer times for {date}: {}", summary.join(", "));

        *cached = Some(day.clone());
        Ok(day)
    }

    async fn fetch(&self, date: NaiveDate) -> Result<DayTimes, AppError> {
        let loc = &self.location;
        let response: ApiResponse = self
            .http
            .get(format!(
                "{}/timingsByCity/{}",
                self.base_url,
                date.format("%d-%m-%Y")
            ))
            // `.query` builds `?city=Milton%20Keynes&...`, escaping spaces.
            .query(&[
                ("city", loc.city.as_str()),
                ("country", loc.country.as_str()),
                ("method", &loc.method.to_string()),
                ("school", &loc.school.to_string()),
                ("timezonestring", "Europe/London"),
                ("iso8601", "true"),
            ])
            .send()
            .await?
            // A city AlAdhan can't find is a 400 response.
            .error_for_status()?
            .json()
            .await?;

        to_day_times(date, &response.data.timings).map_err(AppError::PrayerTimes)
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
}

/// `rename_all = "PascalCase"`: the JSON keys are `Fajr`, `Dhuhr`, ...
/// while Rust fields are lower case. Sunrise, Midnight etc. are ignored.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ApiTimings {
    fajr: String,
    dhuhr: String,
    asr: String,
    maghrib: String,
    isha: String,
}

/// Turns the five ISO 8601 times into a `DayTimes`. Pure, so it's tested
/// without the network.
fn to_day_times(date: NaiveDate, t: &ApiTimings) -> Result<DayTimes, String> {
    let parse = |prayer: Prayer, text: &str| {
        DateTime::parse_from_rfc3339(text)
            .map(|time| (prayer, time.with_timezone(&Utc)))
            .map_err(|e| format!("bad {prayer} time {text:?}: {e}"))
    };
    Ok(DayTimes {
        date,
        times: [
            parse(Prayer::Fajr, &t.fajr)?,
            parse(Prayer::Dhuhr, &t.dhuhr)?,
            parse(Prayer::Asr, &t.asr)?,
            parse(Prayer::Maghrib, &t.maghrib)?,
            parse(Prayer::Isha, &t.isha)?,
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
        "Isha":"2026-10-27T18:31:00+00:00","Imsak":"2026-10-27T04:45:00+00:00"},
        "meta":{"timezone":"Europe/London"}}}"#;

    #[test]
    fn parses_a_real_reply() {
        let reply: ApiResponse = serde_json::from_str(REPLY).unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();
        let day = to_day_times(date, &reply.data.timings).unwrap();

        assert_eq!(day.date, date);
        assert_eq!(
            day.times[0],
            (Prayer::Fajr, "2026-10-27T04:55:00Z".parse().unwrap())
        );
        assert_eq!(
            day.times[4],
            (Prayer::Isha, "2026-10-27T18:31:00Z".parse().unwrap())
        );
    }

    #[test]
    fn summer_offset_is_converted_to_utc() {
        let mut reply: ApiResponse = serde_json::from_str(REPLY).unwrap();
        reply.data.timings.asr = "2026-07-01T17:30:00+01:00".to_string();
        let date = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        let day = to_day_times(date, &reply.data.timings).unwrap();
        assert_eq!(
            day.times[2].1,
            "2026-07-01T16:30:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn a_bad_time_is_an_error_naming_the_prayer() {
        let mut reply: ApiResponse = serde_json::from_str(REPLY).unwrap();
        reply.data.timings.isha = "18:31".to_string();
        let date = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();
        let err = to_day_times(date, &reply.data.timings).unwrap_err();
        assert!(err.contains("Isha"), "{err}");
    }

    #[tokio::test]
    async fn cached_day_is_returned_without_fetching() {
        let source = PrayerTimes::new(Location {
            city: "Nowhere".to_string(),
            country: "ZZ".to_string(),
            method: 3,
            school: 0,
        });
        let reply: ApiResponse = serde_json::from_str(REPLY).unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();
        let day = to_day_times(date, &reply.data.timings).unwrap();
        *source.cached.lock().await = Some(day.clone());

        // "Nowhere, ZZ" would fail if it went to the network.
        assert_eq!(source.for_date(date).await.unwrap(), day);
    }
}

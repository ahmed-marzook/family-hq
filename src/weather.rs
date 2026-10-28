//! Weather forecasts from Open-Meteo (<https://open-meteo.com>): free, no
//! API key. One request gives the current weather and seven days.

use chrono::{NaiveDate, NaiveDateTime};
use chrono_tz::Tz;
use serde::Deserialize;

use crate::error::AppError;
use crate::geo::Coordinates;
use crate::moon::RiseSet;

/// Which forecast `/weather` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// `/weather`: now, plus the rest of today.
    Now,
    /// `/weather tomorrow`
    Tomorrow,
    /// `/weather week`: the next seven days.
    Week,
}

/// The weather now.
#[derive(Debug, Clone, PartialEq)]
pub struct Current {
    pub temperature: f64,
    pub feels_like: f64,
    /// WMO weather code (see `describe`).
    pub code: u8,
    /// km/h.
    pub wind: f64,
}

/// One day's forecast.
#[derive(Debug, Clone, PartialEq)]
pub struct Day {
    pub date: NaiveDate,
    pub code: u8,
    pub min: f64,
    pub max: f64,
    /// Highest chance of rain that day, 0-100. Sometimes missing for days
    /// far ahead.
    pub rain_chance: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Forecast {
    pub current: Current,
    /// Today first, then the following days.
    pub days: Vec<Day>,
}

pub struct Weather {
    http: reqwest::Client,
    base_url: String,
}

impl Default for Weather {
    fn default() -> Self {
        Self::new()
    }
}

impl Weather {
    pub fn new() -> Self {
        Weather {
            http: reqwest::Client::new(),
            base_url: "https://api.open-meteo.com/v1".to_string(),
        }
    }

    /// Current weather and seven days at `at`, with days counted in
    /// `timezone` (so "today" is the person's today).
    pub async fn forecast(&self, at: Coordinates, timezone: Tz) -> Result<Forecast, AppError> {
        let reply: Reply = self
            .http
            .get(format!("{}/forecast", self.base_url))
            .query(&[
                ("latitude", at.latitude.to_string()),
                ("longitude", at.longitude.to_string()),
                (
                    "current",
                    "temperature_2m,apparent_temperature,weather_code,wind_speed_10m".to_string(),
                ),
                (
                    "daily",
                    "weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max"
                        .to_string(),
                ),
                ("timezone", timezone.name().to_string()),
                ("forecast_days", "7".to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        to_forecast(reply).map_err(AppError::Weather)
    }

    /// Moonrise and moonset at `at` on `date` (a day in `timezone`), in
    /// local time. Open-Meteo works these out too, so `/moon` uses the
    /// same service as the weather.
    pub async fn moon_times(
        &self,
        at: Coordinates,
        timezone: Tz,
        date: NaiveDate,
    ) -> Result<RiseSet, AppError> {
        let day = date.format("%Y-%m-%d").to_string();
        let reply: MoonReply = self
            .http
            .get(format!("{}/forecast", self.base_url))
            .query(&[
                ("latitude", at.latitude.to_string()),
                ("longitude", at.longitude.to_string()),
                ("daily", "moonrise,moonset".to_string()),
                ("timezone", timezone.name().to_string()),
                ("start_date", day.clone()),
                ("end_date", day),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        to_rise_set(reply).map_err(AppError::Weather)
    }
}

/// `{"daily":{"moonrise":["2026-10-28T17:24"],"moonset":[null]}}`: one day,
/// local times without an offset. `null` when the moon doesn't rise or set
/// that day.
#[derive(Deserialize)]
struct MoonReply {
    daily: MoonDaily,
}

#[derive(Deserialize)]
struct MoonDaily {
    moonrise: Vec<Option<String>>,
    moonset: Vec<Option<String>>,
}

/// The first (only) day's moonrise and moonset. Pure, so it's tested with a
/// saved reply.
fn to_rise_set(reply: MoonReply) -> Result<RiseSet, String> {
    // `Option<&Option<String>>` -> `Option<NaiveDateTime>`: `flatten` drops
    // the outer "is there a first day" and inner "is there a time" layers
    // into one, then the text is parsed if there is any.
    let parse = |times: &[Option<String>]| -> Result<Option<NaiveDateTime>, String> {
        times
            .first()
            .cloned()
            .flatten()
            .map(|text| {
                NaiveDateTime::parse_from_str(&text, "%Y-%m-%dT%H:%M")
                    .map_err(|e| format!("bad moon time {text:?}: {e}"))
            })
            .transpose()
    };
    Ok(RiseSet {
        rise: parse(&reply.daily.moonrise)?,
        set: parse(&reply.daily.moonset)?,
    })
}

/// Open-Meteo's reply. `daily` is "one array per field", so day *i* is the
/// *i*-th entry of each array.
#[derive(Deserialize)]
struct Reply {
    current: ReplyCurrent,
    daily: ReplyDaily,
}

#[derive(Deserialize)]
struct ReplyCurrent {
    temperature_2m: f64,
    apparent_temperature: f64,
    weather_code: u8,
    wind_speed_10m: f64,
}

#[derive(Deserialize)]
struct ReplyDaily {
    time: Vec<String>,
    weather_code: Vec<u8>,
    temperature_2m_max: Vec<f64>,
    temperature_2m_min: Vec<f64>,
    precipitation_probability_max: Vec<Option<f64>>,
}

/// Turns the arrays into one `Day` per date. Pure, so it's tested with a
/// saved reply.
fn to_forecast(reply: Reply) -> Result<Forecast, String> {
    let d = reply.daily;
    let mut days = Vec::new();
    for (i, date) in d.time.iter().enumerate() {
        // `.get(i)` instead of `[i]`: if the arrays were ever different
        // lengths, that's an error message, not a crash.
        let missing = || format!("day {date} is missing a value");
        days.push(Day {
            date: date
                .parse()
                .map_err(|e| format!("bad date {date:?}: {e}"))?,
            code: *d.weather_code.get(i).ok_or_else(missing)?,
            max: *d.temperature_2m_max.get(i).ok_or_else(missing)?,
            min: *d.temperature_2m_min.get(i).ok_or_else(missing)?,
            rain_chance: d.precipitation_probability_max.get(i).copied().flatten(),
        });
    }
    if days.is_empty() {
        return Err("no days in the forecast".to_string());
    }
    let c = reply.current;
    Ok(Forecast {
        current: Current {
            temperature: c.temperature_2m,
            feels_like: c.apparent_temperature,
            code: c.weather_code,
            wind: c.wind_speed_10m,
        },
        days,
    })
}

/// An emoji and words for a WMO weather code
/// (<https://open-meteo.com/en/docs>, "WMO Weather interpretation codes").
pub fn describe(code: u8) -> (&'static str, &'static str) {
    match code {
        0 => ("☀️", "Clear sky"),
        1 => ("🌤️", "Mainly clear"),
        2 => ("⛅", "Partly cloudy"),
        3 => ("☁️", "Overcast"),
        45 | 48 => ("🌫️", "Fog"),
        51 => ("🌦️", "Light drizzle"),
        53 => ("🌦️", "Drizzle"),
        55 => ("🌧️", "Heavy drizzle"),
        56 | 57 => ("🌧️", "Freezing drizzle"),
        61 => ("🌦️", "Light rain"),
        63 => ("🌧️", "Rain"),
        65 => ("🌧️", "Heavy rain"),
        66 | 67 => ("🌧️", "Freezing rain"),
        71 => ("🌨️", "Light snow"),
        73 => ("🌨️", "Snow"),
        75 => ("❄️", "Heavy snow"),
        77 => ("🌨️", "Snow grains"),
        80 => ("🌦️", "Light showers"),
        81 => ("🌧️", "Showers"),
        82 => ("⛈️", "Violent showers"),
        85 | 86 => ("🌨️", "Snow showers"),
        95 => ("⛈️", "Thunderstorm"),
        96 | 99 => ("⛈️", "Thunderstorm with hail"),
        _ => ("🌡️", "Unknown weather"),
    }
}

/// "8–15°C" (whole degrees; a forecast isn't more precise than that).
fn range(day: &Day) -> String {
    format!("{:.0}–{:.0}°C", day.min, day.max)
}

/// ", 80% chance of rain", or nothing if it's low or unknown.
fn rain(day: &Day) -> String {
    match day.rain_chance {
        Some(chance) if chance >= 20.0 => format!(", {chance:.0}% chance of rain"),
        _ => String::new(),
    }
}

/// The `/weather` reply.
pub fn format(forecast: &Forecast, when: When, city: &str) -> String {
    let today = &forecast.days[0];
    match when {
        When::Now => {
            let c = &forecast.current;
            let (icon, words) = describe(c.code);
            let (day_icon, day_words) = describe(today.code);
            format!(
                "{icon} *Weather in {city}*\n\
                 Now: {:.0}°C (feels like {:.0}°C), {words}, wind {:.0} km/h\n\
                 Today: {day_icon} {day_words}, {}{}\n\n\
                 `/weather tomorrow` · `/weather week`",
                c.temperature,
                c.feels_like,
                c.wind,
                range(today),
                rain(today)
            )
        }
        When::Tomorrow => match forecast.days.get(1) {
            Some(day) => {
                let (icon, words) = describe(day.code);
                format!(
                    "{icon} *Tomorrow in {city}* ({})\n{words}, {}{}",
                    day.date.format("%a %-d %b"),
                    range(day),
                    rain(day)
                )
            }
            None => "❓ No forecast for tomorrow yet.".to_string(),
        },
        When::Week => {
            let mut text = format!("📅 *The week in {city}*\n");
            for day in &forecast.days {
                let (icon, words) = describe(day.code);
                text.push_str(&format!(
                    "{} {icon} {}, {words}{}\n",
                    day.date.format("%a %-d"),
                    range(day),
                    rain(day)
                ));
            }
            text.trim_end().to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed from a real reply for Milton Keynes, 27 Oct 2026 (3 of 7 days).
    const REPLY: &str = r#"{
        "current": {"time":"2026-10-27T20:15","interval":900,"temperature_2m":9.3,
                    "apparent_temperature":6.2,"weather_code":0,"wind_speed_10m":14.4},
        "daily": {"time":["2026-10-27","2026-10-28","2026-10-29"],
                  "weather_code":[53,61,3],
                  "temperature_2m_max":[15.4,13.1,12.0],
                  "temperature_2m_min":[8.0,7.6,5.5],
                  "precipitation_probability_max":[100,80,null]}}"#;

    fn forecast() -> Forecast {
        to_forecast(serde_json::from_str(REPLY).unwrap()).unwrap()
    }

    #[test]
    fn moonrise_and_moonset_are_read_as_local_times() {
        let reply = r#"{"daily":{"time":["2026-10-28"],
            "moonrise":["2026-10-28T17:24"],"moonset":["2026-10-28T10:21"]}}"#;
        let times = to_rise_set(serde_json::from_str(reply).unwrap()).unwrap();
        let at = |s: &str| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap();
        assert_eq!(times.rise, Some(at("2026-10-28 17:24")));
        assert_eq!(times.set, Some(at("2026-10-28 10:21")));
    }

    #[test]
    fn a_day_without_moonrise_is_none_not_an_error() {
        let reply = r#"{"daily":{"moonrise":[null],"moonset":["2026-10-28T10:21"]}}"#;
        let times = to_rise_set(serde_json::from_str(reply).unwrap()).unwrap();
        assert_eq!(times.rise, None);
        assert!(times.set.is_some());

        let bad = r#"{"daily":{"moonrise":["5pm"],"moonset":[]}}"#;
        let err = to_rise_set(serde_json::from_str(bad).unwrap()).unwrap_err();
        assert!(err.contains("5pm"), "{err}");
    }

    #[test]
    fn parses_current_and_days() {
        let f = forecast();
        assert_eq!(f.current.temperature, 9.3);
        assert_eq!(f.days.len(), 3);
        assert_eq!(
            f.days[1].date,
            NaiveDate::from_ymd_opt(2026, 10, 28).unwrap()
        );
        assert_eq!(f.days[2].rain_chance, None, "null rain chance is allowed");
    }

    #[test]
    fn mismatched_arrays_are_an_error_not_a_crash() {
        let broken = REPLY.replace("[53,61,3]", "[53]");
        let err = to_forecast(serde_json::from_str(&broken).unwrap()).unwrap_err();
        assert!(err.contains("missing"), "{err}");
    }

    #[test]
    fn now_shows_current_and_today() {
        let text = format(&forecast(), When::Now, "Milton Keynes");
        assert!(
            text.starts_with("☀️ *Weather in Milton Keynes*\n"),
            "{text}"
        );
        assert!(
            text.contains("Now: 9°C (feels like 6°C), Clear sky, wind 14 km/h"),
            "{text}"
        );
        assert!(
            text.contains("Today: 🌦️ Drizzle, 8–15°C, 100% chance of rain"),
            "{text}"
        );
    }

    #[test]
    fn tomorrow_and_week() {
        let text = format(&forecast(), When::Tomorrow, "Milton Keynes");
        assert!(
            text.starts_with("🌦️ *Tomorrow in Milton Keynes* (Wed 28 Oct)\n"),
            "{text}"
        );
        assert!(
            text.contains("Light rain, 8–13°C, 80% chance of rain"),
            "{text}"
        );

        let text = format(&forecast(), When::Week, "Milton Keynes");
        assert!(
            text.contains("Tue 27 🌦️ 8–15°C, Drizzle, 100% chance of rain\n"),
            "{text}"
        );
        assert!(text.ends_with("Thu 29 ☁️ 6–12°C, Overcast"), "{text}");
    }

    #[test]
    fn every_wmo_code_has_words() {
        for code in [0, 1, 2, 3, 45, 51, 61, 63, 71, 80, 95, 99] {
            assert_ne!(describe(code).1, "Unknown weather", "code {code}");
        }
        assert_eq!(describe(42).1, "Unknown weather");
    }
}

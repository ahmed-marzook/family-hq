//! Weather forecasts from Open-Meteo (<https://open-meteo.com>): free, no
//! API key. One request gives the current weather, seven days, and
//! yesterday (for how much the daylight changed).

use std::time::{Duration, Instant};

use chrono::{Datelike, NaiveDate, NaiveDateTime};
use chrono_tz::Tz;
use serde::Deserialize;
use tracing::warn;

use crate::cache::{PlaceKey, TtlCache};
use crate::calendar::{self, Climate, Normals};
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
    /// `/weather hourly`: the next 12 hours.
    Hourly,
}

/// The weather now.
#[derive(Debug, Clone, PartialEq)]
pub struct Current {
    /// When this was measured, local time (to the quarter hour).
    pub time: NaiveDateTime,
    pub temperature: f64,
    pub feels_like: f64,
    /// WMO weather code (see `describe`).
    pub code: u8,
    /// km/h.
    pub wind: f64,
    /// Where the wind comes from, in degrees: 0 north, 90 east.
    pub wind_from: f64,
}

/// One hour's forecast.
#[derive(Debug, Clone, PartialEq)]
pub struct Hour {
    /// The start of the hour, local time.
    pub time: NaiveDateTime,
    pub code: u8,
    pub temperature: f64,
    /// Chance of rain, 0-100.
    pub rain_chance: Option<f64>,
    /// Whether the sun is up, for a 🌙 instead of a ☀️ on a clear night.
    pub is_day: bool,
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
    /// Local times. Missing where the sun doesn't rise or set that day
    /// (the far north in winter or summer).
    pub sunrise: Option<NaiveDateTime>,
    pub sunset: Option<NaiveDateTime>,
    /// Seconds of daylight.
    pub daylight: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Forecast {
    pub current: Current,
    /// Today first, then the following days.
    pub days: Vec<Day>,
    /// Every hour from yesterday on; `/weather hourly` picks from now.
    pub hours: Vec<Hour>,
    /// Seconds of daylight yesterday, to say how today's compares.
    pub yesterday_daylight: Option<f64>,
    /// Where the forecast is for: it decides the seasons (none in the
    /// tropics, the other way round below the equator).
    pub latitude: f64,
    /// The place's usual year, for its seasons. Only fetched in the
    /// tropics; `None` elsewhere or if the history couldn't be fetched.
    pub normals: Option<Normals>,
}

/// How long a forecast is reused. Open-Meteo updates "current" every 15
/// minutes, so anything longer would show older weather than it has.
const FORECAST_FOR: Duration = Duration::from_secs(15 * 60);

/// How long a place's monthly averages are reused. They're ten years of
/// history, so a new month barely changes them.
const NORMALS_FOR: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// How many full years of history the averages are from.
const HISTORY_YEARS: i32 = 10;

/// How long moonrise and moonset are reused. They never change for a
/// given day; this only stops old days piling up.
const MOON_TIMES_FOR: Duration = Duration::from_secs(12 * 60 * 60);

pub struct Weather {
    http: reqwest::Client,
    base_url: String,
    /// Recent forecasts per place, so several `/weather`s in a row (or a
    /// whole group asking) make one request.
    forecasts: TtlCache<PlaceKey, Forecast>,
    /// Moonrise and moonset per place and day.
    moon_times: TtlCache<(PlaceKey, NaiveDate), RiseSet>,
    /// Monthly averages per place (tropical places only).
    normals: TtlCache<PlaceKey, Normals>,
    /// Open-Meteo's history is a separate address from its forecasts.
    archive_url: String,
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
            forecasts: TtlCache::new(FORECAST_FOR),
            moon_times: TtlCache::new(MOON_TIMES_FOR),
            normals: TtlCache::new(NORMALS_FOR),
            archive_url: "https://archive-api.open-meteo.com/v1".to_string(),
        }
    }

    /// Current weather and seven days at `at`, with days counted in
    /// `timezone` (so "today" is the person's today). Reuses one fetched
    /// in the last 15 minutes.
    ///
    /// Two requests arriving together before the first answer is cached
    /// both fetch; that's harmless, and simpler than making one wait.
    pub async fn forecast(&self, at: Coordinates, timezone: Tz) -> Result<Forecast, AppError> {
        let key = PlaceKey::new(at, timezone);
        if let Some(forecast) = self.forecasts.get(&key, Instant::now()) {
            return Ok(forecast);
        }
        let mut forecast = self.fetch_forecast(at, timezone).await?;
        // The tropics' seasons come from the place's history. If that
        // fails, the forecast still goes out (with a note instead).
        if Climate::at(forecast.latitude) == Climate::Tropical {
            let this_year = forecast.days[0].date.year();
            forecast.normals = self
                .normals(at, timezone, this_year)
                .await
                .inspect_err(|err| warn!("no monthly averages: {err}"))
                .ok();
        }
        // Only answers are cached, never errors, so a failure is retried
        // on the next request.
        self.forecasts.insert(key, forecast.clone(), Instant::now());
        Ok(forecast)
    }

    /// The place's average rain and temperature for each month over the
    /// last ten full years before `this_year`. One request of about 85 KB,
    /// then cached for 30 days.
    async fn normals(
        &self,
        at: Coordinates,
        timezone: Tz,
        this_year: i32,
    ) -> Result<Normals, AppError> {
        let key = PlaceKey::new(at, timezone);
        if let Some(normals) = self.normals.get(&key, Instant::now()) {
            return Ok(normals);
        }
        let first_year = this_year - HISTORY_YEARS;
        let reply: ArchiveReply = self
            .http
            .get(format!("{}/archive", self.archive_url))
            .query(&[
                ("latitude", at.latitude.to_string()),
                ("longitude", at.longitude.to_string()),
                ("start_date", format!("{first_year}-01-01")),
                ("end_date", format!("{}-12-31", this_year - 1)),
                ("daily", "precipitation_sum,temperature_2m_mean".to_string()),
                ("timezone", "UTC".to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let normals = to_normals(&reply.daily).map_err(AppError::Weather)?;
        self.normals.insert(key, normals.clone(), Instant::now());
        Ok(normals)
    }

    async fn fetch_forecast(&self, at: Coordinates, timezone: Tz) -> Result<Forecast, AppError> {
        let reply: Reply = self
            .http
            .get(format!("{}/forecast", self.base_url))
            .query(&[
                ("latitude", at.latitude.to_string()),
                ("longitude", at.longitude.to_string()),
                (
                    "current",
                    "temperature_2m,apparent_temperature,weather_code,wind_speed_10m,\
                     wind_direction_10m"
                        .to_string(),
                ),
                (
                    "daily",
                    "weather_code,temperature_2m_max,temperature_2m_min,\
                     precipitation_probability_max,sunrise,sunset,daylight_duration"
                        .to_string(),
                ),
                (
                    "hourly",
                    "temperature_2m,weather_code,precipitation_probability,is_day".to_string(),
                ),
                ("timezone", timezone.name().to_string()),
                ("forecast_days", "7".to_string()),
                // Yesterday as well, as the first day.
                ("past_days", "1".to_string()),
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
    /// same service as the weather. Fetched once per place and day.
    pub async fn moon_times(
        &self,
        at: Coordinates,
        timezone: Tz,
        date: NaiveDate,
    ) -> Result<RiseSet, AppError> {
        let key = (PlaceKey::new(at, timezone), date);
        if let Some(times) = self.moon_times.get(&key, Instant::now()) {
            return Ok(times);
        }
        let times = self.fetch_moon_times(at, timezone, date).await?;
        self.moon_times.insert(key, times, Instant::now());
        Ok(times)
    }

    async fn fetch_moon_times(
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

/// Daily history: `{"daily":{"time":["2015-01-01",...],
/// "precipitation_sum":[0.4,...],"temperature_2m_mean":[26.3,...]}}`.
#[derive(Deserialize)]
struct ArchiveReply {
    daily: ArchiveDaily,
}

#[derive(Deserialize)]
struct ArchiveDaily {
    time: Vec<String>,
    precipitation_sum: Vec<Option<f64>>,
    temperature_2m_mean: Vec<Option<f64>>,
}

/// Averages daily history into 12 months: rain is each month's total,
/// averaged over the years; temperature the average day. Days with a
/// missing value are skipped. Pure, so it's tested with made-up days.
fn to_normals(daily: &ArchiveDaily) -> Result<Normals, String> {
    let mut rain_total = [0.0; 12];
    let mut temperature_total = [0.0; 12];
    let mut temperature_days = [0_u32; 12];
    // Which years each month was seen in, to divide rain totals by.
    let mut years: [std::collections::BTreeSet<&str>; 12] = Default::default();

    for (i, date) in daily.time.iter().enumerate() {
        // "2015-01-31": the year is [0..4], the month [5..7]. `get` rather
        // than `[..]` so a short string is skipped, not a panic.
        let (Some(year), Some(month)) = (date.get(0..4), date.get(5..7)) else {
            continue;
        };
        let Ok(month) = month.parse::<usize>() else {
            continue;
        };
        let Some(m) = month.checked_sub(1).filter(|m| *m < 12) else {
            continue;
        };
        if let Some(Some(rain)) = daily.precipitation_sum.get(i) {
            rain_total[m] += rain;
            years[m].insert(year);
        }
        if let Some(Some(temperature)) = daily.temperature_2m_mean.get(i) {
            temperature_total[m] += temperature;
            temperature_days[m] += 1;
        }
    }
    if years.iter().any(|y| y.is_empty()) || temperature_days.contains(&0) {
        return Err("history is missing a month".to_string());
    }
    Ok(Normals {
        rain: std::array::from_fn(|m| rain_total[m] / years[m].len() as f64),
        temperature: std::array::from_fn(|m| temperature_total[m] / f64::from(temperature_days[m])),
    })
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
    latitude: f64,
    current: ReplyCurrent,
    daily: ReplyDaily,
    hourly: ReplyHourly,
}

#[derive(Deserialize)]
struct ReplyHourly {
    time: Vec<String>,
    temperature_2m: Vec<f64>,
    weather_code: Vec<u8>,
    precipitation_probability: Vec<Option<f64>>,
    /// 1 when the sun is up, 0 when not.
    is_day: Vec<u8>,
}

#[derive(Deserialize)]
struct ReplyCurrent {
    time: String,
    temperature_2m: f64,
    apparent_temperature: f64,
    weather_code: u8,
    wind_speed_10m: f64,
    wind_direction_10m: f64,
}

#[derive(Deserialize)]
struct ReplyDaily {
    time: Vec<String>,
    weather_code: Vec<u8>,
    temperature_2m_max: Vec<f64>,
    temperature_2m_min: Vec<f64>,
    precipitation_probability_max: Vec<Option<f64>>,
    sunrise: Vec<Option<String>>,
    sunset: Vec<Option<String>>,
    daylight_duration: Vec<Option<f64>>,
}

/// Open-Meteo's local times, "2026-10-28T06:50", or `None` for a `null`.
fn local_time(text: Option<&String>) -> Result<Option<NaiveDateTime>, String> {
    text.map(|t| {
        NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M")
            .map_err(|e| format!("bad time {t:?}: {e}"))
    })
    .transpose()
}

/// Turns the arrays into one `Day` per date. The first is yesterday
/// (`past_days=1`): only its daylight is kept. Pure, so it's tested with a
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
            sunrise: local_time(d.sunrise.get(i).and_then(Option::as_ref))?,
            sunset: local_time(d.sunset.get(i).and_then(Option::as_ref))?,
            daylight: d.daylight_duration.get(i).copied().flatten(),
        });
    }
    // Yesterday, then at least today.
    if days.len() < 2 {
        return Err("no days in the forecast".to_string());
    }
    let yesterday = days.remove(0);
    let h = reply.hourly;
    let mut hours = Vec::new();
    for (i, time) in h.time.iter().enumerate() {
        let missing = || format!("hour {time} is missing a value");
        hours.push(Hour {
            time: local_time(Some(time))?.ok_or_else(missing)?,
            code: *h.weather_code.get(i).ok_or_else(missing)?,
            temperature: *h.temperature_2m.get(i).ok_or_else(missing)?,
            rain_chance: h.precipitation_probability.get(i).copied().flatten(),
            is_day: *h.is_day.get(i).ok_or_else(missing)? == 1,
        });
    }

    let c = reply.current;
    Ok(Forecast {
        current: Current {
            time: local_time(Some(&c.time))?.ok_or("no current time")?,
            temperature: c.temperature_2m,
            feels_like: c.apparent_temperature,
            code: c.weather_code,
            wind: c.wind_speed_10m,
            wind_from: c.wind_direction_10m,
        },
        days,
        hours,
        yesterday_daylight: yesterday.daylight,
        latitude: reply.latitude,
        normals: None,
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

/// The compass point a wind comes from: 230° is "SW".
fn compass(degrees: f64) -> &'static str {
    const POINTS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    // Each point covers 45°, centred on itself (N is 337.5° to 22.5°).
    let slice = ((degrees.rem_euclid(360.0) + 22.5) / 45.0) as usize % 8;
    POINTS[slice]
}

/// "9h 52m" for a number of seconds.
fn hours_minutes(seconds: f64) -> String {
    let minutes = (seconds / 60.0).round() as i64;
    format!("{}h {:02}m", minutes / 60, minutes % 60)
}

/// "2m 14s less daylight than yesterday", from the two lengths in seconds.
fn daylight_change(today: f64, yesterday: f64) -> String {
    let change = (today - yesterday).round() as i64;
    let size = change.abs();
    let amount = match (size / 60, size % 60) {
        (0, seconds) => format!("{seconds}s"),
        (minutes, seconds) => format!("{minutes}m {seconds:02}s"),
    };
    match change {
        0 => "same daylight as yesterday".to_string(),
        c if c < 0 => format!("{amount} less daylight than yesterday"),
        _ => format!("{amount} more daylight than yesterday"),
    }
}

const SUN: &str = "🌅";

/// "🌅 Sun 06:50–16:42 · 9h 52m of daylight, 2m 14s less than yesterday",
/// or `None` if Open-Meteo had no sunrise or sunset today.
fn sun_line(today: &Day, yesterday_daylight: Option<f64>) -> Option<String> {
    let (rise, set) = (today.sunrise?, today.sunset?);
    let mut line = format!("{SUN} Sun {}–{}", rise.format("%H:%M"), set.format("%H:%M"));
    if let Some(daylight) = today.daylight {
        line.push_str(&format!(" · {} of daylight", hours_minutes(daylight)));
        if let Some(yesterday) = yesterday_daylight {
            line.push_str(&format!(" ({})", daylight_change(daylight, yesterday)));
        }
    }
    Some(line)
}

const NIGHT: &str = "🌙";

/// The weather's emoji, without a sun at night: a moon when it's clear,
/// a plain cloud when it's partly cloudy.
fn icon_at(code: u8, is_day: bool) -> &'static str {
    match code {
        0 | 1 if !is_day => NIGHT,
        2 if !is_day => describe(3).0,
        _ => describe(code).0,
    }
}

/// How many hours `/weather hourly` shows.
const HOURS: usize = 12;

/// `/weather hourly`: the next 12 hours from the current one, with the
/// day named when it passes midnight.
fn format_hours(forecast: &Forecast, city: &str) -> String {
    // The hour `current.time` is in: 20:15 → 20:00. `with_minute(0)` can't
    // fail, but it returns an `Option`, so fall back to the time itself.
    let now = forecast.current.time;
    let this_hour = chrono::Timelike::with_minute(&now, 0).unwrap_or(now);
    let coming: Vec<&Hour> = forecast
        .hours
        .iter()
        .filter(|hour| hour.time >= this_hour)
        .take(HOURS)
        .collect();
    let Some(first) = coming.first() else {
        return "❓ No hourly forecast right now. Try `/weather`.".to_string();
    };

    let mut text = format!("🕐 *Next {HOURS} hours in {city}*\n");
    let mut day = first.time.date();
    for hour in coming {
        if hour.time.date() != day {
            day = hour.time.date();
            text.push_str(&format!("\n*{}*\n", day.format("%a %-d %b")));
        }
        let rain = match hour.rain_chance {
            Some(chance) if chance >= 20.0 => format!(" · {chance:.0}% rain"),
            _ => String::new(),
        };
        text.push_str(&format!(
            "{} {} {:.0}°C{rain}\n",
            hour.time.format("%H:%M"),
            icon_at(hour.code, hour.is_day),
            hour.temperature
        ));
    }
    text.push_str("\n`/weather` · `/weather tomorrow` · `/weather week`");
    text
}

/// The `/weather` reply.
pub fn format(forecast: &Forecast, when: When, city: &str) -> String {
    let today = &forecast.days[0];
    match when {
        When::Now => {
            let c = &forecast.current;
            let (icon, words) = describe(c.code);
            let (day_icon, day_words) = describe(today.code);
            let mut text = format!(
                "{icon} *Weather in {city}*\n\
                 Now: {:.0}°C (feels like {:.0}°C) · {words} · wind {:.0} km/h {}\n\
                 Today: {day_icon} {day_words}, {}{}\n\n",
                c.temperature,
                c.feels_like,
                c.wind,
                compass(c.wind_from),
                range(today),
                rain(today)
            );
            // The forecast's first day is the person's today (it was asked
            // for in their timezone), so no clock is needed here.
            text.push_str(&calendar::format(
                today.date,
                forecast.latitude,
                forecast.normals.as_ref(),
            ));
            text.push_str("\n\n");
            if let Some(sun) = sun_line(today, forecast.yesterday_daylight) {
                text.push_str(&sun);
                text.push_str("\n\n");
            }
            text.push_str("`/weather hourly` · `/weather tomorrow` · `/weather week`");
            text
        }
        When::Hourly => format_hours(forecast, city),
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

    // Trimmed from a real reply for Milton Keynes, 27 Oct 2026: yesterday
    // (26th, from `past_days=1`), then 3 of the 7 days.
    const REPLY: &str = r#"{"latitude":52.04455,
        "current": {"time":"2026-10-27T20:15","interval":900,"temperature_2m":9.3,
                    "apparent_temperature":6.2,"weather_code":0,"wind_speed_10m":14.4,
                    "wind_direction_10m":230},
"hourly": {"time":["2026-10-27T18:00","2026-10-27T19:00","2026-10-27T20:00","2026-10-27T21:00","2026-10-27T22:00","2026-10-27T23:00","2026-10-28T00:00","2026-10-28T01:00","2026-10-28T02:00","2026-10-28T03:00","2026-10-28T04:00","2026-10-28T05:00","2026-10-28T06:00","2026-10-28T07:00","2026-10-28T08:00","2026-10-28T09:00"],
                  "temperature_2m":[10.1,9.8,9.3,9.0,8.7,8.4,8.2,8.0,7.8,7.6,7.5,7.5,7.6,8.1,9.0,10.2],
                  "weather_code":[2,1,0,0,1,2,3,61,61,63,3,3,2,1,1,2],
                  "precipitation_probability":[10,5,0,0,0,10,30,70,80,90,40,20,10,null,0,0],
                  "is_day":[0,0,0,0,0,0,0,0,0,0,0,0,0,1,1,1]},
        "daily": {"time":["2026-10-26","2026-10-27","2026-10-28","2026-10-29"],
                  "weather_code":[3,53,61,3],
                  "temperature_2m_max":[14.0,15.4,13.1,12.0],
                  "temperature_2m_min":[6.0,8.0,7.6,5.5],
                  "precipitation_probability_max":[10,100,80,null],
                  "sunrise":["2026-10-26T06:46","2026-10-27T06:48","2026-10-28T06:50",null],
                  "sunset":["2026-10-26T16:46","2026-10-27T16:44","2026-10-28T16:42",null],
                  "daylight_duration":[36130.4,35862.1,35595.0,null]}}"#;

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
        assert_eq!(f.days.len(), 3, "yesterday isn't one of the days");
        assert_eq!(
            f.days[0].date,
            NaiveDate::from_ymd_opt(2026, 10, 27).unwrap()
        );
        assert_eq!(f.yesterday_daylight, Some(36130.4));
        assert_eq!(f.current.wind_from, 230.0);
        assert!(f.days[2].sunrise.is_none(), "null sunrise is allowed");
        assert_eq!(
            f.days[1].date,
            NaiveDate::from_ymd_opt(2026, 10, 28).unwrap()
        );
        assert_eq!(f.days[2].rain_chance, None, "null rain chance is allowed");
    }

    #[test]
    fn mismatched_arrays_are_an_error_not_a_crash() {
        let broken = REPLY.replace("[3,53,61,3]", "[3]");
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
            text.contains("Now: 9°C (feels like 6°C) · Clear sky · wind 14 km/h SW\n"),
            "{text}"
        );
        assert!(
            text.contains("Today: 🌦️ Drizzle, 8–15°C, 100% chance of rain\n\n🍂 *Autumn*"),
            "{text}"
        );
        assert!(text.contains("🍂 *Autumn* (day 57 of 91)\n"), "{text}");
        assert!(text.contains("🗓️ *2026* (day 300 of 365)\n"), "{text}");
        // 35862.1 - 36130.4 = -268.3 seconds.
        assert!(
            text.contains(
                "🌅 Sun 06:48–16:44 · 9h 58m of daylight (4m 28s less daylight than yesterday)\n"
            ),
            "{text}"
        );
        assert!(
            text.ends_with("`/weather hourly` · `/weather tomorrow` · `/weather week`"),
            "{text}"
        );
    }

    #[test]
    fn hourly_starts_at_this_hour_and_names_the_next_day() {
        // It's 20:15 on the 27th: 20:00 to 07:00, 12 hours.
        let text = format(&forecast(), When::Hourly, "Milton Keynes");
        let expected = "🕐 *Next 12 hours in Milton Keynes*\n\
             20:00 🌙 9°C\n\
             21:00 🌙 9°C\n\
             22:00 🌙 9°C\n\
             23:00 ☁️ 8°C\n\
             \n*Wed 28 Oct*\n\
             00:00 ☁️ 8°C · 30% rain\n\
             01:00 🌦️ 8°C · 70% rain\n\
             02:00 🌦️ 8°C · 80% rain\n\
             03:00 🌧️ 8°C · 90% rain\n\
             04:00 ☁️ 8°C · 40% rain\n\
             05:00 ☁️ 8°C · 20% rain\n\
             06:00 ☁️ 8°C\n\
             07:00 🌤️ 8°C\n\n\
             `/weather` · `/weather tomorrow` · `/weather week`";
        assert_eq!(text, expected);
    }

    #[test]
    fn hourly_with_no_hours_left_says_so() {
        let mut forecast = forecast();
        forecast.hours.clear();
        let text = format(&forecast, When::Hourly, "Milton Keynes");
        assert!(text.starts_with("❓ No hourly forecast"), "{text}");
    }

    #[test]
    fn southern_hemisphere_has_its_own_season() {
        let mut forecast = forecast();
        forecast.latitude = -33.87;
        let text = format(&forecast, When::Now, "Sydney");
        assert!(text.contains("🌸 *Spring* (day 57 of 91)"), "{text}");
    }

    #[test]
    fn the_tropics_have_no_four_seasons() {
        let mut forecast = forecast();
        forecast.latitude = 21.42;
        let text = format(&forecast, When::Now, "Makkah");
        assert!(
            text.contains("🌴 No spring, summer, autumn or winter"),
            "{text}"
        );
        assert!(text.contains("🗓️ *2026*"), "{text}");
    }

    #[test]
    fn history_is_averaged_into_months() {
        // Two Januaries (10 mm and 30 mm of rain) and a February each year;
        // every other month one day. A missing value is skipped.
        let mut time = vec!["2015-01-01", "2016-01-01", "2015-02-01", "2016-02-01"];
        let mut rain = vec![Some(10.0), Some(30.0), Some(4.0), None];
        let mut temperature = vec![Some(20.0), Some(22.0), Some(25.0), Some(27.0)];
        let others: Vec<String> = (3..=12).map(|m| format!("2015-{m:02}-01")).collect();
        for day in &others {
            time.push(day);
            rain.push(Some(100.0));
            temperature.push(Some(30.0));
        }
        let daily = ArchiveDaily {
            time: time.into_iter().map(str::to_string).collect(),
            precipitation_sum: rain,
            temperature_2m_mean: temperature,
        };
        let normals = to_normals(&daily).unwrap();
        assert_eq!(normals.rain[0], 20.0, "average of the two Januaries");
        assert_eq!(normals.rain[1], 4.0, "only one February had rain data");
        assert_eq!(normals.temperature[1], 26.0);
        assert_eq!(normals.rain[11], 100.0);

        let short = ArchiveDaily {
            time: vec!["2015-01-01".to_string()],
            precipitation_sum: vec![Some(1.0)],
            temperature_2m_mean: vec![Some(1.0)],
        };
        assert!(to_normals(&short).unwrap_err().contains("missing a month"));
    }

    #[test]
    fn no_sun_line_without_sunrise() {
        let mut forecast = forecast();
        forecast.days[0].sunrise = None;
        let text = format(&forecast, When::Now, "Tromsø");
        assert!(!text.contains("🌅"), "{text}");
    }

    #[test]
    fn wind_points_and_daylight_words() {
        assert_eq!(compass(0.0), "N");
        assert_eq!(compass(359.0), "N");
        assert_eq!(compass(230.0), "SW");
        assert_eq!(compass(292.5), "NW");
        assert_eq!(hours_minutes(35595.0), "9h 53m");
        assert_eq!(hours_minutes(3600.0), "1h 00m");
        assert_eq!(
            daylight_change(100.0, 234.0),
            "2m 14s less daylight than yesterday"
        );
        assert_eq!(
            daylight_change(130.0, 100.0),
            "30s more daylight than yesterday"
        );
        assert_eq!(daylight_change(100.2, 100.0), "same daylight as yesterday");
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

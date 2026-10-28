//! How far through the season and the year a date is, for `/weather`.
//!
//! Pure date maths: the date comes in (the person's today, from the
//! forecast), nothing reads the clock.

use std::fmt;

use chrono::{DateTime, Datelike, Months, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;

use crate::progress;

/// Meteorological seasons: whole months, three each, as the Met Office
/// uses them. (Astronomical ones start on the equinoxes and solstices,
/// around the 21st, and vary by a day from year to year.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Season {
    Spring,
    Summer,
    Autumn,
    Winter,
}

const SPRING: &str = "🌸";
const SUMMER: &str = "🌻";
const AUTUMN: &str = "🍂";
const WINTER: &str = "❄️";
const YEAR: &str = "🗓️";
const DAY: &str = "⏳";
const TROPICS: &str = "🌴";

impl Season {
    pub fn emoji(self) -> &'static str {
        match self {
            Season::Spring => SPRING,
            Season::Summer => SUMMER,
            Season::Autumn => AUTUMN,
            Season::Winter => WINTER,
        }
    }

    pub fn next(self) -> Season {
        match self {
            Season::Spring => Season::Summer,
            Season::Summer => Season::Autumn,
            Season::Autumn => Season::Winter,
            Season::Winter => Season::Spring,
        }
    }

    /// The season at the same time of year on the other side of the
    /// equator: March is spring in London and autumn in Sydney.
    fn opposite(self) -> Season {
        self.next().next()
    }
}

impl fmt::Display for Season {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            Season::Spring => "Spring",
            Season::Summer => "Summer",
            Season::Autumn => "Autumn",
            Season::Winter => "Winter",
        })
    }
}

/// How far through a stretch of days (a season, a year) a date is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stretch {
    /// Today is day `day` (from 1) of `length`.
    pub day: i64,
    pub length: i64,
    /// Days from today until the next one starts (1 on its last day).
    pub days_left: i64,
}

impl Stretch {
    /// From `start` (its first day) up to `end` (the next one's first day).
    fn between(start: NaiveDate, end: NaiveDate, today: NaiveDate) -> Stretch {
        Stretch {
            day: (today - start).num_days() + 1,
            length: (end - start).num_days(),
            days_left: (end - today).num_days(),
        }
    }
}

/// The season `today` is in, and how far through it. `southern` flips the
/// names for the southern hemisphere (the dates are the same).
pub fn season(today: NaiveDate, southern: bool) -> (Season, Stretch) {
    // Which season by month, and how many months ago it started.
    let (northern, months_in) = match today.month() {
        3..=5 => (Season::Spring, today.month() - 3),
        6..=8 => (Season::Summer, today.month() - 6),
        9..=11 => (Season::Autumn, today.month() - 9),
        // December is the first month of winter; January the second.
        12 => (Season::Winter, 0),
        // January is 1 month in, February 2.
        month => (Season::Winter, month),
    };
    // `with_day(1)` can't fail (every month has a 1st), nor can going back
    // at most two months, so `expect` documents rather than risks a panic.
    let first_of_month = today.with_day(1).expect("every month has a 1st");
    let start = first_of_month - Months::new(months_in);
    let end = start + Months::new(3);
    let season = if southern {
        northern.opposite()
    } else {
        northern
    };
    (season, Stretch::between(start, end, today))
}

/// How far through its year `today` is.
pub fn year(today: NaiveDate) -> Stretch {
    let start = NaiveDate::from_ymd_opt(today.year(), 1, 1).expect("1 January exists");
    let end = start + Months::new(12);
    Stretch::between(start, end, today)
}

/// The bar and words under a heading: "■■■■■■□□□□ 64% done · 34d to Winter".
fn progress_line(stretch: Stretch, next: &str) -> String {
    format!(
        "{} {}% done · {}d to {next}",
        progress::bar(stretch.day, stretch.length),
        progress::percent(stretch.day, stretch.length),
        stretch.days_left
    )
}

/// The Tropics of Cancer and Capricorn, in degrees: between them the sun
/// is overhead at some point in the year, and there's no spring, summer,
/// autumn and winter.
const TROPIC: f64 = 23.44;

/// What kind of year a place has, from how far it is from the equator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Climate {
    /// Four seasons. `southern`: they're the other way round (Christmas in
    /// summer). The polar regions count too: they have the same seasons,
    /// with very long or short days.
    FourSeasons { southern: bool },
    /// Near the equator. The year there is wet and dry seasons (or hot and
    /// less hot), and when they fall depends on the region, not on the
    /// latitude, so they can't be worked out from here.
    Tropical,
}

impl Climate {
    pub fn at(latitude: f64) -> Climate {
        if latitude.abs() < TROPIC {
            Climate::Tropical
        } else {
            Climate::FourSeasons {
                southern: latitude < 0.0,
            }
        }
    }
}

/// The season and year part of `/weather`, for a place at `latitude`:
///
/// ```text
/// 🍂 *Autumn* (day 58 of 91)
/// ■■■■■■□□□□ 64% done · 34d to Winter
///
/// 🗓️ *2026* (day 301 of 365)
/// ■■■■■■■■□□ 82% done · 65d to 2027
/// ```
///
/// In the tropics the season part is one line saying there aren't four.
pub fn format(today: NaiveDate, latitude: f64) -> String {
    let season_part = match Climate::at(latitude) {
        Climate::FourSeasons { southern } => {
            let (season, s) = season(today, southern);
            format!(
                "{} *{season}* (day {} of {})\n{}",
                season.emoji(),
                s.day,
                s.length,
                progress_line(s, &season.next().to_string())
            )
        }
        Climate::Tropical => {
            format!("{TROPICS} No spring, summer, autumn or winter this close to the equator.")
        }
    };
    let y = year(today);
    format!(
        "{season_part}\n\n{YEAR} *{}* (day {} of {})\n{}",
        today.year(),
        y.day,
        y.length,
        progress_line(y, &(today.year() + 1).to_string())
    )
}

/// The moment `date` starts (local midnight) in `timezone`, in UTC.
///
/// `earliest()` because a local time can be ambiguous or missing when the
/// clocks change. Midnight never is in the UK (the change is at 1-2am), but
/// some countries change at midnight, and then the day starts at 01:00:
/// `or_else` tries that.
fn start_of(date: NaiveDate, timezone: Tz) -> Option<DateTime<Utc>> {
    let local = |hour| {
        let time = date.and_hms_opt(hour, 0, 0)?;
        timezone.from_local_datetime(&time).earliest()
    };
    local(0).or_else(|| local(1)).map(|t| t.with_timezone(&Utc))
}

/// How far through their day someone is at `now`, in minutes, on their own
/// clock: from their midnight to the next. Usually 1440 minutes long; 1380
/// or 1500 on the days the clocks change.
pub fn day(now: DateTime<Utc>, timezone: Tz) -> Option<Stretch> {
    let today = now.with_timezone(&timezone).date_naive();
    let start = start_of(today, timezone)?;
    let end = start_of(today.succ_opt()?, timezone)?;
    Some(Stretch {
        day: (now - start).num_minutes(),
        length: (end - start).num_minutes(),
        days_left: (end - now).num_minutes(),
    })
}

/// "⏳ ■■■■■■■□□□ 70% through the day · 7h 12m left", for `/todo`.
pub fn day_line(now: DateTime<Utc>, timezone: Tz) -> Option<String> {
    let d = day(now, timezone)?;
    Some(format!(
        "{DAY} {} {}% through the day · {}h {:02}m left",
        progress::bar(d.day, d.length),
        progress::percent(d.day, d.length),
        d.days_left / 60,
        d.days_left % 60
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn autumn_in_late_october() {
        let (season, s) = season(date(2026, 10, 28), false);
        assert_eq!(season, Season::Autumn);
        // 1 Sep is day 1; Sep + Oct + Nov = 30 + 31 + 30 days; 1 Dec is 34
        // days after 28 Oct.
        assert_eq!(
            s,
            Stretch {
                day: 58,
                length: 91,
                days_left: 34
            }
        );
    }

    #[test]
    fn winter_runs_over_new_year_and_leap_days() {
        let (name, s) = season(date(2027, 1, 15), false);
        assert_eq!(name, Season::Winter);
        assert_eq!((s.day, s.length), (46, 90), "Dec 31 + Jan 15; 31+31+28");
        let (_, leap) = season(date(2028, 2, 29), false);
        assert_eq!((leap.day, leap.length, leap.days_left), (91, 91, 1));
        let (_, first) = season(date(2026, 12, 1), false);
        assert_eq!(first.day, 1, "1 December starts winter");
    }

    #[test]
    fn every_day_of_a_year_is_in_exactly_one_season_day() {
        // Walking a whole year: each day is one more than the day before,
        // or day 1 when the next season starts, and never past the length.
        let mut day = date(2026, 1, 1);
        let mut previous = season(day - chrono::Duration::days(1), false);
        while day.year() == 2026 {
            let current = season(day, false);
            let (_, s) = current;
            assert!(s.day >= 1 && s.day <= s.length, "{day}: {s:?}");
            assert_eq!(s.day + s.days_left - 1, s.length, "{day}");
            if current.0 == previous.0 {
                assert_eq!(s.day, previous.1.day + 1, "{day}");
            } else {
                assert_eq!((current.0, s.day), (previous.0.next(), 1), "{day}");
            }
            previous = current;
            day += chrono::Duration::days(1);
        }
    }

    #[test]
    fn the_southern_hemisphere_has_the_opposite_season() {
        assert_eq!(season(date(2026, 10, 28), true).0, Season::Spring);
        assert_eq!(season(date(2026, 1, 15), true).0, Season::Summer);
        assert_eq!(season(date(2026, 4, 1), true).0, Season::Autumn);
        assert_eq!(season(date(2026, 7, 1), true).0, Season::Winter);
    }

    #[test]
    fn climate_follows_the_latitude() {
        let four = |southern| Climate::FourSeasons { southern };
        assert_eq!(Climate::at(52.04), four(false), "Milton Keynes");
        assert_eq!(Climate::at(25.20), four(false), "Dubai, just outside");
        assert_eq!(Climate::at(78.22), four(false), "Svalbard");
        assert_eq!(Climate::at(-33.87), four(true), "Sydney");
        assert_eq!(Climate::at(21.42), Climate::Tropical, "Makkah");
        assert_eq!(Climate::at(1.35), Climate::Tropical, "Singapore");
        assert_eq!(Climate::at(-6.79), Climate::Tropical, "Dar es Salaam");
    }

    #[test]
    fn the_tropics_get_the_year_but_no_four_seasons() {
        let text = format(date(2026, 10, 28), 21.42);
        assert!(
            text.starts_with("🌴 No spring, summer, autumn or winter"),
            "{text}"
        );
        assert!(!text.contains("Autumn"), "{text}");
        assert!(text.ends_with("82% done · 65d to 2027"), "{text}");
        // Sydney: spring in October.
        assert!(format(date(2026, 10, 28), -33.87).starts_with("🌸 *Spring*"));
    }

    #[test]
    fn year_progress() {
        assert_eq!(
            year(date(2026, 10, 28)),
            Stretch {
                day: 301,
                length: 365,
                days_left: 65
            }
        );
        assert_eq!(year(date(2028, 12, 31)).length, 366, "leap year");
        assert_eq!(year(date(2026, 12, 31)).days_left, 1);
    }

    fn utc(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn day_progress_on_the_persons_own_clock() {
        let london = chrono_tz::Europe::London;
        // 18:00 GMT: 18 of 24 hours gone.
        let line = day_line(utc("2026-10-28T18:00:00Z"), london).unwrap();
        assert_eq!(line, "⏳ ■■■■■■■■□□ 75% through the day · 6h 00m left");
        // The same moment is 22:00 in Dubai (UTC+4).
        let dubai = day(utc("2026-10-28T18:00:00Z"), chrono_tz::Asia::Dubai).unwrap();
        assert_eq!((dubai.day, dubai.days_left), (22 * 60, 2 * 60));
        // In BST, 07:30 UTC is 08:30 local.
        let summer = day(utc("2026-07-01T07:30:00Z"), london).unwrap();
        assert_eq!(summer.day, 8 * 60 + 30);
    }

    #[test]
    fn clock_change_days_are_23_and_25_hours() {
        let london = chrono_tz::Europe::London;
        // Clocks go forward on 29 Mar 2026 and back on 25 Oct 2026.
        let spring = day(utc("2026-03-29T12:00:00Z"), london).unwrap();
        assert_eq!(spring.length, 23 * 60);
        let autumn = day(utc("2026-10-25T12:00:00Z"), london).unwrap();
        assert_eq!(autumn.length, 25 * 60);
        assert_eq!(autumn.day + autumn.days_left, autumn.length);
    }

    #[test]
    fn a_day_starting_at_1am_after_a_midnight_clock_change() {
        // Chile's clocks go forward at midnight: 6 Sep 2026 starts at 01:00.
        let santiago = chrono_tz::America::Santiago;
        let d = day(utc("2026-09-06T12:00:00Z"), santiago).unwrap();
        assert_eq!(d.length, 23 * 60, "{d:?}");
    }

    #[test]
    fn formatted_like_the_rest_of_weather() {
        let text = format(date(2026, 10, 28), 52.04);
        assert_eq!(
            text,
            "🍂 *Autumn* (day 58 of 91)\n■■■■■■□□□□ 64% done · 34d to Winter\n\n\
             🗓️ *2026* (day 301 of 365)\n■■■■■■■■□□ 82% done · 65d to 2027"
        );
    }
}

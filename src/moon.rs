//! The moon: its phase, how much of it is lit, and when the next new and
//! full moons are.
//!
//! Worked out here rather than asked of an API, so `/prayers` can show the
//! phase without another network call, and it's all testable. It uses the
//! biggest terms of the standard formulas for the sun and moon (Jean Meeus,
//! *Astronomical Algorithms*, chapters 25 and 47), which put the phases
//! within about a quarter of an hour: plenty for a chat message.
//!
//! Moonrise and moonset come from Open-Meteo (see `Weather::moon_times`).

use std::fmt;

use chrono::{DateTime, Duration, NaiveDateTime, Utc};

use crate::prayer::HijriDate;
use crate::settings::Settings;

/// 2000-01-01 12:00 UTC, the starting point ("J2000") of the formulas below.
const J2000: DateTime<Utc> = DateTime::from_timestamp(946_728_000, 0).unwrap();

/// The average time from one new moon to the next, in days.
pub const SYNODIC_MONTH: f64 = 29.530_589;

/// Days (with fractions) since J2000.
fn days_since_j2000(time: DateTime<Utc>) -> f64 {
    (time - J2000).num_seconds() as f64 / 86_400.0
}

/// `sin` of an angle in degrees (Rust's `sin` takes radians).
fn sin_deg(degrees: f64) -> f64 {
    degrees.to_radians().sin()
}

/// How far the moon is ahead of the sun around the sky, 0-360°: 0 is a new
/// moon, 90 first quarter, 180 full, 270 last quarter.
fn elongation(time: DateTime<Utc>) -> f64 {
    let d = days_since_j2000(time);

    // The sun: its mean longitude, corrected by its mean anomaly `m` (the
    // Earth's orbit isn't a circle).
    let m = 357.529_109_2 + 0.985_600_28 * d;
    let sun = 280.466_46 + 0.985_647_36 * d + 1.914_602 * sin_deg(m) + 0.019_993 * sin_deg(2.0 * m);

    // The moon: its mean longitude plus the largest periodic terms.
    // `e` is the mean elongation, `mm` the moon's mean anomaly and `f` its
    // distance from its node.
    let e = 297.850_192_1 + 12.190_749_12 * d;
    let mm = 134.963_396_4 + 13.064_992_95 * d;
    let f = 93.272_095 + 13.229_350_24 * d;
    let moon = 218.316_447_7
        + 13.176_396_48 * d
        + 6.288_774 * sin_deg(mm)
        + 1.274_027 * sin_deg(2.0 * e - mm)
        + 0.658_314 * sin_deg(2.0 * e)
        + 0.213_618 * sin_deg(2.0 * mm)
        - 0.185_116 * sin_deg(m)
        - 0.114_332 * sin_deg(2.0 * f)
        + 0.058_793 * sin_deg(2.0 * e - 2.0 * mm)
        + 0.057_066 * sin_deg(2.0 * e - m - mm)
        + 0.053_322 * sin_deg(2.0 * e + mm)
        + 0.045_758 * sin_deg(2.0 * e - m)
        - 0.040_923 * sin_deg(m - mm)
        - 0.034_720 * sin_deg(e)
        - 0.030_383 * sin_deg(m + mm);

    // `rem_euclid` is `%` that never goes negative: -10 becomes 350.
    (moon - sun).rem_euclid(360.0)
}

/// The eight named phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    New,
    WaxingCrescent,
    FirstQuarter,
    WaxingGibbous,
    Full,
    WaningGibbous,
    LastQuarter,
    WaningCrescent,
}

const NEW: &str = "🌑";
const WAXING_CRESCENT: &str = "🌒";
const FIRST_QUARTER: &str = "🌓";
const WAXING_GIBBOUS: &str = "🌔";
const FULL: &str = "🌕";
const WANING_GIBBOUS: &str = "🌖";
const LAST_QUARTER: &str = "🌗";
const WANING_CRESCENT: &str = "🌘";
const CRESCENT: &str = "🌙";
const RISE: &str = "⬆️";
const SET: &str = "⬇️";

impl Phase {
    /// In order around the month, so `ALL[i]` is the phase centred on
    /// `i * 45°`.
    const ALL: [Phase; 8] = [
        Phase::New,
        Phase::WaxingCrescent,
        Phase::FirstQuarter,
        Phase::WaxingGibbous,
        Phase::Full,
        Phase::WaningGibbous,
        Phase::LastQuarter,
        Phase::WaningCrescent,
    ];

    pub fn emoji(self) -> &'static str {
        match self {
            Phase::New => NEW,
            Phase::WaxingCrescent => WAXING_CRESCENT,
            Phase::FirstQuarter => FIRST_QUARTER,
            Phase::WaxingGibbous => WAXING_GIBBOUS,
            Phase::Full => FULL,
            Phase::WaningGibbous => WANING_GIBBOUS,
            Phase::LastQuarter => LAST_QUARTER,
            Phase::WaningCrescent => WANING_CRESCENT,
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            Phase::New => "New moon",
            Phase::WaxingCrescent => "Waxing crescent",
            Phase::FirstQuarter => "First quarter",
            Phase::WaxingGibbous => "Waxing gibbous",
            Phase::Full => "Full moon",
            Phase::WaningGibbous => "Waning gibbous",
            Phase::LastQuarter => "Last quarter",
            Phase::WaningCrescent => "Waning crescent",
        })
    }
}

/// The moon at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Moon {
    /// See `elongation`: 0 new, 180 full.
    pub elongation: f64,
}

impl Moon {
    pub fn at(time: DateTime<Utc>) -> Moon {
        Moon {
            elongation: elongation(time),
        }
    }

    /// The nearest of the eight phases: each covers 45°, centred on its
    /// own angle, so "Full moon" is 157.5° to 202.5° (about a day and a
    /// half either side).
    pub fn phase(self) -> Phase {
        // + 22.5 so each phase's slice starts half a slice before its
        // centre; `% 8` wraps 337.5°-360° back round to New.
        let slice = ((self.elongation + 22.5) / 45.0) as usize % 8;
        Phase::ALL[slice]
    }

    /// How much of the side facing us is lit, 0.0 to 1.0.
    pub fn lit(self) -> f64 {
        (1.0 - self.elongation.to_radians().cos()) / 2.0
    }
}

/// The first time after `after` that the moon reaches `target` degrees
/// (0 for a new moon, 180 for full).
///
/// The elongation always grows (about 12° a day), so this steps forward six
/// hours at a time until it passes `target`, then halves the step until
/// it's within a minute.
pub fn next_elongation(after: DateTime<Utc>, target: f64) -> DateTime<Utc> {
    // How far past `target` the moon is, in -180..180: negative before it,
    // positive after.
    let past = |time| (elongation(time) - target + 180.0).rem_euclid(360.0) - 180.0;

    let step = Duration::hours(6);
    let mut before = after;
    // 31 days of six-hour steps is more than a month, so this always ends.
    for _ in 0..124 {
        let next = before + step;
        if past(before) < 0.0 && past(next) >= 0.0 {
            let mut after_it = next;
            while after_it - before > Duration::minutes(1) {
                let middle = before + (after_it - before) / 2;
                if past(middle) < 0.0 {
                    before = middle;
                } else {
                    after_it = middle;
                }
            }
            return after_it;
        }
        before = next;
    }
    // Only reachable if the formulas above were broken.
    unreachable!("the moon passes every angle within a month")
}

pub fn next_new_moon(after: DateTime<Utc>) -> DateTime<Utc> {
    next_elongation(after, 0.0)
}

pub fn next_full_moon(after: DateTime<Utc>) -> DateTime<Utc> {
    next_elongation(after, 180.0)
}

/// Moonrise and moonset on one day, in local time. Either can be missing:
/// the moon rises about 50 minutes later each day, so some days it doesn't
/// rise (or set) at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiseSet {
    pub rise: Option<NaiveDateTime>,
    pub set: Option<NaiveDateTime>,
}

/// The `/moon` reply:
///
/// ```text
/// 🌖 *Moon* (Milton Keynes, Wed 28 Oct)
/// Waning gibbous, 93% lit
/// 🌙 17 Jumada al-Ula 1448 AH
/// ⬆️ Rises 17:24 · ⬇️ Sets 10:21
///
/// 🌑 New moon: Mon 9 Nov, 07:02
/// 🌕 Full moon: Tue 24 Nov, 14:53
/// ```
///
/// `rise_set` and `hijri` are left out when they couldn't be fetched.
pub fn format(
    now: DateTime<Utc>,
    settings: &Settings,
    rise_set: Option<RiseSet>,
    hijri: Option<HijriDate>,
) -> String {
    let moon = Moon::at(now);
    let phase = moon.phase();
    let day = |time: DateTime<Utc>| {
        time.with_timezone(&settings.timezone)
            .format("%a %-d %b")
            .to_string()
    };

    let mut text = format!(
        "{} *Moon* ({}, {})\n{phase}, {:.0}% lit\n",
        phase.emoji(),
        settings.city,
        day(now),
        moon.lit() * 100.0
    );
    if let Some(hijri) = hijri {
        text.push_str(&format!("{CRESCENT} {hijri}\n"));
    }
    if let Some(times) = rise_set {
        let time = |t: Option<NaiveDateTime>| match t {
            Some(t) => t.format("%H:%M").to_string(),
            None => "not today".to_string(),
        };
        text.push_str(&format!(
            "{RISE} Rises {} · {SET} Sets {}\n",
            time(times.rise),
            time(times.set)
        ));
    }

    // Whichever comes first goes first.
    let mut next = [
        (NEW, "New moon", next_new_moon(now)),
        (FULL, "Full moon", next_full_moon(now)),
    ];
    next.sort_by_key(|(_, _, when)| *when);
    text.push('\n');
    for (emoji, name, when) in next {
        text.push_str(&format!(
            "{emoji} {name}: {}, {}\n",
            day(when),
            settings.clock(when)
        ));
    }
    text.push_str(
        "\nA new Islamic month starts once the new crescent is seen, \
         usually a day or two after the new moon.",
    );
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    /// Minutes between two times, either way round.
    fn minutes_apart(a: DateTime<Utc>, b: DateTime<Utc>) -> i64 {
        (a - b).num_minutes().abs()
    }

    // Reference times from the US Naval Observatory.
    #[test]
    fn new_and_full_moons_match_the_naval_observatory() {
        let cases = [
            (
                next_full_moon(utc("2026-10-20T00:00:00Z")),
                "2026-10-26T04:12:00Z",
            ),
            (
                next_new_moon(utc("2026-10-28T12:00:00Z")),
                "2026-11-09T07:02:00Z",
            ),
            (
                next_full_moon(utc("2026-10-28T12:00:00Z")),
                "2026-11-24T14:53:00Z",
            ),
            (
                next_new_moon(utc("2026-11-10T00:00:00Z")),
                "2026-12-09T00:52:00Z",
            ),
            (
                next_new_moon(utc("2026-01-01T00:00:00Z")),
                "2026-01-18T19:52:00Z",
            ),
            (
                next_full_moon(utc("2026-02-20T00:00:00Z")),
                "2026-03-03T11:38:00Z",
            ),
        ];
        for (found, expected) in cases {
            let apart = minutes_apart(found, utc(expected));
            assert!(apart <= 20, "{found} vs {expected}: {apart} min apart");
        }
    }

    #[test]
    fn quarters_are_where_they_should_be() {
        // First quarter 26 Jan 04:47, last quarter 1 Nov 20:28.
        let first = Moon::at(utc("2026-01-26T04:47:00Z"));
        assert!((first.elongation - 90.0).abs() < 0.5, "{first:?}");
        assert!((first.lit() - 0.5).abs() < 0.01);
        let last = Moon::at(utc("2026-11-01T20:28:00Z"));
        assert!((last.elongation - 270.0).abs() < 0.5, "{last:?}");
    }

    #[test]
    fn phase_today_matches_the_naval_observatory() {
        // USNO for 28 Oct 2026: "Waning Gibbous", 92% lit.
        let moon = Moon::at(utc("2026-10-28T12:00:00Z"));
        assert_eq!(moon.phase(), Phase::WaningGibbous);
        // Within a couple of points: `lit` ignores how far the moon is
        // above or below the sun's path, which USNO includes.
        assert!((moon.lit() * 100.0 - 92.0).abs() <= 2.0, "{}", moon.lit());
    }

    #[test]
    fn phases_are_named_by_the_nearest_eighth() {
        let phase = |elongation| Moon { elongation }.phase();
        assert_eq!(phase(0.0), Phase::New);
        assert_eq!(phase(359.0), Phase::New, "wraps round");
        assert_eq!(phase(22.4), Phase::New);
        assert_eq!(phase(22.6), Phase::WaxingCrescent);
        assert_eq!(phase(90.0), Phase::FirstQuarter);
        assert_eq!(phase(180.0), Phase::Full);
        assert_eq!(phase(200.0), Phase::Full);
        assert_eq!(phase(210.0), Phase::WaningGibbous);
        assert_eq!(phase(300.0), Phase::WaningCrescent);
    }

    #[test]
    fn every_phase_has_its_own_emoji() {
        let emoji: Vec<_> = Phase::ALL.iter().map(|p| p.emoji()).collect();
        assert_eq!(emoji.join(""), "🌑🌒🌓🌔🌕🌖🌗🌘");
    }

    #[test]
    fn moon_reply_shows_phase_times_and_whats_next() {
        let settings = crate::settings::tests::defaults();
        let at = |s: &str| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap();
        let times = RiseSet {
            rise: Some(at("2026-10-28 17:24")),
            set: None,
        };
        let hijri = HijriDate {
            day: 17,
            month: 5,
            year: 1448,
        };
        let text = format(
            utc("2026-10-28T12:00:00Z"),
            &settings,
            Some(times),
            Some(hijri),
        );

        assert!(
            text.starts_with(
                "🌖 *Moon* (Milton Keynes, Wed 28 Oct)\nWaning gibbous, 93% lit\n\
                 🌙 17 Jumada al-Ula 1448 AH\n⬆️ Rises 17:24 · ⬇️ Sets not today\n"
            ),
            "{text}"
        );
        // New moon first: it comes before the next full moon. Within the
        // test's 20 minutes of USNO's 07:02 and 14:53 (so 06:42-07:22, 14:33-15:13).
        let new = text.find("🌑 New moon: Mon 9 Nov, 0").expect(&text);
        let full = text.find("🌕 Full moon: Tue 24 Nov, 1").expect(&text);
        assert!(new < full, "{text}");
    }

    #[test]
    fn moon_reply_without_the_network_still_has_the_phase() {
        let settings = crate::settings::tests::defaults();
        // Two days before full: the full moon is listed first.
        let text = format(utc("2026-10-24T04:00:00Z"), &settings, None, None);
        assert!(text.contains("Waxing gibbous"), "{text}");
        assert!(!text.contains("Rises") && !text.contains("AH"), "{text}");
        let full = text.find("🌕 Full moon: Mon 26 Oct").expect(&text);
        let new = text.find("🌑 New moon: Mon 9 Nov").expect(&text);
        assert!(full < new, "{text}");
    }

    #[test]
    fn right_on_a_phase_finds_the_next_one() {
        let full = next_full_moon(utc("2026-10-20T00:00:00Z"));
        let next = next_full_moon(full + Duration::minutes(1));
        let days = (next - full).num_hours() as f64 / 24.0;
        assert!((days - SYNODIC_MONTH).abs() < 1.0, "{days} days later");
    }
}

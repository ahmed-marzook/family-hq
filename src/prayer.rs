//! The five daily prayers, and the rules for when to remind people.
//!
//! Pure functions only: no clock, no network, no database. The current
//! time is always passed in, so every rule can be tested with any time.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Europe::London;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prayer {
    Fajr,
    Dhuhr,
    Asr,
    Maghrib,
    Isha,
}

impl Prayer {
    /// All five, in the order they happen in a day.
    pub const ALL: [Prayer; 5] = [
        Prayer::Fajr,
        Prayer::Dhuhr,
        Prayer::Asr,
        Prayer::Maghrib,
        Prayer::Isha,
    ];

    /// The exact text stored in the database (and allowed by its CHECK).
    pub fn as_db_str(self) -> &'static str {
        match self {
            Prayer::Fajr => "fajr",
            Prayer::Dhuhr => "dhuhr",
            Prayer::Asr => "asr",
            Prayer::Maghrib => "maghrib",
            Prayer::Isha => "isha",
        }
    }
}

/// For people: "Fajr", "Dhuhr", ...
impl fmt::Display for Prayer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let name = match self {
            Prayer::Fajr => "Fajr",
            Prayer::Dhuhr => "Dhuhr",
            Prayer::Asr => "Asr",
            Prayer::Maghrib => "Maghrib",
            Prayer::Isha => "Isha",
        };
        f.pad(name)
    }
}

/// Reads the database text back (`"asr"` → `Prayer::Asr`).
impl FromStr for Prayer {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Find the prayer whose database text matches.
        Prayer::ALL
            .into_iter()
            .find(|p| p.as_db_str() == s)
            .ok_or_else(|| format!("unknown prayer '{s}'"))
    }
}

/// One day's prayer start times.
#[derive(Debug, Clone, PartialEq)]
pub struct DayTimes {
    /// The day, in Europe/London.
    pub date: NaiveDate,
    /// When each prayer starts, in UTC, in `Prayer::ALL` order.
    pub times: [(Prayer, DateTime<Utc>); 5],
}

/// How long after a prayer starts a missed reminder is still worth sending
/// (say the server was restarting at the time).
pub const LATE_REMINDER_GRACE: Duration = Duration::minutes(30);

/// "Today" for the family: the date in Europe/London, not UTC. Between
/// midnight and 1am in summer the two differ.
pub fn london_date(now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&London).date_naive()
}

/// A time as the family reads it: "14:16" on the London clock.
pub fn london_hhmm(time: DateTime<Utc>) -> String {
    time.with_timezone(&London).format("%H:%M").to_string()
}

/// The prayers whose reminder should go out at `now`.
///
/// A reminder is due from `lead` before the prayer starts until
/// `LATE_REMINDER_GRACE` after. Whether it was already sent is the
/// database's job (`prayer_reminders` allows one per prayer per chat).
pub fn due_reminders(
    day: &DayTimes,
    now: DateTime<Utc>,
    lead: Duration,
) -> Vec<(Prayer, DateTime<Utc>)> {
    day.times
        .iter()
        .filter(|(_, start)| *start - lead <= now && now < *start + LATE_REMINDER_GRACE)
        .copied()
        .collect()
}

/// The reminder message for one prayer.
pub fn reminder_text(prayer: Prayer, start: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let at = london_hhmm(start);
    let minutes = (start - now).num_minutes();
    let when = if minutes > 0 {
        format!("at {at}, in {minutes} min")
    } else {
        format!("started at {at}")
    };
    format!("🕌 *{prayer}* {when}\nReact to this message once you've prayed.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    /// Milton Keynes, 27 Oct 2026 (GMT, so London time = UTC).
    fn day() -> DayTimes {
        DayTimes {
            date: NaiveDate::from_ymd_opt(2026, 10, 27).unwrap(),
            times: [
                (Prayer::Fajr, utc("2026-10-27T04:55:00Z")),
                (Prayer::Dhuhr, utc("2026-10-27T11:47:00Z")),
                (Prayer::Asr, utc("2026-10-27T14:16:00Z")),
                (Prayer::Maghrib, utc("2026-10-27T16:44:00Z")),
                (Prayer::Isha, utc("2026-10-27T18:31:00Z")),
            ],
        }
    }

    fn due_at(now: &str) -> Vec<Prayer> {
        due_reminders(&day(), utc(now), Duration::minutes(10))
            .into_iter()
            .map(|(p, _)| p)
            .collect()
    }

    #[test]
    fn db_text_round_trips() {
        for prayer in Prayer::ALL {
            assert_eq!(prayer.as_db_str().parse::<Prayer>(), Ok(prayer));
        }
        assert!("sunrise".parse::<Prayer>().is_err());
    }

    #[test]
    fn reminder_window_opens_lead_minutes_before() {
        assert_eq!(due_at("2026-10-27T14:05:59Z"), []);
        assert_eq!(due_at("2026-10-27T14:06:00Z"), [Prayer::Asr]);
        assert_eq!(due_at("2026-10-27T14:16:00Z"), [Prayer::Asr]);
    }

    #[test]
    fn late_reminder_still_due_within_grace_then_dropped() {
        assert_eq!(due_at("2026-10-27T14:45:59Z"), [Prayer::Asr]);
        assert_eq!(due_at("2026-10-27T14:46:00Z"), []);
    }

    #[test]
    fn nothing_due_between_prayers() {
        assert_eq!(due_at("2026-10-27T09:00:00Z"), []);
        assert_eq!(due_at("2026-10-27T23:30:00Z"), []);
    }

    #[test]
    fn london_date_differs_from_utc_after_midnight_in_summer() {
        // 23:30 UTC on 1 July is 00:30 BST on 2 July.
        let now = utc("2026-07-01T23:30:00Z");
        assert_eq!(
            london_date(now),
            NaiveDate::from_ymd_opt(2026, 7, 2).unwrap()
        );
        assert_eq!(london_hhmm(now), "00:30");
    }

    #[test]
    fn reminder_text_before_and_after_start() {
        let start = utc("2026-10-27T14:16:00Z");
        assert_eq!(
            reminder_text(Prayer::Asr, start, utc("2026-10-27T14:06:00Z")),
            "🕌 *Asr* at 14:16, in 10 min\nReact to this message once you've prayed."
        );
        assert!(
            reminder_text(Prayer::Asr, start, utc("2026-10-27T14:20:00Z"))
                .starts_with("🕌 *Asr* started at 14:16")
        );
    }
}

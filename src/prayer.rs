//! The five daily prayers, and the rules for when to remind people.
//!
//! Pure functions only: no clock, no network, no database. The current
//! time is always passed in, so every rule can be tested with any time.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Tz;

use crate::settings::Settings;

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

/// Where and how prayer times are calculated. Also the cache key for
/// fetched times: two chats with equal `Location`s share one fetch, which
/// is what `Hash` + `Eq` allow (a `HashMap` key needs both).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Location {
    pub city: String,
    /// Country name or 2-letter code, e.g. "GB".
    pub country: String,
    /// AlAdhan calculation method, e.g. 3 = Muslim World League.
    pub method: u8,
    /// Asr: `SHAFII` or `HANAFI`.
    pub school: u8,
    /// The timezone the day and times are asked for in.
    pub timezone: Tz,
}

impl Location {
    /// One text key for "same place, same times", e.g.
    /// `Milton Keynes|GB|3|0|Europe/London`. Group reminders store it, so a
    /// group with members in two countries gets one reminder per place.
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.city, self.country, self.method, self.school, self.timezone
        )
    }
}

/// AlAdhan's `school` values for Asr. Shafi'i is AlAdhan's "standard".
pub const SHAFII: u8 = 0;
pub const HANAFI: u8 = 1;

/// AlAdhan's calculation methods, by number
/// (<https://aladhan.com/calculation-methods>). 99 ("custom") needs angle
/// settings we don't store, so it's left out.
pub const METHODS: [(u8, &str); 23] = [
    (0, "Shia Ithna-Ashari (Jafari)"),
    (1, "University of Islamic Sciences, Karachi"),
    (2, "Islamic Society of North America (ISNA)"),
    (3, "Muslim World League"),
    (4, "Umm Al-Qura University, Makkah"),
    (5, "Egyptian General Authority of Survey"),
    (7, "Institute of Geophysics, University of Tehran"),
    (8, "Gulf Region"),
    (9, "Kuwait"),
    (10, "Qatar"),
    (11, "Majlis Ugama Islam Singapura, Singapore"),
    (12, "Union Organization Islamic de France"),
    (13, "Diyanet İşleri Başkanlığı, Turkey"),
    (14, "Spiritual Administration of Muslims of Russia"),
    (15, "Moonsighting Committee Worldwide"),
    (16, "Dubai"),
    (17, "JAKIM, Malaysia"),
    (18, "Tunisia"),
    (19, "Algeria"),
    (20, "KEMENAG, Indonesia"),
    (21, "Morocco"),
    (22, "Comunidade Islamica de Lisboa"),
    (23, "Ministry of Awqaf, Jordan"),
];

/// The name of method `id`, or `None` if AlAdhan has no such method.
pub fn method_name(id: u8) -> Option<&'static str> {
    METHODS
        .iter()
        .find(|(m, _)| *m == id)
        .map(|(_, name)| *name)
}

/// "Shafi'i" or "Hanafi".
pub fn school_name(school: u8) -> &'static str {
    if school == HANAFI {
        "Hanafi"
    } else {
        "Shafi'i"
    }
}

/// When one prayer's time starts and ends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    pub prayer: Prayer,
    pub start: DateTime<Utc>,
    /// Fajr ends at sunrise, Isha at Islamic midnight, the others when the
    /// next prayer starts.
    pub end: DateTime<Utc>,
}

/// One day's prayer times.
#[derive(Debug, Clone, PartialEq)]
pub struct DayTimes {
    /// The day, in the chat's timezone.
    pub date: NaiveDate,
    /// The five windows, in UTC, in `Prayer::ALL` order.
    pub windows: [Window; 5],
}

/// The two reminders each prayer gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderKind {
    /// The prayer's time has begun.
    Start,
    /// Its time ends soon (`Settings::ending_minutes` before the end).
    Ending,
}

impl ReminderKind {
    /// The exact text stored in `prayer_reminders.kind`.
    pub fn as_db_str(self) -> &'static str {
        match self {
            ReminderKind::Start => "start",
            ReminderKind::Ending => "ending",
        }
    }
}

/// How long after its moment a missed start reminder is still worth
/// sending (say the server was restarting at the time).
pub const LATE_REMINDER_GRACE: Duration = Duration::minutes(30);

/// The reminders that should go out at `now`.
///
/// * Start: from the start until `LATE_REMINDER_GRACE` after (but not
///   once the "ending" one is due, which would make it pointless).
/// * Ending: from `ending_minutes` before the end until the end itself.
///   After the end it's too late to be useful, so it's skipped.
///
/// Whether one was already sent is the database's job (`prayer_reminders`
/// allows one of each kind per prayer per chat).
pub fn due_reminders(
    day: &DayTimes,
    now: DateTime<Utc>,
    ending_minutes: i64,
) -> Vec<(ReminderKind, Window)> {
    let mut due = Vec::new();
    for window in day.windows {
        let ending_from = window.end - Duration::minutes(ending_minutes);
        if window.start <= now && now < window.start + LATE_REMINDER_GRACE && now < ending_from {
            due.push((ReminderKind::Start, window));
        }
        if ending_from <= now && now < window.end {
            due.push((ReminderKind::Ending, window));
        }
    }
    due
}

/// The reminder message.
///
/// `place` names the city when it matters (in a group whose members are in
/// different places): "has started in Colombo". `None` leaves it out.
pub fn reminder_text(
    kind: ReminderKind,
    window: &Window,
    now: DateTime<Utc>,
    settings: &Settings,
    place: Option<&str>,
) -> String {
    let prayer = window.prayer;
    let (start, end) = (settings.clock(window.start), settings.clock(window.end));
    // `map_or(default, f)`: "" without a place, " in Colombo" with one.
    let place = place.map_or(String::new(), |p| format!(" in {p}"));
    let first_line = match kind {
        ReminderKind::Start => {
            format!("🕌 *{prayer}* has started{place} ({start}). It ends at {end}.")
        }
        ReminderKind::Ending => {
            // Rounded up, so 19m30s left reads "20 min", not "19 min".
            let seconds = (window.end - now).num_seconds().max(0);
            let minutes = (seconds + 59) / 60;
            format!("⏳ *{prayer}* ends in {minutes} min{place} ({end}).")
        }
    };
    format!("{first_line}\nReact to this message once you've prayed.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::tests::defaults;

    fn utc(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn window(prayer: Prayer, start: &str, end: &str) -> Window {
        Window {
            prayer,
            start: utc(start),
            end: utc(end),
        }
    }

    /// Milton Keynes, 27 Oct 2026 (GMT, so London time = UTC).
    fn day() -> DayTimes {
        DayTimes {
            date: NaiveDate::from_ymd_opt(2026, 10, 27).unwrap(),
            windows: [
                window(Prayer::Fajr, "2026-10-27T04:55:00Z", "2026-10-27T06:49:00Z"),
                window(
                    Prayer::Dhuhr,
                    "2026-10-27T11:47:00Z",
                    "2026-10-27T14:16:00Z",
                ),
                window(Prayer::Asr, "2026-10-27T14:16:00Z", "2026-10-27T16:44:00Z"),
                window(
                    Prayer::Maghrib,
                    "2026-10-27T16:44:00Z",
                    "2026-10-27T18:31:00Z",
                ),
                window(Prayer::Isha, "2026-10-27T18:31:00Z", "2026-10-27T23:46:00Z"),
            ],
        }
    }

    /// What's due at `now` with the default 20 minutes, as (prayer, kind).
    fn due_at(now: &str) -> Vec<(Prayer, ReminderKind)> {
        due_reminders(&day(), utc(now), 20)
            .into_iter()
            .map(|(kind, w)| (w.prayer, kind))
            .collect()
    }

    #[test]
    fn method_and_school_names() {
        assert_eq!(method_name(3), Some("Muslim World League"));
        assert_eq!(method_name(6), None, "AlAdhan has no method 6");
        assert_eq!(method_name(99), None, "custom isn't supported");
        assert_eq!(school_name(SHAFII), "Shafi'i");
        assert_eq!(school_name(HANAFI), "Hanafi");
    }

    #[test]
    fn db_text_round_trips() {
        for prayer in Prayer::ALL {
            assert_eq!(prayer.as_db_str().parse::<Prayer>(), Ok(prayer));
        }
        assert!("sunrise".parse::<Prayer>().is_err());
    }

    #[test]
    fn start_reminder_from_the_start_for_30_minutes() {
        // Just before Asr starts, Dhuhr is about to end: its "ending" one is due.
        assert_eq!(
            due_at("2026-10-27T14:15:59Z"),
            [(Prayer::Dhuhr, ReminderKind::Ending)]
        );
        assert_eq!(
            due_at("2026-10-27T14:16:00Z"),
            [(Prayer::Asr, ReminderKind::Start)]
        );
        assert_eq!(
            due_at("2026-10-27T14:45:59Z"),
            [(Prayer::Asr, ReminderKind::Start)]
        );
        assert_eq!(due_at("2026-10-27T14:46:00Z"), []);
    }

    #[test]
    fn ending_reminder_from_20_minutes_before_the_end_until_the_end() {
        assert_eq!(due_at("2026-10-27T16:23:59Z"), []);
        assert_eq!(
            due_at("2026-10-27T16:24:00Z"),
            [(Prayer::Asr, ReminderKind::Ending)]
        );
        assert_eq!(
            due_at("2026-10-27T16:43:59Z"),
            [(Prayer::Asr, ReminderKind::Ending)]
        );
        // 16:44 is Asr's end and Maghrib's start.
        assert_eq!(
            due_at("2026-10-27T16:44:00Z"),
            [(Prayer::Maghrib, ReminderKind::Start)]
        );
    }

    #[test]
    fn fajr_ends_at_sunrise() {
        assert_eq!(
            due_at("2026-10-27T06:30:00Z"),
            [(Prayer::Fajr, ReminderKind::Ending)]
        );
        assert_eq!(due_at("2026-10-27T06:49:00Z"), []);
    }

    #[test]
    fn a_window_shorter_than_the_warning_skips_the_start_reminder() {
        // A copy of the normal day with a 15-minute Fajr. (`Window` is
        // `Copy`, so the array is copied and changing it leaves `day()` alone.)
        let mut short = day();
        short.windows[0] = window(Prayer::Fajr, "2026-10-27T05:00:00Z", "2026-10-27T05:15:00Z");
        // Already inside the last 20 minutes when it starts: only "ending".
        let due = due_reminders(&short, utc("2026-10-27T05:00:00Z"), 20);
        let kinds: Vec<ReminderKind> = due.iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, [ReminderKind::Ending]);
    }

    #[test]
    fn nothing_due_between_reminders() {
        assert_eq!(due_at("2026-10-27T09:00:00Z"), []);
        assert_eq!(due_at("2026-10-27T15:30:00Z"), []);
    }

    #[test]
    fn reminder_text_can_name_the_place() {
        let asr = day().windows[2];
        let text = reminder_text(
            ReminderKind::Start,
            &asr,
            utc("2026-10-27T14:16:00Z"),
            &defaults(),
            Some("Colombo"),
        );
        assert!(
            text.starts_with("🕌 *Asr* has started in Colombo (14:16)."),
            "{text}"
        );
    }

    #[test]
    fn reminder_texts() {
        let asr = day().windows[2];
        assert_eq!(
            reminder_text(
                ReminderKind::Start,
                &asr,
                utc("2026-10-27T14:16:00Z"),
                &defaults(),
                None
            ),
            "🕌 *Asr* has started (14:16). It ends at 16:44.\nReact to this message once you've prayed."
        );
        assert_eq!(
            reminder_text(
                ReminderKind::Ending,
                &asr,
                utc("2026-10-27T16:24:30Z"),
                &defaults(),
                None
            ),
            "⏳ *Asr* ends in 20 min (16:44).\nReact to this message once you've prayed."
        );
    }
}

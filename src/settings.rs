//! Per-chat settings: timezone, location and prayer preferences.
//!
//! A DM's settings are that person's own; a group's are shared by its
//! members. Anything a chat hasn't set comes from the server defaults
//! (`.env`). Pure code: the database side is `repo::settings`.

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;

use crate::prayer::Location;

/// One chat's complete settings, after filling in defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Decides what "today" means and how times are shown.
    pub timezone: Tz,
    pub city: String,
    /// Country name or 2-letter code, e.g. "GB".
    pub country: String,
    /// AlAdhan calculation method, e.g. 3 = Muslim World League.
    pub prayer_method: u8,
    /// Asr: `prayer::SHAFII` or `prayer::HANAFI`.
    pub prayer_school: u8,
    /// How many minutes before a prayer's time ends the "ending soon"
    /// reminder goes out.
    pub prayer_ending_minutes: i64,
}

impl Settings {
    /// The chat's date at `now`. Between midnight and 1am in a UK summer,
    /// London's date is a day ahead of UTC's, for example.
    pub fn today(&self, now: DateTime<Utc>) -> NaiveDate {
        now.with_timezone(&self.timezone).date_naive()
    }

    /// A time as the chat reads it: "14:16" on its own clock.
    pub fn clock(&self, time: DateTime<Utc>) -> String {
        time.with_timezone(&self.timezone)
            .format("%H:%M")
            .to_string()
    }

    /// What AlAdhan needs to calculate this chat's prayer times.
    pub fn prayer_location(&self) -> Location {
        Location {
            city: self.city.clone(),
            country: self.country.clone(),
            method: self.prayer_method,
            school: self.prayer_school,
            timezone: self.timezone,
        }
    }
}

/// A chat's own choices, as stored. `None` means "use the default", so a
/// chat only stores what it changed, and a new server default reaches
/// every chat that didn't choose its own.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Overrides {
    pub timezone: Option<String>,
    pub city: Option<String>,
    pub country: Option<String>,
    pub prayer_method: Option<i64>,
    pub prayer_school: Option<i64>,
    pub prayer_ending_minutes: Option<i64>,
}

impl Overrides {
    /// The settings this chat actually uses: its own where it has them,
    /// `defaults` everywhere else.
    pub fn apply(&self, defaults: &Settings) -> Settings {
        // City and country only ever change together (a CHECK in the
        // schema), so take both from the same place.
        let (city, country) = match (&self.city, &self.country) {
            (Some(city), Some(country)) => (city.clone(), country.clone()),
            _ => (defaults.city.clone(), defaults.country.clone()),
        };
        Settings {
            // `.parse::<Tz>()` reads an IANA name like "Europe/London". The
            // name was checked when it was saved; if it's somehow unknown
            // now, the default is safer than an error on every message.
            timezone: self
                .timezone
                .as_deref()
                .and_then(|name| name.parse().ok())
                .unwrap_or(defaults.timezone),
            city,
            country,
            // `u8::try_from` fails for values outside 0..=255; the schema's
            // CHECKs make that impossible, but a default beats a panic.
            prayer_method: self
                .prayer_method
                .and_then(|m| u8::try_from(m).ok())
                .unwrap_or(defaults.prayer_method),
            prayer_school: self
                .prayer_school
                .and_then(|s| u8::try_from(s).ok())
                .unwrap_or(defaults.prayer_school),
            prayer_ending_minutes: self
                .prayer_ending_minutes
                .unwrap_or(defaults.prayer_ending_minutes),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::prayer::{HANAFI, SHAFII};

    /// Milton Keynes defaults, shared with other modules' tests.
    pub(crate) fn defaults() -> Settings {
        Settings {
            timezone: chrono_tz::Europe::London,
            city: "Milton Keynes".to_string(),
            country: "GB".to_string(),
            prayer_method: 3,
            prayer_school: SHAFII,
            prayer_ending_minutes: 20,
        }
    }

    #[test]
    fn no_overrides_means_the_defaults() {
        assert_eq!(Overrides::default().apply(&defaults()), defaults());
    }

    #[test]
    fn overrides_replace_only_what_they_set() {
        let own = Overrides {
            city: Some("Paris".to_string()),
            country: Some("FR".to_string()),
            timezone: Some("Europe/Paris".to_string()),
            prayer_school: Some(HANAFI as i64),
            ..Overrides::default()
        };
        let s = own.apply(&defaults());
        assert_eq!((s.city.as_str(), s.country.as_str()), ("Paris", "FR"));
        assert_eq!(s.timezone, chrono_tz::Europe::Paris);
        assert_eq!(s.prayer_school, HANAFI);
        assert_eq!(s.prayer_method, 3, "kept the default");
        assert_eq!(s.prayer_ending_minutes, 20, "kept the default");
    }

    #[test]
    fn unknown_stored_timezone_falls_back_to_the_default() {
        let own = Overrides {
            timezone: Some("Mars/Olympus".to_string()),
            ..Overrides::default()
        };
        assert_eq!(own.apply(&defaults()).timezone, chrono_tz::Europe::London);
    }

    #[test]
    fn today_and_clock_follow_the_timezone() {
        let now: DateTime<Utc> = "2026-07-01T23:30:00Z".parse().unwrap();
        let london = defaults();
        assert_eq!(
            london.today(now),
            NaiveDate::from_ymd_opt(2026, 7, 2).unwrap()
        );
        assert_eq!(london.clock(now), "00:30");

        let new_york = Settings {
            timezone: chrono_tz::America::New_York,
            ..defaults()
        };
        assert_eq!(
            new_york.today(now),
            NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()
        );
        assert_eq!(new_york.clock(now), "19:30");
    }
}

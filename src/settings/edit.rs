//! Changing a chat's settings: one set of rules for `/settings` in WhatsApp
//! and for the admin dashboard, so the two can never disagree about what's
//! allowed.
//!
//! Each caller words its own replies; this module only checks and saves.

use chrono_tz::Tz;
use sqlx::SqliteConnection;

use crate::error::AppError;
use crate::geo::{Geo, Town};
use crate::prayer::{self, HANAFI, SHAFII};
use crate::repo::settings as stored;
use crate::settings::Settings;

/// The shortest and longest "ending soon" warning, in minutes.
pub const ENDING_MINUTES: std::ops::RangeInclusive<i64> = 1..=120;

/// One change to a chat's settings.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// Look the town up, and take its coordinates and timezone with it.
    City {
        city: String,
        country: String,
    },
    /// An IANA name, e.g. "Europe/London".
    Timezone(String),
    PrayerMethod(u8),
    PrayerSchool(u8),
    PrayerEnding(i64),
    /// Back to the server defaults. Prayer reminders stay on or off.
    Reset,
}

/// Why a change wasn't made. Nothing is saved when this comes back.
#[derive(Debug, Clone, PartialEq)]
pub enum Refused {
    UnknownTown { city: String, country: String },
    UnknownTimezone(String),
    UnknownMethod(u8),
    UnknownSchool(u8),
    EndingOutOfRange(i64),
}

/// What a successful change did, for the caller's reply.
#[derive(Debug, Clone, PartialEq)]
pub enum Saved {
    Done,
    /// The town as the geocoder spells it, and the timezone that came
    /// with it.
    Town {
        name: String,
        timezone: Tz,
    },
}

/// A change that passed its checks, ready to save. The town has been
/// looked up already, so saving it needs no network.
#[derive(Debug, Clone, PartialEq)]
pub enum Checked {
    Town(Town),
    Timezone(Tz),
    PrayerMethod(u8),
    PrayerSchool(u8),
    PrayerEnding(i64),
    Reset,
}

/// Checks `change`: `Ok` with what to save, or `Err` with why it isn't
/// allowed. Nothing is saved here, and there's no database at all.
///
/// A new town is looked up with `geo`, over the network. That's why this
/// is a step of its own: callers run it *before* opening their
/// transaction, so a slow geocoder never holds the database's write lock
/// (see `routes::webhook`). Any failed lookup, even the geocoder being
/// down, counts as "couldn't find it".
pub async fn check(geo: &Geo, change: &Change) -> Result<Checked, Refused> {
    match change {
        Change::City { city, country } => match geo.find_town(city, country).await {
            Ok(town) => Ok(Checked::Town(town)),
            Err(_) => Err(Refused::UnknownTown {
                city: city.clone(),
                country: country.clone(),
            }),
        },
        Change::Timezone(name) => check_timezone(name).map(Checked::Timezone),
        Change::PrayerMethod(method) => {
            check_method(*method).map(|()| Checked::PrayerMethod(*method))
        }
        Change::PrayerSchool(school) => {
            check_school(*school).map(|()| Checked::PrayerSchool(*school))
        }
        Change::PrayerEnding(minutes) => {
            check_ending(*minutes).map(|()| Checked::PrayerEnding(*minutes))
        }
        Change::Reset => Ok(Checked::Reset),
    }
}

/// Saves a checked change for `chat_id`. Only the database: safe inside a
/// transaction.
///
/// `current` is the chat's settings now: a town whose timezone the
/// geocoder doesn't know keeps the current timezone.
pub async fn save(
    conn: &mut SqliteConnection,
    chat_id: i64,
    current: &Settings,
    checked: Checked,
) -> Result<Saved, AppError> {
    match checked {
        Checked::Town(town) => {
            let timezone = town.timezone.unwrap_or(current.timezone);
            stored::set_location(
                conn,
                chat_id,
                &town.name,
                &town.country,
                town.coordinates,
                timezone.name(),
            )
            .await?;
            return Ok(Saved::Town {
                name: town.name,
                timezone,
            });
        }
        Checked::Timezone(timezone) => {
            stored::set_timezone(conn, chat_id, timezone.name()).await?;
        }
        Checked::PrayerMethod(method) => {
            stored::set_prayer_method(conn, chat_id, method).await?;
        }
        Checked::PrayerSchool(school) => {
            stored::set_prayer_school(conn, chat_id, school).await?;
        }
        Checked::PrayerEnding(minutes) => {
            stored::set_prayer_ending_minutes(conn, chat_id, minutes).await?;
        }
        Checked::Reset => stored::reset(conn, chat_id).await?,
    }
    Ok(Saved::Done)
}

// The checks on their own: pure, so they're simple to test.

/// A known IANA timezone name, as a `Tz`.
pub fn check_timezone(name: &str) -> Result<Tz, Refused> {
    name.parse()
        .map_err(|_| Refused::UnknownTimezone(name.to_string()))
}

/// A method AlAdhan knows (there's no method 6, for example).
pub fn check_method(method: u8) -> Result<(), Refused> {
    match prayer::method_name(method) {
        Some(_) => Ok(()),
        None => Err(Refused::UnknownMethod(method)),
    }
}

/// Shafi'i (0) or Hanafi (1).
pub fn check_school(school: u8) -> Result<(), Refused> {
    if school == SHAFII || school == HANAFI {
        Ok(())
    } else {
        Err(Refused::UnknownSchool(school))
    }
}

/// Between 1 and 120 minutes.
pub fn check_ending(minutes: i64) -> Result<(), Refused> {
    if ENDING_MINUTES.contains(&minutes) {
        Ok(())
    } else {
        Err(Refused::EndingOutOfRange(minutes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::repo::chats;
    use crate::settings::tests::defaults;
    use crate::testkit::fake_geocoder;

    /// `check`, then `save` if it passed: what each caller does, with its
    /// transaction in between. The outer `Result` is "something broke"
    /// (the database), the inner one "allowed or not".
    async fn apply(
        conn: &mut SqliteConnection,
        geo: &Geo,
        chat_id: i64,
        current: &Settings,
        change: &Change,
    ) -> Result<Result<Saved, Refused>, AppError> {
        match check(geo, change).await {
            Ok(checked) => save(conn, chat_id, current, checked).await.map(Ok),
            Err(refused) => Ok(Err(refused)),
        }
    }

    #[test]
    fn checks() {
        assert_eq!(check_timezone("Asia/Karachi"), Ok(chrono_tz::Asia::Karachi));
        assert_eq!(
            check_timezone("Mars/Olympus"),
            Err(Refused::UnknownTimezone("Mars/Olympus".to_string()))
        );
        assert_eq!(check_method(3), Ok(()));
        assert_eq!(check_method(6), Err(Refused::UnknownMethod(6)));
        assert_eq!(check_school(HANAFI), Ok(()));
        assert_eq!(check_school(2), Err(Refused::UnknownSchool(2)));
        assert_eq!(check_ending(1), Ok(()));
        assert_eq!(check_ending(120), Ok(()));
        assert_eq!(check_ending(0), Err(Refused::EndingOutOfRange(0)));
        assert_eq!(check_ending(121), Err(Refused::EndingOutOfRange(121)));
    }

    #[tokio::test]
    async fn changes_are_saved_and_refusals_save_nothing() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let chat = chats::find_or_create(
            &mut conn,
            "120363000000000001@g.us",
            None,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        let geo = Geo::with_geocoding_url(&fake_geocoder().await);
        let now = defaults();

        // Refused: nothing stored.
        for change in [
            Change::Timezone("Mars/Olympus".to_string()),
            Change::PrayerMethod(6),
            Change::PrayerSchool(7),
            Change::PrayerEnding(121),
            Change::City {
                city: "Nowhereville".to_string(),
                country: "GB".to_string(),
            },
        ] {
            let result = apply(&mut conn, &geo, chat, &now, &change).await.unwrap();
            assert!(result.is_err(), "{change:?} should be refused");
        }
        assert_eq!(
            stored::get(&mut conn, chat).await.unwrap(),
            stored::ChatSettings::default()
        );

        // Allowed: stored.
        let town = Change::City {
            city: "leicester".to_string(),
            country: "GB".to_string(),
        };
        assert_eq!(
            apply(&mut conn, &geo, chat, &now, &town).await.unwrap(),
            Ok(Saved::Town {
                name: "Leicester".to_string(),
                timezone: chrono_tz::Europe::London
            })
        );
        for change in [
            Change::Timezone("Asia/Karachi".to_string()),
            Change::PrayerMethod(2),
            Change::PrayerSchool(HANAFI),
            Change::PrayerEnding(45),
        ] {
            let result = apply(&mut conn, &geo, chat, &now, &change).await.unwrap();
            assert_eq!(result, Ok(Saved::Done), "{change:?}");
        }
        let own = stored::get(&mut conn, chat).await.unwrap().overrides;
        assert_eq!(own.city.as_deref(), Some("Leicester"));
        assert_eq!(own.country.as_deref(), Some("GB"));
        assert_eq!(own.timezone.as_deref(), Some("Asia/Karachi"));
        assert_eq!(own.prayer_method, Some(2));
        assert_eq!(own.prayer_school, Some(i64::from(HANAFI)));
        assert_eq!(own.prayer_ending_minutes, Some(45));

        apply(&mut conn, &geo, chat, &now, &Change::Reset)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored::get(&mut conn, chat).await.unwrap().overrides,
            crate::settings::Overrides::default()
        );
    }
}

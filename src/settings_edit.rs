//! Changing a chat's settings: one set of rules for `/settings` in WhatsApp
//! and for the admin dashboard, so the two can never disagree about what's
//! allowed.
//!
//! Each caller words its own replies; this module only checks and saves.

use chrono_tz::Tz;
use sqlx::SqliteConnection;

use crate::Apis;
use crate::error::AppError;
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

/// Checks `change` and, if it's allowed, saves it for `chat_id`.
///
/// The result is a `Result` inside a `Result`, and the two layers mean
/// different things: the outer `Err` is "something broke" (the database),
/// the inner `Err` is "the person asked for something that isn't allowed".
/// `?` deals with the first; the caller explains the second.
///
/// `current` is the chat's settings now: a town whose timezone the
/// geocoder doesn't know keeps the current timezone.
pub async fn apply(
    conn: &mut SqliteConnection,
    apis: &Apis,
    chat_id: i64,
    current: &Settings,
    change: &Change,
) -> Result<Result<Saved, Refused>, AppError> {
    match change {
        Change::City { city, country } => {
            let Ok(town) = apis.geo.find_town(city, country).await else {
                return Ok(Err(Refused::UnknownTown {
                    city: city.clone(),
                    country: country.clone(),
                }));
            };
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
            Ok(Ok(Saved::Town {
                name: town.name,
                timezone,
            }))
        }
        Change::Timezone(name) => match check_timezone(name) {
            Ok(timezone) => {
                stored::set_timezone(conn, chat_id, timezone.name()).await?;
                Ok(Ok(Saved::Done))
            }
            Err(refused) => Ok(Err(refused)),
        },
        Change::PrayerMethod(method) => match check_method(*method) {
            Ok(()) => {
                stored::set_prayer_method(conn, chat_id, *method).await?;
                Ok(Ok(Saved::Done))
            }
            Err(refused) => Ok(Err(refused)),
        },
        Change::PrayerSchool(school) => match check_school(*school) {
            Ok(()) => {
                stored::set_prayer_school(conn, chat_id, *school).await?;
                Ok(Ok(Saved::Done))
            }
            Err(refused) => Ok(Err(refused)),
        },
        Change::PrayerEnding(minutes) => match check_ending(*minutes) {
            Ok(()) => {
                stored::set_prayer_ending_minutes(conn, chat_id, *minutes).await?;
                Ok(Ok(Saved::Done))
            }
            Err(refused) => Ok(Err(refused)),
        },
        Change::Reset => {
            stored::reset(conn, chat_id).await?;
            Ok(Ok(Saved::Done))
        }
    }
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
pub(crate) mod tests {
    use axum::Json;
    use axum::extract::Query;
    use serde_json::{Value, json};

    use super::*;
    use crate::aladhan::PrayerTimes;
    use crate::db::test_pool;
    use crate::geo::Geo;
    use crate::repo::chats;
    use crate::settings::tests::defaults;

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

    /// A fake Open-Meteo geocoder that only knows Leicester. Returns its
    /// base URL.
    pub(crate) async fn fake_geocoder() -> String {
        async fn search(Query(q): Query<Vec<(String, String)>>) -> Json<Value> {
            let name = q.iter().find(|(k, _)| k == "name").map(|(_, v)| v.as_str());
            if name.is_some_and(|n| n.eq_ignore_ascii_case("leicester")) {
                Json(json!({ "results": [{
                    "name": "Leicester", "country_code": "gb",
                    "latitude": 52.6369, "longitude": -1.1398,
                    "timezone": "Europe/London"
                }] }))
            } else {
                Json(json!({}))
            }
        }
        let app = axum::Router::new().route("/search", axum::routing::get(search));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
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
        let mut apis = Apis::new(PrayerTimes::new(defaults()));
        apis.geo = Geo::with_geocoding_url(&fake_geocoder().await);
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
            let result = apply(&mut conn, &apis, chat, &now, &change).await.unwrap();
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
            apply(&mut conn, &apis, chat, &now, &town).await.unwrap(),
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
            let result = apply(&mut conn, &apis, chat, &now, &change).await.unwrap();
            assert_eq!(result, Ok(Saved::Done), "{change:?}");
        }
        let own = stored::get(&mut conn, chat).await.unwrap().overrides;
        assert_eq!(own.city.as_deref(), Some("Leicester"));
        assert_eq!(own.country.as_deref(), Some("GB"));
        assert_eq!(own.timezone.as_deref(), Some("Asia/Karachi"));
        assert_eq!(own.prayer_method, Some(2));
        assert_eq!(own.prayer_school, Some(i64::from(HANAFI)));
        assert_eq!(own.prayer_ending_minutes, Some(45));

        apply(&mut conn, &apis, chat, &now, &Change::Reset)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored::get(&mut conn, chat).await.unwrap().overrides,
            crate::settings::Overrides::default()
        );
    }
}

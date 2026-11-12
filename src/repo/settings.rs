//! Per-chat settings (`chat_settings`). A DM's row is that person's own
//! settings; a group's is shared. No row, or NULL in a column, means "use
//! the server default".

use sqlx::SqliteConnection;

use crate::geo::Coordinates;
use crate::settings::{self, Overrides, Settings};

/// A chat's stored settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatSettings {
    /// `/prayers on` in this chat.
    pub prayer_reminders: bool,
    /// What it changed from the defaults.
    pub overrides: Overrides,
}

/// One member's personal settings (from their DM with the bot), for
/// `/settings everyone`.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberSettings {
    pub user_id: i64,
    pub display_name: String,
    /// Phone-number ID, e.g. `447700900001@s.whatsapp.net`, for @mentions.
    pub wa_jid: String,
    /// `None` if they've never messaged the bot privately.
    pub settings: Option<ChatSettings>,
}

/// The chat's settings. A chat that never changed anything has no row,
/// which reads as "reminders off, all defaults".
pub async fn get(conn: &mut SqliteConnection, chat_id: i64) -> Result<ChatSettings, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT prayer_reminders AS "prayer_reminders: bool", timezone, city, country,
                  latitude, longitude, prayer_method, prayer_school, prayer_ending_minutes
           FROM chat_settings WHERE chat_id = ?"#,
        chat_id
    )
    .fetch_optional(conn)
    .await?;

    Ok(row.map_or(ChatSettings::default(), |r| ChatSettings {
        prayer_reminders: r.prayer_reminders,
        overrides: Overrides {
            timezone: r.timezone,
            city: r.city,
            country: r.country,
            latitude: r.latitude,
            longitude: r.longitude,
            prayer_method: r.prayer_method,
            prayer_school: r.prayer_school,
            prayer_ending_minutes: r.prayer_ending_minutes,
        },
    }))
}

/// Every member of a group with their personal settings, A to Z.
pub async fn members(
    conn: &mut SqliteConnection,
    chat_id: i64,
) -> Result<Vec<MemberSettings>, sqlx::Error> {
    // Two LEFT JOINs: a member's DM chat (if they have one), then that DM's
    // settings row (if any). `s.chat_id AS "has_row?"` tells the two apart:
    // NULL means no settings row at all.
    let rows = sqlx::query!(
        r#"SELECT u.id AS user_id, u.display_name, u.wa_jid,
                  s.chat_id AS "has_row?",
                  s.prayer_reminders AS "prayer_reminders?: bool",
                  s.timezone, s.city, s.country,
                  s.latitude, s.longitude, s.prayer_method, s.prayer_school, s.prayer_ending_minutes
           FROM chat_members m
           JOIN users u ON u.id = m.user_id
           LEFT JOIN chats dm ON dm.dm_user_id = u.id
           LEFT JOIN chat_settings s ON s.chat_id = dm.id
           WHERE m.chat_id = ?
           ORDER BY u.display_name"#,
        chat_id
    )
    .fetch_all(conn)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| MemberSettings {
            user_id: r.user_id,
            display_name: r.display_name,
            wa_jid: r.wa_jid,
            settings: r.has_row.map(|_| ChatSettings {
                prayer_reminders: r.prayer_reminders.unwrap_or(false),
                overrides: Overrides {
                    timezone: r.timezone,
                    city: r.city,
                    country: r.country,
                    latitude: r.latitude,
                    longitude: r.longitude,
                    prayer_method: r.prayer_method,
                    prayer_school: r.prayer_school,
                    prayer_ending_minutes: r.prayer_ending_minutes,
                },
            }),
        })
        .collect())
}

/// A person's own settings: the ones stored for their DM with the bot.
/// `None` if they've never messaged the bot privately (or never changed
/// anything there).
pub async fn personal(
    conn: &mut SqliteConnection,
    user_id: i64,
) -> Result<Option<ChatSettings>, sqlx::Error> {
    let dm = sqlx::query_scalar!("SELECT id FROM chats WHERE dm_user_id = ?", user_id)
        .fetch_optional(&mut *conn)
        .await?;
    match dm {
        Some(dm) => {
            let settings = get(conn, dm).await?;
            // A DM that never changed anything reads as the default, which
            // is the same as having no personal settings.
            Ok((settings != ChatSettings::default()).then_some(settings))
        }
        None => Ok(None),
    }
}

/// The settings that apply in chat `chat_id`, for `person` if there is one:
/// `settings::layered` with what's stored. `None` for things that belong to
/// the whole chat (a group's reminder or summary). In a DM, pass its owner
/// (or `None`: the result is the same).
pub async fn effective(
    conn: &mut SqliteConnection,
    defaults: &Settings,
    chat_id: i64,
    person: Option<i64>,
) -> Result<Settings, sqlx::Error> {
    let chat = get(&mut *conn, chat_id).await?;
    let own = match person {
        Some(user_id) => personal(conn, user_id).await?,
        None => None,
    };
    Ok(settings::layered(
        defaults,
        &chat.overrides,
        own.as_ref().map(|own| &own.overrides),
    ))
}

/// The one group this person is in that the bot is still in, for the
/// admin pages, which have a person but no chat. `None` if they're in no
/// such group, or in several (then no one group's settings are theirs).
pub async fn home_chat(
    conn: &mut SqliteConnection,
    user_id: i64,
) -> Result<Option<i64>, sqlx::Error> {
    // `LIMIT 2` is enough to tell "exactly one" from "several".
    let groups = sqlx::query_scalar!(
        "SELECT c.id FROM chat_members m
         JOIN chats c ON c.id = m.chat_id
         WHERE m.user_id = ? AND c.kind = 'group' AND c.left_at IS NULL
         ORDER BY c.id LIMIT 2",
        user_id
    )
    .fetch_all(conn)
    .await?;
    Ok(match groups.as_slice() {
        [one] => Some(*one),
        _ => None,
    })
}

// The setters below all "upsert": `INSERT ... ON CONFLICT (chat_id) DO
// UPDATE` creates the chat's row the first time and changes just the one
// setting after that. `excluded.x` is the value the INSERT tried to write.

/// Turns prayer reminders on or off (`/prayers on` / `/prayers off`).
pub async fn set_prayer_reminders(
    conn: &mut SqliteConnection,
    chat_id: i64,
    on: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO chat_settings (chat_id, prayer_reminders) VALUES (?, ?)
         ON CONFLICT (chat_id) DO UPDATE SET prayer_reminders = excluded.prayer_reminders",
        chat_id,
        on
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// City, country, where it is, and its timezone, all together.
pub async fn set_location(
    conn: &mut SqliteConnection,
    chat_id: i64,
    city: &str,
    country: &str,
    at: Coordinates,
    timezone: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO chat_settings (chat_id, city, country, latitude, longitude, timezone)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (chat_id) DO UPDATE
         SET city = excluded.city, country = excluded.country,
             latitude = excluded.latitude, longitude = excluded.longitude,
             timezone = excluded.timezone",
        chat_id,
        city,
        country,
        at.latitude,
        at.longitude,
        timezone
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn set_timezone(
    conn: &mut SqliteConnection,
    chat_id: i64,
    timezone: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO chat_settings (chat_id, timezone) VALUES (?, ?)
         ON CONFLICT (chat_id) DO UPDATE SET timezone = excluded.timezone",
        chat_id,
        timezone
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn set_prayer_method(
    conn: &mut SqliteConnection,
    chat_id: i64,
    method: u8,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO chat_settings (chat_id, prayer_method) VALUES (?, ?)
         ON CONFLICT (chat_id) DO UPDATE SET prayer_method = excluded.prayer_method",
        chat_id,
        method
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn set_prayer_school(
    conn: &mut SqliteConnection,
    chat_id: i64,
    school: u8,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO chat_settings (chat_id, prayer_school) VALUES (?, ?)
         ON CONFLICT (chat_id) DO UPDATE SET prayer_school = excluded.prayer_school",
        chat_id,
        school
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn set_prayer_ending_minutes(
    conn: &mut SqliteConnection,
    chat_id: i64,
    minutes: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO chat_settings (chat_id, prayer_ending_minutes) VALUES (?, ?)
         ON CONFLICT (chat_id) DO UPDATE SET prayer_ending_minutes = excluded.prayer_ending_minutes",
        chat_id,
        minutes
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Back to the defaults (`/settings reset`). Leaves reminders on or off.
pub async fn reset(conn: &mut SqliteConnection, chat_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE chat_settings
         SET timezone = NULL, city = NULL, country = NULL, latitude = NULL,
             longitude = NULL, prayer_method = NULL,
             prayer_school = NULL, prayer_ending_minutes = NULL
         WHERE chat_id = ?",
        chat_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::repo::chats;
    use crate::testkit::register;
    use chrono::Utc;

    fn paris() -> Coordinates {
        Coordinates {
            latitude: 48.8566,
            longitude: 2.3522,
        }
    }

    async fn group(conn: &mut SqliteConnection) -> i64 {
        chats::find_or_create(conn, "120363000000000001@g.us", None, Utc::now())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn settings_start_empty_and_change_one_at_a_time() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let chat = group(&mut conn).await;

        assert_eq!(get(&mut conn, chat).await.unwrap(), ChatSettings::default());

        set_location(&mut conn, chat, "Paris", "FR", paris(), "Europe/Paris")
            .await
            .unwrap();
        set_prayer_school(&mut conn, chat, 1).await.unwrap();
        set_prayer_reminders(&mut conn, chat, true).await.unwrap();
        let s = get(&mut conn, chat).await.unwrap();
        assert!(s.prayer_reminders);
        assert_eq!(s.overrides.city.as_deref(), Some("Paris"));
        assert_eq!(s.overrides.timezone.as_deref(), Some("Europe/Paris"));
        assert_eq!(
            (s.overrides.latitude, s.overrides.longitude),
            (Some(48.8566), Some(2.3522)),
            "coordinates are stored with the city"
        );
        assert_eq!(s.overrides.prayer_school, Some(1));
        assert_eq!(
            s.overrides.prayer_method, None,
            "untouched setting stays default"
        );

        reset(&mut conn, chat).await.unwrap();
        let s = get(&mut conn, chat).await.unwrap();
        assert!(s.prayer_reminders, "reset keeps reminders on");
        assert_eq!(s.overrides, Overrides::default());
    }

    #[tokio::test]
    async fn settings_outside_the_allowed_range_are_rejected() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let chat = group(&mut conn).await;
        assert!(set_prayer_method(&mut conn, chat, 99).await.is_err());
        assert!(set_prayer_school(&mut conn, chat, 2).await.is_err());
        assert!(set_prayer_ending_minutes(&mut conn, chat, 0).await.is_err());
        assert!(
            set_prayer_ending_minutes(&mut conn, chat, 500)
                .await
                .is_err()
        );
        assert!(
            set_location(&mut conn, chat, "  ", "GB", paris(), "Europe/London")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn members_show_their_personal_dm_settings() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let chat = group(&mut conn).await;

        let mut people = Vec::new();
        for (name, phone) in [("Bilal", "447700900001"), ("Khadija", "447700900002")] {
            let user = register(&mut conn, name, phone, now).await;
            chats::add_member(&mut conn, chat, user.id, now)
                .await
                .unwrap();
            people.push(user);
        }
        // Bilal has a DM with his own city; Khadija has never DM'd the bot.
        let bilal = &people[0];
        let dm = chats::find_or_create(&mut conn, &bilal.wa_jid, Some(bilal.id), now)
            .await
            .unwrap();
        set_location(&mut conn, dm, "Leicester", "GB", paris(), "Europe/London")
            .await
            .unwrap();

        let members = members(&mut conn, chat).await.unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].display_name, "Bilal");
        let own = members[0].settings.as_ref().expect("Bilal has settings");
        assert_eq!(own.overrides.city.as_deref(), Some("Leicester"));
        assert_eq!(members[1].display_name, "Khadija");
        assert_eq!(members[1].settings, None);
    }

    #[tokio::test]
    async fn effective_puts_the_persons_own_over_the_chats() {
        use crate::settings::tests::defaults;
        use crate::testkit::{seed_family, utc};

        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T12:00:00Z")).await;
        set_location(
            &mut conn,
            family.group,
            "Paris",
            "FR",
            paris(),
            "Europe/Paris",
        )
        .await
        .unwrap();
        set_timezone(&mut conn, family.dm, "America/New_York")
            .await
            .unwrap();
        let (group, dm) = (family.group, family.dm);
        let bilal = Some(family.bilal.id);
        let tz = |s: Settings| s.timezone.name().to_string();

        let whole_group = effective(&mut conn, &defaults(), group, None)
            .await
            .unwrap();
        assert_eq!(tz(whole_group), "Europe/Paris");
        let his = effective(&mut conn, &defaults(), group, bilal)
            .await
            .unwrap();
        assert_eq!(tz(his.clone()), "America/New_York");
        assert_eq!(
            his.city, "Paris",
            "the group's town: he only set a timezone"
        );
        let hers = effective(&mut conn, &defaults(), group, Some(family.khadija.id))
            .await
            .unwrap();
        assert_eq!(tz(hers), "Europe/Paris", "no DM: the group's");

        // In a DM, with or without its owner, the DM's settings.
        let in_dm = effective(&mut conn, &defaults(), dm, bilal).await.unwrap();
        assert_eq!(
            in_dm,
            effective(&mut conn, &defaults(), dm, None).await.unwrap()
        );
        assert_eq!(
            (tz(in_dm.clone()), in_dm.city.as_str()),
            ("America/New_York".to_string(), "Milton Keynes")
        );
    }

    #[tokio::test]
    async fn home_chat_is_the_one_group_the_bot_is_still_in() {
        use crate::testkit::{seed_family, utc};

        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = utc("2026-11-01T12:00:00Z");
        let family = seed_family(&mut conn, now).await;
        let bilal = family.bilal.id;
        // His DM doesn't count: it isn't a group.
        assert_eq!(
            home_chat(&mut conn, bilal).await.unwrap(),
            Some(family.group)
        );

        // A second group: no single one.
        let other = chats::find_or_create(&mut conn, "120363000000000002@g.us", None, now)
            .await
            .unwrap();
        chats::add_member(&mut conn, other, bilal, now)
            .await
            .unwrap();
        assert_eq!(home_chat(&mut conn, bilal).await.unwrap(), None);

        // The bot has left the second one: back to the first.
        crate::repo::cleanup::set_left(&mut conn, other, Some(now))
            .await
            .unwrap();
        assert_eq!(
            home_chat(&mut conn, bilal).await.unwrap(),
            Some(family.group)
        );

        // Someone in no group at all.
        let alone = register(&mut conn, "Yusuf", "447700900003", now).await;
        assert_eq!(home_chat(&mut conn, alone.id).await.unwrap(), None);
    }
}

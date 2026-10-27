//! Per-chat settings (`chat_settings`). A DM's row is that person's own
//! settings; a group's is shared. No row, or NULL in a column, means "use
//! the server default".

use sqlx::SqliteConnection;

use crate::settings::Overrides;

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
    // `as "prayer_reminders: bool"`: read the 0/1 column as a Rust bool.
    let row = sqlx::query!(
        r#"SELECT prayer_reminders AS "prayer_reminders: bool", timezone, city, country,
                  prayer_method, prayer_school, prayer_ending_minutes
           FROM chat_settings WHERE chat_id = ?"#,
        chat_id
    )
    .fetch_optional(conn)
    .await?;

    // `map_or(default, f)`: `default` if there's no row, else `f(row)`.
    Ok(row.map_or(ChatSettings::default(), |r| ChatSettings {
        prayer_reminders: r.prayer_reminders,
        overrides: Overrides {
            timezone: r.timezone,
            city: r.city,
            country: r.country,
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
                  s.prayer_method, s.prayer_school, s.prayer_ending_minutes
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

/// City, country and the timezone that goes with them, together.
pub async fn set_location(
    conn: &mut SqliteConnection,
    chat_id: i64,
    city: &str,
    country: &str,
    timezone: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO chat_settings (chat_id, city, country, timezone) VALUES (?, ?, ?, ?)
         ON CONFLICT (chat_id) DO UPDATE
         SET city = excluded.city, country = excluded.country, timezone = excluded.timezone",
        chat_id,
        city,
        country,
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
         SET timezone = NULL, city = NULL, country = NULL, prayer_method = NULL,
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
    use crate::person::Person;
    use crate::repo::{chats, users};
    use chrono::Utc;

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

        set_location(&mut conn, chat, "Paris", "FR", "Europe/Paris")
            .await
            .unwrap();
        set_prayer_school(&mut conn, chat, 1).await.unwrap();
        set_prayer_reminders(&mut conn, chat, true).await.unwrap();
        let s = get(&mut conn, chat).await.unwrap();
        assert!(s.prayer_reminders);
        assert_eq!(s.overrides.city.as_deref(), Some("Paris"));
        assert_eq!(s.overrides.timezone.as_deref(), Some("Europe/Paris"));
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
            set_location(&mut conn, chat, "  ", "GB", "Europe/London")
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

        let mut ids = Vec::new();
        for (name, jid) in [
            ("Bilal", "447700900001@s.whatsapp.net"),
            ("Khadija", "447700900002@s.whatsapp.net"),
        ] {
            let person = Person {
                name: name.to_string(),
                jid: jid.to_string(),
                lid: None,
            };
            let user = users::find_or_register(&mut conn, &person, now)
                .await
                .unwrap()
                .unwrap();
            chats::add_member(&mut conn, chat, user.id, now)
                .await
                .unwrap();
            ids.push((user.id, jid));
        }
        // Bilal has a DM with his own city; Khadija has never DM'd the bot.
        let (bilal, bilal_jid) = ids[0];
        let dm = chats::find_or_create(&mut conn, bilal_jid, Some(bilal), now)
            .await
            .unwrap();
        set_location(&mut conn, dm, "Leicester", "GB", "Europe/London")
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
}

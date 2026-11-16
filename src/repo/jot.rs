//! Jot: each person's settings, the prompts sent and what they jotted down.

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Utc};
use sqlx::SqliteConnection;

/// The text form of a clock time in the database ("09:00"): zero-padded, so
/// the schema can compare two of them as text.
const CLOCK: &str = "%H:%M";

/// Someone's Jot settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JotSettings {
    pub enabled: bool,
    pub interval_minutes: u32,
    /// Local clock times, in the person's own timezone.
    pub start: NaiveTime,
    pub end: NaiveTime,
    /// When it was last turned on or changed.
    pub updated_at: DateTime<Utc>,
}

fn decode_time(text: &str) -> Result<NaiveTime, sqlx::Error> {
    NaiveTime::parse_from_str(text, CLOCK).map_err(|err| sqlx::Error::Decode(Box::new(err)))
}

fn decode_interval(minutes: i64) -> Result<u32, sqlx::Error> {
    u32::try_from(minutes).map_err(|err| sqlx::Error::Decode(Box::new(err)))
}

/// This person's settings, if they've ever turned Jot on.
pub async fn get(
    conn: &mut SqliteConnection,
    user_id: i64,
) -> Result<Option<JotSettings>, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT enabled AS "enabled: bool", interval_minutes, start_local, end_local,
                  updated_at AS "updated_at: DateTime<Utc>"
           FROM jot_settings WHERE user_id = ?"#,
        user_id
    )
    .fetch_optional(conn)
    .await?;
    row.map(|r| {
        Ok(JotSettings {
            enabled: r.enabled,
            interval_minutes: decode_interval(r.interval_minutes)?,
            start: decode_time(&r.start_local)?,
            end: decode_time(&r.end_local)?,
            updated_at: r.updated_at,
        })
    })
    .transpose()
}

/// Turns Jot on with these settings (or changes them). `now` is kept, so no
/// prompt from before it is sent.
pub async fn turn_on(
    conn: &mut SqliteConnection,
    user_id: i64,
    interval_minutes: u32,
    start: NaiveTime,
    end: NaiveTime,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let (start, end) = (
        start.format(CLOCK).to_string(),
        end.format(CLOCK).to_string(),
    );
    sqlx::query!(
        "INSERT INTO jot_settings (user_id, enabled, interval_minutes, start_local, end_local, updated_at)
         VALUES (?, 1, ?, ?, ?, ?)
         ON CONFLICT (user_id) DO UPDATE SET
             enabled = 1,
             interval_minutes = excluded.interval_minutes,
             start_local = excluded.start_local,
             end_local = excluded.end_local,
             updated_at = excluded.updated_at",
        user_id,
        interval_minutes,
        start,
        end,
        now
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Turns Jot off, keeping the interval and hours for next time.
pub async fn turn_off(conn: &mut SqliteConnection, user_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE jot_settings SET enabled = 0 WHERE user_id = ?",
        user_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Someone with Jot on, and the DM their prompts go to.
#[derive(Debug, Clone, PartialEq)]
pub struct Subscriber {
    pub user_id: i64,
    pub chat_id: i64,
    pub wa_chat_id: String,
    pub display_name: String,
    pub settings: JotSettings,
}

/// Everyone with Jot on whose DM the bot is still in.
pub async fn subscribers(conn: &mut SqliteConnection) -> Result<Vec<Subscriber>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT u.id AS user_id, u.display_name, c.id AS chat_id, c.wa_chat_id,
                  s.interval_minutes, s.start_local, s.end_local,
                  s.updated_at AS "updated_at: DateTime<Utc>"
           FROM jot_settings s
           JOIN users u ON u.id = s.user_id
           JOIN chats c ON c.dm_user_id = u.id AND c.left_at IS NULL
           WHERE s.enabled = 1"#
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|r| {
            Ok(Subscriber {
                user_id: r.user_id,
                chat_id: r.chat_id,
                wa_chat_id: r.wa_chat_id,
                display_name: r.display_name,
                settings: JotSettings {
                    enabled: true,
                    interval_minutes: decode_interval(r.interval_minutes)?,
                    start: decode_time(&r.start_local)?,
                    end: decode_time(&r.end_local)?,
                    updated_at: r.updated_at,
                },
            })
        })
        .collect()
}

/// Claims the prompt for `slot_at`. `true` the first time only, so it's sent
/// once even if the loop runs again or restarts.
pub async fn claim_prompt(
    conn: &mut SqliteConnection,
    user_id: i64,
    chat_id: i64,
    slot_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "INSERT INTO jot_prompts (user_id, chat_id, slot_at, claimed_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT (user_id, slot_at) DO NOTHING",
        user_id,
        chat_id,
        slot_at,
        now
    )
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Remembers the WhatsApp ID of a sent prompt, so a reply quoting it can be
/// matched to its slot.
pub async fn set_prompt_message(
    conn: &mut SqliteConnection,
    user_id: i64,
    slot_at: DateTime<Utc>,
    wa_message_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE jot_prompts SET wa_message_id = ? WHERE user_id = ? AND slot_at = ?",
        wa_message_id,
        user_id,
        slot_at
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The slot of the prompt a message quoted, if it is one of the prompts
/// sent in this chat.
pub async fn slot_of_message(
    conn: &mut SqliteConnection,
    wa_message_id: &str,
    chat_id: i64,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT slot_at AS "slot_at: DateTime<Utc>"
           FROM jot_prompts WHERE wa_message_id = ? AND chat_id = ?"#,
        wa_message_id,
        chat_id
    )
    .fetch_optional(conn)
    .await
}

/// How long after a prompt a plain message still answers it.
pub const OPEN_FOR: Duration = Duration::minutes(30);

/// The newest prompt in this chat that was sent within `OPEN_FOR` of `now`
/// and has no answer yet: what a plain message is taken to be a reply to.
pub async fn open_prompt(
    conn: &mut SqliteConnection,
    chat_id: i64,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    let since = now - OPEN_FOR;
    sqlx::query_scalar!(
        r#"SELECT p.slot_at AS "slot_at: DateTime<Utc>"
           FROM jot_prompts p
           WHERE p.chat_id = ?
             AND p.wa_message_id IS NOT NULL
             AND datetime(p.claimed_at) > datetime(?)
             AND datetime(p.claimed_at) <= datetime(?)
             AND NOT EXISTS (SELECT 1 FROM jot_entries e
                             WHERE e.user_id = p.user_id AND e.slot_at = p.slot_at)
           ORDER BY datetime(p.claimed_at) DESC
           LIMIT 1"#,
        chat_id,
        since,
        now
    )
    .fetch_optional(conn)
    .await
}

/// Saves what someone jotted down. `slot_at` is the prompt it answers, or
/// `None` for a `+jot`.
pub async fn add_entry(
    conn: &mut SqliteConnection,
    user_id: i64,
    logged_at: DateTime<Utc>,
    day: NaiveDate,
    slot_at: Option<DateTime<Utc>>,
    text: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO jot_entries (user_id, logged_at, day, slot_at, text)
         VALUES (?, ?, ?, ?, ?)",
        user_id,
        logged_at,
        day,
        slot_at,
        text
    )
    .execute(conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::testkit::{date, seed_family, utc};

    fn at(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[tokio::test]
    async fn off_until_turned_on_and_remembers_its_settings_after_off() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = utc("2026-11-16T10:00:00Z");
        let family = seed_family(&mut conn, now).await;
        let user = family.bilal.id;

        assert_eq!(get(&mut conn, user).await.unwrap(), None);
        assert!(subscribers(&mut conn).await.unwrap().is_empty());

        turn_on(&mut conn, user, 120, at(9, 0), at(18, 0), now)
            .await
            .unwrap();
        let found = subscribers(&mut conn).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].chat_id, family.dm);
        assert_eq!(found[0].settings.interval_minutes, 120);

        turn_off(&mut conn, user).await.unwrap();
        assert!(subscribers(&mut conn).await.unwrap().is_empty());
        let kept = get(&mut conn, user).await.unwrap().unwrap();
        assert!(!kept.enabled);
        assert_eq!((kept.start, kept.end), (at(9, 0), at(18, 0)));
    }

    #[tokio::test]
    async fn the_schema_refuses_a_bad_interval_and_an_end_before_the_start() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = utc("2026-11-16T10:00:00Z");
        let family = seed_family(&mut conn, now).await;
        let user = family.bilal.id;

        assert!(
            turn_on(&mut conn, user, 15, at(9, 0), at(18, 0), now)
                .await
                .is_err()
        );
        assert!(
            turn_on(&mut conn, user, 300, at(9, 0), at(18, 0), now)
                .await
                .is_err()
        );
        assert!(
            turn_on(&mut conn, user, 120, at(18, 0), at(9, 0), now)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_prompt_is_claimed_once_and_stays_open_for_half_an_hour_until_answered() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = utc("2026-11-16T11:00:00Z");
        let family = seed_family(&mut conn, now).await;
        let (user, dm) = (family.bilal.id, family.dm);
        let slot = utc("2026-11-16T11:00:00Z");

        assert!(claim_prompt(&mut conn, user, dm, slot, now).await.unwrap());
        assert!(!claim_prompt(&mut conn, user, dm, slot, now).await.unwrap());
        // Not open until WhatsApp has accepted it.
        assert_eq!(open_prompt(&mut conn, dm, now).await.unwrap(), None);
        set_prompt_message(&mut conn, user, slot, "PROMPT1")
            .await
            .unwrap();

        let soon = utc("2026-11-16T11:29:00Z");
        assert_eq!(open_prompt(&mut conn, dm, soon).await.unwrap(), Some(slot));
        let late = utc("2026-11-16T11:31:00Z");
        assert_eq!(open_prompt(&mut conn, dm, late).await.unwrap(), None);
        assert_eq!(
            slot_of_message(&mut conn, "PROMPT1", dm).await.unwrap(),
            Some(slot)
        );
        assert_eq!(
            slot_of_message(&mut conn, "PROMPT1", family.group)
                .await
                .unwrap(),
            None
        );

        add_entry(
            &mut conn,
            user,
            soon,
            date("2026-11-16"),
            Some(slot),
            "emails",
        )
        .await
        .unwrap();
        assert_eq!(
            open_prompt(&mut conn, dm, soon).await.unwrap(),
            None,
            "answered"
        );
    }
}

//! The shared `reminders` table: every reminder message the bot sends, with
//! the feature that owns it, so the one sending loop (`reminders::engine`)
//! can send the ones written ahead of time, and a reaction can be routed
//! back to its feature.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;
use tracing::warn;

/// Which feature a reminder belongs to, and so where a reaction to it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Prayer,
    Todo,
    /// Written by someone (`+remind`, Stage 2.2).
    Custom,
}

impl Source {
    /// The exact text stored in `reminders.source`.
    pub fn as_db_str(self) -> &'static str {
        match self {
            Source::Prayer => "prayer",
            Source::Todo => "todo",
            Source::Custom => "custom",
        }
    }
}

impl FromStr for Source {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "prayer" => Ok(Source::Prayer),
            "todo" => Ok(Source::Todo),
            "custom" => Ok(Source::Custom),
            other => Err(format!("unknown reminder source {other:?}")),
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_db_str())
    }
}

/// A reminder to add.
#[derive(Debug, Clone, PartialEq)]
pub struct NewReminder<'a> {
    pub chat_id: i64,
    pub due_at: DateTime<Utc>,
    pub source: Source,
    /// The owning feature's row (see the migration), if it has one.
    pub source_id: Option<i64>,
    pub text: &'a str,
    /// Phone numbers to @mention.
    pub mentions: &'a [String],
}

/// Writes a reminder to be sent at `due_at` by the sending loop, which
/// claims it then. Returns its id.
///
/// Only the tests call it so far: `+remind` (Stage 2.2) and habit
/// reminders (H3) are what will write reminders ahead of time, and the
/// sending side is built first so they don't each bring their own loop.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn schedule(
    conn: &mut SqliteConnection,
    reminder: &NewReminder<'_>,
) -> Result<i64, sqlx::Error> {
    insert(conn, reminder, None).await
}

/// Records a reminder that's being sent right now, already claimed, so the
/// sending loop never sends it again. Its text isn't kept (see the
/// migration). Returns its id, for `set_message`.
pub async fn record_sending(
    conn: &mut SqliteConnection,
    reminder: &NewReminder<'_>,
    now: DateTime<Utc>,
) -> Result<i64, sqlx::Error> {
    let unkept = NewReminder {
        text: "",
        mentions: &[],
        ..reminder.clone()
    };
    insert(conn, &unkept, Some(now)).await
}

async fn insert(
    conn: &mut SqliteConnection,
    reminder: &NewReminder<'_>,
    claimed_at: Option<DateTime<Utc>>,
) -> Result<i64, sqlx::Error> {
    let source = reminder.source.as_db_str();
    let mentions = reminder.mentions.join(" ");
    sqlx::query_scalar!(
        r#"INSERT INTO reminders (chat_id, due_at, source, source_id, text, mentions, claimed_at)
           VALUES (?, ?, ?, ?, ?, ?, ?)
           RETURNING id AS "id!""#,
        reminder.chat_id,
        reminder.due_at,
        source,
        reminder.source_id,
        reminder.text,
        mentions,
        claimed_at
    )
    .fetch_one(conn)
    .await
}

/// A written-ahead reminder whose time has come, as the loop sends it.
#[derive(Debug, Clone, PartialEq)]
pub struct Due {
    pub id: i64,
    /// The WhatsApp chat to send it to.
    pub wa_chat_id: String,
    pub source: Source,
    pub text: String,
    pub mentions: Vec<String>,
}

/// Every reminder due by `now` that hasn't been claimed, oldest first.
/// Ones due while the bot was down are in here too: they're caught up on
/// the first tick after start-up. Chats the bot has left are skipped (it
/// can't post there).
///
/// `datetime(...)` compares the moments, not the text of the timestamps.
pub async fn due(conn: &mut SqliteConnection, now: DateTime<Utc>) -> Result<Vec<Due>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT r.id, c.wa_chat_id, r.source, r.text, r.mentions
           FROM reminders r
           JOIN chats c ON c.id = r.chat_id
           WHERE r.claimed_at IS NULL AND c.left_at IS NULL
             AND datetime(r.due_at) <= datetime(?)
           ORDER BY datetime(r.due_at), r.id"#,
        now
    )
    .fetch_all(conn)
    .await?;
    // The CHECK constraint only allows the three sources, so this parse can
    // only fail if the code and the schema disagree; such a row is dropped
    // with a warning rather than stopping every other reminder.
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let source = r
                .source
                .parse()
                .inspect_err(|err| warn!("reminder {} dropped: {err}", r.id))
                .ok()?;
            Some(Due {
                id: r.id,
                wa_chat_id: r.wa_chat_id,
                source,
                text: r.text,
                mentions: r.mentions.split_whitespace().map(str::to_string).collect(),
            })
        })
        .collect())
}

/// Claims a written-ahead reminder for sending. `true` the first time only,
/// so two ticks (or a tick racing a restart) can never both send it.
pub async fn claim(
    conn: &mut SqliteConnection,
    id: i64,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "UPDATE reminders SET claimed_at = ? WHERE id = ? AND claimed_at IS NULL",
        now,
        id
    )
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Records the WhatsApp ID of a sent reminder, so reactions can find it.
pub async fn set_message(
    conn: &mut SqliteConnection,
    id: i64,
    wa_message_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE reminders SET wa_message_id = ? WHERE id = ?",
        wa_message_id,
        id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The reminder a message was, as a reaction needs it: whose it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub source: Source,
    pub source_id: Option<i64>,
}

/// Which reminder `wa_message_id` was, if it's one of ours in this chat.
pub async fn find(
    conn: &mut SqliteConnection,
    wa_message_id: &str,
    wa_chat_id: &str,
) -> Result<Option<Found>, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT r.source, r.source_id
           FROM reminders r
           JOIN chats c ON c.id = r.chat_id
           WHERE r.wa_message_id = ? AND c.wa_chat_id = ?"#,
        wa_message_id,
        wa_chat_id
    )
    .fetch_optional(conn)
    .await?;
    // As in `due`: a row the code can't read is dropped, with a warning.
    Ok(row.and_then(|r| {
        let source = r
            .source
            .parse()
            .inspect_err(|err| warn!("reminder {wa_message_id} dropped: {err}"))
            .ok()?;
        Some(Found {
            source,
            source_id: r.source_id,
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::repo::chats;
    use crate::testkit::{GROUP, utc};

    async fn group(conn: &mut SqliteConnection) -> i64 {
        chats::find_or_create(conn, GROUP, None, utc("2026-11-01T12:00:00Z"))
            .await
            .unwrap()
    }

    fn dentist(chat_id: i64, mentions: &[String]) -> NewReminder<'_> {
        NewReminder {
            chat_id,
            due_at: utc("2026-11-13T10:00:00Z"),
            source: Source::Custom,
            source_id: None,
            text: "dentist",
            mentions,
        }
    }

    #[test]
    fn sources_round_trip_through_their_text() {
        for source in [Source::Prayer, Source::Todo, Source::Custom] {
            assert_eq!(source.as_db_str().parse(), Ok(source));
        }
        assert!("habit".parse::<Source>().is_err());
    }

    #[tokio::test]
    async fn a_scheduled_reminder_is_due_from_its_time_and_claimed_once() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let chat = group(&mut conn).await;
        let mentions = ["447700900002".to_string()];
        let id = schedule(&mut conn, &dentist(chat, &mentions))
            .await
            .unwrap();

        assert_eq!(
            due(&mut conn, utc("2026-11-13T09:59:00Z")).await.unwrap(),
            [],
            "not yet"
        );
        // Long after: still due, so a reminder missed while down catches up.
        let later = utc("2026-11-14T08:00:00Z");
        assert_eq!(
            due(&mut conn, later).await.unwrap(),
            [Due {
                id,
                wa_chat_id: GROUP.to_string(),
                source: Source::Custom,
                text: "dentist".to_string(),
                mentions: mentions.to_vec(),
            }]
        );

        assert!(claim(&mut conn, id, later).await.unwrap());
        assert!(!claim(&mut conn, id, later).await.unwrap(), "once");
        assert_eq!(due(&mut conn, later).await.unwrap(), [], "claimed");
    }

    #[tokio::test]
    async fn one_recorded_while_sending_is_never_due_and_keeps_no_text() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let chat = group(&mut conn).await;
        let mentions = ["447700900002".to_string()];
        let now = utc("2026-11-13T10:00:00Z");
        let id = record_sending(&mut conn, &dentist(chat, &mentions), now)
            .await
            .unwrap();

        assert_eq!(due(&mut conn, now).await.unwrap(), []);
        let kept: (String, String) =
            sqlx::query_as("SELECT text, mentions FROM reminders WHERE id = ?")
                .bind(id)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(kept, (String::new(), String::new()));
    }

    #[tokio::test]
    async fn found_by_message_id_in_its_own_chat_only() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let chat = group(&mut conn).await;
        let todo = NewReminder {
            source: Source::Todo,
            source_id: Some(7),
            ..dentist(chat, &[])
        };
        let id = record_sending(&mut conn, &todo, utc("2026-11-13T10:00:00Z"))
            .await
            .unwrap();
        assert_eq!(find(&mut conn, "3EB0AA", GROUP).await.unwrap(), None);

        set_message(&mut conn, id, "3EB0AA").await.unwrap();
        assert_eq!(
            find(&mut conn, "3EB0AA", GROUP).await.unwrap(),
            Some(Found {
                source: Source::Todo,
                source_id: Some(7),
            })
        );
        let elsewhere = find(&mut conn, "3EB0AA", "120363000000000009@g.us").await;
        assert_eq!(elsewhere.unwrap(), None, "same message ID, other chat");
    }

    #[tokio::test]
    async fn a_left_chat_has_nothing_due() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let chat = group(&mut conn).await;
        schedule(&mut conn, &dentist(chat, &[])).await.unwrap();
        crate::repo::cleanup::set_left(&mut conn, chat, Some(utc("2026-11-12T00:00:00Z")))
            .await
            .unwrap();
        assert_eq!(
            due(&mut conn, utc("2026-11-14T00:00:00Z")).await.unwrap(),
            []
        );
    }
}

//! Who wants todo reminders, and which one they had last.

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;

/// Someone who has todo reminders on in a chat.
#[derive(Debug, Clone, PartialEq)]
pub struct Subscriber {
    pub chat_id: i64,
    pub wa_chat_id: String,
    /// `Some` in a DM (its owner); `None` in a group.
    pub dm_user_id: Option<i64>,
    pub user_id: i64,
    pub display_name: String,
    /// Phone-number ID, for the @mention in a group.
    pub wa_jid: String,
}

/// Turns reminders on. Doing it again restarts from `now`.
pub async fn enable(
    conn: &mut SqliteConnection,
    chat_id: i64,
    user_id: i64,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO todo_reminders (chat_id, user_id, last_slot_at)
         VALUES (?, ?, ?)
         ON CONFLICT (chat_id, user_id) DO UPDATE SET last_slot_at = excluded.last_slot_at",
        chat_id,
        user_id,
        now
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn disable(
    conn: &mut SqliteConnection,
    chat_id: i64,
    user_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "DELETE FROM todo_reminders WHERE chat_id = ? AND user_id = ?",
        chat_id,
        user_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn is_on(
    conn: &mut SqliteConnection,
    chat_id: i64,
    user_id: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM todo_reminders WHERE chat_id = ? AND user_id = ?)
           AS "on: bool""#,
        chat_id,
        user_id
    )
    .fetch_one(conn)
    .await
}

pub async fn subscribers(conn: &mut SqliteConnection) -> Result<Vec<Subscriber>, sqlx::Error> {
    sqlx::query_as!(
        Subscriber,
        "SELECT c.id AS chat_id, c.wa_chat_id, c.dm_user_id,
                u.id AS user_id, u.display_name, u.wa_jid
         FROM todo_reminders r
         JOIN chats c ON c.id = r.chat_id
         JOIN users u ON u.id = r.user_id"
    )
    .fetch_all(conn)
    .await
}

/// Claims the reminder at `slot_at` for this person. `true` the first time
/// only, so it's sent once even if the loop runs again or restarts.
///
/// `datetime(...)` compares the moments, not the text of the timestamps.
pub async fn claim(
    conn: &mut SqliteConnection,
    chat_id: i64,
    user_id: i64,
    slot_at: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "UPDATE todo_reminders SET last_slot_at = ?
         WHERE chat_id = ? AND user_id = ? AND datetime(last_slot_at) < datetime(?)",
        slot_at,
        chat_id,
        user_id,
        slot_at
    )
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::person::Person;
    use crate::repo::{chats, users};

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    /// A user and a group they're in.
    async fn setup(conn: &mut SqliteConnection) -> (i64, i64) {
        let now = at("2026-10-31T09:00:00Z");
        let bilal = Person {
            name: "Bilal".to_string(),
            jid: "447700900001@s.whatsapp.net".to_string(),
            lid: None,
        };
        let user = users::find_or_register(conn, &bilal, now)
            .await
            .unwrap()
            .unwrap();
        let chat = chats::find_or_create(conn, "120363000000000001@g.us", None, now)
            .await
            .unwrap();
        (chat, user.id)
    }

    #[tokio::test]
    async fn off_until_turned_on() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let (chat, user) = setup(&mut conn).await;
        let now = at("2026-10-31T09:00:00Z");

        assert!(!is_on(&mut conn, chat, user).await.unwrap());
        assert!(subscribers(&mut conn).await.unwrap().is_empty());

        enable(&mut conn, chat, user, now).await.unwrap();
        assert!(is_on(&mut conn, chat, user).await.unwrap());
        let found = subscribers(&mut conn).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].display_name, "Bilal");

        disable(&mut conn, chat, user).await.unwrap();
        assert!(!is_on(&mut conn, chat, user).await.unwrap());
    }

    #[tokio::test]
    async fn each_slot_is_claimed_once_and_not_one_from_before_turning_on() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let (chat, user) = setup(&mut conn).await;
        // Turned on at 13:10.
        enable(&mut conn, chat, user, at("2026-10-31T13:10:00Z"))
            .await
            .unwrap();

        let earlier = at("2026-10-31T13:00:00Z");
        assert!(!claim(&mut conn, chat, user, earlier).await.unwrap());

        let next = at("2026-10-31T18:00:00Z");
        assert!(claim(&mut conn, chat, user, next).await.unwrap());
        assert!(!claim(&mut conn, chat, user, next).await.unwrap(), "once");
    }
}

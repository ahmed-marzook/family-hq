//! WhatsApp bookkeeping: which messages are already handled, and which
//! rows a numbered list message showed.

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;

/// Records that a message is being handled. Returns `false` if it already
/// was, i.e. this is GOWA retrying the same webhook.
pub async fn mark_processed(
    conn: &mut SqliteConnection,
    wa_message_id: &str,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "INSERT INTO processed_messages (wa_message_id, received_at)
         VALUES (?, ?)
         ON CONFLICT (wa_message_id) DO NOTHING",
        wa_message_id,
        now
    )
    .execute(conn)
    .await?;
    // 1 row inserted = new; 0 = the ID was already there.
    Ok(result.rows_affected() == 1)
}

/// What a list showed at one position.
#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub position: i64,
    /// `"todo"` or `"shop"`.
    pub item_type: String,
    pub item_id: i64,
}

/// Saves the rows a list message showed, numbered from 1 in the order
/// given, so replies to it can be mapped back to them.
pub async fn save_list(
    conn: &mut SqliteConnection,
    wa_message_id: &str,
    chat_id: i64,
    item_type: &str,
    item_ids: &[i64],
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    // `zip(1..)` pairs each id with 1, 2, 3, ...
    for (item_id, position) in item_ids.iter().zip(1_i64..) {
        sqlx::query!(
            "INSERT INTO wa_message_context
                (wa_message_id, chat_id, position, item_type, item_id, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
            wa_message_id,
            chat_id,
            position,
            item_type,
            item_id,
            now
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Everything a list message showed, in order. Empty if `wa_message_id`
/// isn't one of the bot's lists in this chat.
pub async fn list_items(
    conn: &mut SqliteConnection,
    wa_message_id: &str,
    chat_id: i64,
) -> Result<Vec<ListItem>, sqlx::Error> {
    sqlx::query_as!(
        ListItem,
        "SELECT position, item_type, item_id FROM wa_message_context
         WHERE wa_message_id = ? AND chat_id = ?
         ORDER BY position",
        wa_message_id,
        chat_id
    )
    .fetch_all(conn)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::repo::chats;

    #[tokio::test]
    async fn second_mark_is_a_duplicate() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        assert!(mark_processed(&mut conn, "AC1", Utc::now()).await.unwrap());
        assert!(!mark_processed(&mut conn, "AC1", Utc::now()).await.unwrap());
        assert!(mark_processed(&mut conn, "AC2", Utc::now()).await.unwrap());
    }

    #[tokio::test]
    async fn list_round_trip_only_in_its_chat() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let chat = chats::find_or_create(&mut conn, "120363000000000001@g.us", None, now)
            .await
            .unwrap();
        let other = chats::find_or_create(&mut conn, "120363000000000002@g.us", None, now)
            .await
            .unwrap();

        save_list(&mut conn, "3EB0AA", chat, "todo", &[7, 3], now)
            .await
            .unwrap();

        let items = list_items(&mut conn, "3EB0AA", chat).await.unwrap();
        let positions: Vec<(i64, i64)> = items.iter().map(|i| (i.position, i.item_id)).collect();
        assert_eq!(positions, [(1, 7), (2, 3)]);

        assert!(
            list_items(&mut conn, "3EB0AA", other)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            list_items(&mut conn, "NOPE", chat)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

//! Shopping list items. Like todos, every query is limited to one chat, so
//! each group (and each DM) has its own list.

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;

/// An item still to buy, as shown in a `/shop` list.
#[derive(Debug, Clone, PartialEq)]
pub struct ShopItem {
    pub id: i64,
    pub name: String,
}

/// Adds an item to the chat's list and returns its id.
pub async fn add(
    conn: &mut SqliteConnection,
    chat_id: i64,
    name: &str,
    added_by: i64,
    now: DateTime<Utc>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"INSERT INTO shopping_items (chat_id, name, added_by, created_at)
           VALUES (?, ?, ?, ?)
           RETURNING id AS "id!""#,
        chat_id,
        name,
        added_by,
        now
    )
    .fetch_one(conn)
    .await
}

/// Is `name` already on the chat's list, still to buy? Ignores case, so
/// "Milk" and "milk" count as the same item.
pub async fn is_on_list(
    conn: &mut SqliteConnection,
    chat_id: i64,
    name: &str,
) -> Result<bool, sqlx::Error> {
    // `COLLATE NOCASE` on the comparison: case-insensitive for this one
    // check, while the stored name keeps the capitals people typed.
    sqlx::query_scalar!(
        r#"SELECT EXISTS(
               SELECT 1 FROM shopping_items
               WHERE chat_id = ? AND bought_at IS NULL AND name = ? COLLATE NOCASE
           ) AS "on_list: bool""#,
        chat_id,
        name
    )
    .fetch_one(conn)
    .await
}

/// The id of the unbought item called `name` (ignoring case), if any. If
/// the same name is somehow on the list twice, the oldest comes first.
pub async fn find_to_buy(
    conn: &mut SqliteConnection,
    chat_id: i64,
    name: &str,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT id FROM shopping_items
         WHERE chat_id = ? AND bought_at IS NULL AND name = ? COLLATE NOCASE
         ORDER BY created_at, id
         LIMIT 1",
        chat_id,
        name
    )
    .fetch_optional(conn)
    .await
}

/// Items still to buy, oldest first. That order is what the numbers in a
/// `/shop` list refer to.
pub async fn list_to_buy(
    conn: &mut SqliteConnection,
    chat_id: i64,
) -> Result<Vec<ShopItem>, sqlx::Error> {
    sqlx::query_as!(
        ShopItem,
        "SELECT id, name FROM shopping_items
         WHERE chat_id = ? AND bought_at IS NULL
         ORDER BY created_at, id",
        chat_id
    )
    .fetch_all(conn)
    .await
}

/// How many items are bought but not yet cleared.
pub async fn count_bought(conn: &mut SqliteConnection, chat_id: i64) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!: i64" FROM shopping_items
           WHERE chat_id = ? AND bought_at IS NOT NULL"#,
        chat_id
    )
    .fetch_one(conn)
    .await
}

/// Ticks an item off. Returns its name, or `None` if it isn't an unbought
/// item in this chat (already bought, removed, or another chat's).
pub async fn mark_bought(
    conn: &mut SqliteConnection,
    id: i64,
    chat_id: i64,
    by: i64,
    now: DateTime<Utc>,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "UPDATE shopping_items SET bought_by = ?, bought_at = ?
         WHERE id = ? AND chat_id = ? AND bought_at IS NULL
         RETURNING name",
        by,
        now,
        id,
        chat_id
    )
    .fetch_optional(conn)
    .await
}

/// Deletes one item, bought or not. Returns its name, or `None` if there
/// was no such item in this chat.
pub async fn remove(
    conn: &mut SqliteConnection,
    id: i64,
    chat_id: i64,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "DELETE FROM shopping_items WHERE id = ? AND chat_id = ? RETURNING name",
        id,
        chat_id
    )
    .fetch_optional(conn)
    .await
}

/// Deletes every bought item in the chat (`/shop clear`). Returns how many.
pub async fn clear_bought(conn: &mut SqliteConnection, chat_id: i64) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM shopping_items WHERE chat_id = ? AND bought_at IS NOT NULL",
        chat_id
    )
    .execute(conn)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::person::Person;
    use crate::repo::{chats, users};

    /// A user and a group chat; returns (chat_id, user_id).
    async fn setup(conn: &mut SqliteConnection) -> (i64, i64) {
        let now = Utc::now();
        let sender = Person {
            name: "Bilal".to_string(),
            jid: "447700900001@s.whatsapp.net".to_string(),
            lid: None,
        };
        let user = users::find_or_register(conn, &sender, now)
            .await
            .unwrap()
            .unwrap();
        let chat = chats::find_or_create(conn, "120363000000000001@g.us", None, now)
            .await
            .unwrap();
        (chat, user.id)
    }

    #[tokio::test]
    async fn add_buy_clear() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let (chat, user) = setup(&mut conn).await;
        let now = Utc::now();

        let milk = add(&mut conn, chat, "Milk", user, now).await.unwrap();
        let eggs = add(&mut conn, chat, "eggs", user, now).await.unwrap();
        assert!(
            is_on_list(&mut conn, chat, "milk").await.unwrap(),
            "ignores case"
        );

        let names: Vec<String> = list_to_buy(&mut conn, chat)
            .await
            .unwrap()
            .into_iter()
            .map(|i| i.name)
            .collect();
        assert_eq!(names, ["Milk", "eggs"]);

        assert_eq!(
            mark_bought(&mut conn, milk, chat, user, now)
                .await
                .unwrap()
                .as_deref(),
            Some("Milk")
        );
        assert_eq!(
            mark_bought(&mut conn, milk, chat, user, now).await.unwrap(),
            None
        );
        assert!(
            !is_on_list(&mut conn, chat, "milk").await.unwrap(),
            "bought ≠ on list"
        );
        assert_eq!(count_bought(&mut conn, chat).await.unwrap(), 1);

        assert_eq!(clear_bought(&mut conn, chat).await.unwrap(), 1);
        assert_eq!(count_bought(&mut conn, chat).await.unwrap(), 0);
        let left = list_to_buy(&mut conn, chat).await.unwrap();
        assert_eq!(
            left,
            [ShopItem {
                id: eggs,
                name: "eggs".to_string()
            }]
        );
    }

    #[tokio::test]
    async fn another_chat_cannot_touch_the_item() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let (chat, user) = setup(&mut conn).await;
        let now = Utc::now();
        let other = chats::find_or_create(&mut conn, "120363000000000002@g.us", None, now)
            .await
            .unwrap();

        let id = add(&mut conn, chat, "milk", user, now).await.unwrap();
        assert!(list_to_buy(&mut conn, other).await.unwrap().is_empty());
        assert!(!is_on_list(&mut conn, other, "milk").await.unwrap());
        assert_eq!(
            mark_bought(&mut conn, id, other, user, now).await.unwrap(),
            None
        );
        assert_eq!(remove(&mut conn, id, other).await.unwrap(), None);
        assert_eq!(clear_bought(&mut conn, other).await.unwrap(), 0);
    }
}

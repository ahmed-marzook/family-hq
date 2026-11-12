//! WhatsApp bookkeeping: which messages are already handled, and which
//! rows a numbered list message showed.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;

/// Which list a numbered list message showed, as `wa_message_context.item_type`
/// stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    Todo,
    Shop,
}

impl ListKind {
    /// The exact text stored in the database (and allowed by its CHECK).
    pub fn as_db_str(self) -> &'static str {
        match self {
            ListKind::Todo => "todo",
            ListKind::Shop => "shop",
        }
    }
}

impl FromStr for ListKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        [ListKind::Todo, ListKind::Shop]
            .into_iter()
            .find(|k| k.as_db_str() == s)
            .ok_or_else(|| format!("unknown list kind '{s}'"))
    }
}

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
    pub item_id: i64,
}

/// A numbered list message the bot sent: which kind of list it was, and
/// what each number showed (never empty). The kind belongs to the whole
/// list rather than to each item, so nothing has to peek at the first item
/// to find out what the list is.
#[derive(Debug, Clone, PartialEq)]
pub struct SentList {
    pub kind: ListKind,
    pub items: Vec<ListItem>,
}

/// One row of `wa_message_context`, as the queries below read it.
struct Row {
    position: i64,
    item_type: String,
    item_id: i64,
}

/// Turns a list message's rows into a `SentList`, or `None` if there are
/// none (it isn't a list).
fn into_list(rows: Vec<Row>) -> Result<Option<SentList>, sqlx::Error> {
    // A list is saved all at once with one kind, so every row has the same
    // `item_type`; the first says which.
    let Some(first) = rows.first() else {
        return Ok(None);
    };
    let kind = first
        .item_type
        .parse()
        .map_err(|err: String| sqlx::Error::Decode(err.into()))?;
    let items = rows
        .into_iter()
        .map(|r| ListItem {
            position: r.position,
            item_id: r.item_id,
        })
        .collect();
    Ok(Some(SentList { kind, items }))
}

/// Saves the rows a list message showed, numbered from 1 in the order
/// given, so replies to it can be mapped back to them.
pub async fn save_list(
    conn: &mut SqliteConnection,
    wa_message_id: &str,
    chat_id: i64,
    kind: ListKind,
    item_ids: &[i64],
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let item_type = kind.as_db_str();
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

/// Everything a list message showed, in order. `None` if `wa_message_id`
/// isn't one of the bot's lists in this chat.
pub async fn list_items(
    conn: &mut SqliteConnection,
    wa_message_id: &str,
    chat_id: i64,
) -> Result<Option<SentList>, sqlx::Error> {
    let rows = sqlx::query_as!(
        Row,
        "SELECT position, item_type, item_id FROM wa_message_context
         WHERE wa_message_id = ? AND chat_id = ?
         ORDER BY position",
        wa_message_id,
        chat_id
    )
    .fetch_all(conn)
    .await?;
    into_list(rows)
}

/// What the most recent list of `kind` sent in this chat showed, for
/// commands like `+got 1 3` that use numbers without replying. `None` if
/// no such list has been sent.
pub async fn latest_list_items(
    conn: &mut SqliteConnection,
    chat_id: i64,
    kind: ListKind,
) -> Result<Option<SentList>, sqlx::Error> {
    let item_type = kind.as_db_str();
    // The inner SELECT finds the newest list message; the outer one reads
    // all its rows. `rowid` breaks ties when two lists share a timestamp.
    let rows = sqlx::query_as!(
        Row,
        "SELECT position, item_type, item_id FROM wa_message_context
         WHERE wa_message_id = (
             SELECT wa_message_id FROM wa_message_context
             WHERE chat_id = ?1 AND item_type = ?2
             ORDER BY created_at DESC, rowid DESC
             LIMIT 1
         )
         ORDER BY position",
        chat_id,
        item_type
    )
    .fetch_all(conn)
    .await?;
    into_list(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::repo::chats;

    #[test]
    fn list_kind_round_trips_through_its_db_text() {
        for kind in [ListKind::Todo, ListKind::Shop] {
            assert_eq!(kind.as_db_str().parse::<ListKind>(), Ok(kind));
        }
        assert!("shopping".parse::<ListKind>().is_err());
        assert!("".parse::<ListKind>().is_err());
    }

    #[tokio::test]
    async fn latest_list_is_the_newest_of_its_type_in_the_chat() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let t = |s: &str| s.parse::<chrono::DateTime<Utc>>().unwrap();
        let chat = chats::find_or_create(&mut conn, "120363000000000001@g.us", None, Utc::now())
            .await
            .unwrap();

        assert!(
            latest_list_items(&mut conn, chat, ListKind::Shop)
                .await
                .unwrap()
                .is_none()
        );

        save_list(
            &mut conn,
            "OLD",
            chat,
            ListKind::Shop,
            &[1, 2],
            t("2026-10-27T10:00:00Z"),
        )
        .await
        .unwrap();
        save_list(
            &mut conn,
            "NEW",
            chat,
            ListKind::Shop,
            &[5],
            t("2026-10-27T11:00:00Z"),
        )
        .await
        .unwrap();
        save_list(
            &mut conn,
            "TODO",
            chat,
            ListKind::Todo,
            &[9],
            t("2026-10-27T12:00:00Z"),
        )
        .await
        .unwrap();

        let list = latest_list_items(&mut conn, chat, ListKind::Shop)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(list.kind, ListKind::Shop);
        let ids: Vec<i64> = list.items.iter().map(|i| i.item_id).collect();
        assert_eq!(ids, [5], "newest shop list, not the newer todo list");
    }

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

        save_list(&mut conn, "3EB0AA", chat, ListKind::Todo, &[7, 3], now)
            .await
            .unwrap();

        let list = list_items(&mut conn, "3EB0AA", chat)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(list.kind, ListKind::Todo);
        let positions: Vec<(i64, i64)> =
            list.items.iter().map(|i| (i.position, i.item_id)).collect();
        assert_eq!(positions, [(1, 7), (2, 3)]);

        assert!(
            list_items(&mut conn, "3EB0AA", other)
                .await
                .unwrap()
                .is_none()
        );
        assert!(list_items(&mut conn, "NOPE", chat).await.unwrap().is_none());
    }
}

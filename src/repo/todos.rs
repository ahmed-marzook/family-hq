//! Todos. Every query is limited to one chat (`chat_id`), so one group can
//! never see or change another group's list.

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;

/// One open todo, as shown in a `/todo` list.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenTodo {
    pub id: i64,
    pub title: String,
    /// The assignee's display name, if there is one.
    pub assignee: Option<String>,
}

/// Adds a pending todo and returns its id.
pub async fn add(
    conn: &mut SqliteConnection,
    chat_id: i64,
    title: &str,
    created_by: i64,
    assignee_id: Option<i64>,
    now: DateTime<Utc>,
) -> Result<i64, sqlx::Error> {
    // `AS "id!"`: the `!` tells the macro this column is never NULL.
    // SQLite can't work that out for `RETURNING`, so without it the macro
    // would make it `Option<i64>`.
    sqlx::query_scalar!(
        r#"INSERT INTO todos (chat_id, title, created_by, assignee_id, created_at)
           VALUES (?, ?, ?, ?, ?)
           RETURNING id AS "id!""#,
        chat_id,
        title,
        created_by,
        assignee_id,
        now
    )
    .fetch_one(conn)
    .await
}

/// The chat's pending todos, oldest first. That order is what the numbers
/// in a `/todo` list refer to.
pub async fn list_open(
    conn: &mut SqliteConnection,
    chat_id: i64,
) -> Result<Vec<OpenTodo>, sqlx::Error> {
    // `LEFT JOIN`: keep todos with no assignee (their name is NULL).
    // `as "assignee?"`: the `?` tells the macro this column can be NULL,
    // so it becomes `Option<String>`. SQLite can't always work that out
    // for joined columns by itself.
    sqlx::query_as!(
        OpenTodo,
        r#"SELECT t.id, t.title, u.display_name AS "assignee?"
           FROM todos t
           LEFT JOIN users u ON u.id = t.assignee_id
           WHERE t.chat_id = ? AND t.status = 'pending'
           ORDER BY t.created_at, t.id"#,
        chat_id
    )
    .fetch_all(conn)
    .await
}

/// Marks a todo done. Returns its title, or `None` if it isn't a pending
/// todo in this chat (already done, removed, or another chat's).
pub async fn complete(
    conn: &mut SqliteConnection,
    id: i64,
    chat_id: i64,
    by: i64,
    now: DateTime<Utc>,
) -> Result<Option<String>, sqlx::Error> {
    // One statement checks and changes, so there's no gap between "is it
    // still pending?" and "mark it done" for someone else to slip into.
    sqlx::query_scalar!(
        "UPDATE todos
         SET status = 'completed', completed_by = ?, completed_at = ?
         WHERE id = ? AND chat_id = ? AND status = 'pending'
         RETURNING title",
        by,
        now,
        id,
        chat_id
    )
    .fetch_optional(conn)
    .await
}

/// Deletes a todo. Returns its title, or `None` if there was no such todo
/// in this chat.
pub async fn remove(
    conn: &mut SqliteConnection,
    id: i64,
    chat_id: i64,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "DELETE FROM todos WHERE id = ? AND chat_id = ? RETURNING title",
        id,
        chat_id
    )
    .fetch_optional(conn)
    .await
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
    async fn add_list_complete_remove() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let (chat, user) = setup(&mut conn).await;
        let now = Utc::now();

        let gym = add(&mut conn, chat, "go gym", user, Some(user), now)
            .await
            .unwrap();
        let tap = add(&mut conn, chat, "fix the tap", user, None, now)
            .await
            .unwrap();

        let open = list_open(&mut conn, chat).await.unwrap();
        assert_eq!(
            open,
            [
                OpenTodo {
                    id: gym,
                    title: "go gym".to_string(),
                    assignee: Some("Bilal".to_string())
                },
                OpenTodo {
                    id: tap,
                    title: "fix the tap".to_string(),
                    assignee: None
                },
            ]
        );

        assert_eq!(
            complete(&mut conn, gym, chat, user, now)
                .await
                .unwrap()
                .as_deref(),
            Some("go gym")
        );
        // Completing twice: nothing left to complete.
        assert_eq!(
            complete(&mut conn, gym, chat, user, now).await.unwrap(),
            None
        );

        assert_eq!(
            remove(&mut conn, tap, chat).await.unwrap().as_deref(),
            Some("fix the tap")
        );
        assert!(list_open(&mut conn, chat).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn another_chat_cannot_touch_the_todo() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let (chat, user) = setup(&mut conn).await;
        let now = Utc::now();
        let other = chats::find_or_create(&mut conn, "120363000000000002@g.us", None, now)
            .await
            .unwrap();

        let id = add(&mut conn, chat, "go gym", user, None, now)
            .await
            .unwrap();
        assert!(list_open(&mut conn, other).await.unwrap().is_empty());
        assert_eq!(
            complete(&mut conn, id, other, user, now).await.unwrap(),
            None
        );
        assert_eq!(remove(&mut conn, id, other).await.unwrap(), None);
    }
}

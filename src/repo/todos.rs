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

/// One todo as `/todo` shows it: open or done.
#[derive(Debug, Clone, PartialEq)]
pub struct ListedTodo {
    pub id: i64,
    pub title: String,
    /// The assignee's display name, if there is one.
    pub assignee: Option<String>,
    /// Who completed it and when; `None` while it's open.
    pub done: Option<Completion>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    /// Display name of whoever ticked it off.
    pub by: String,
    pub at: DateTime<Utc>,
}

/// The whole list, as `/todo` shows it: open todos (oldest first), then
/// done ones (in the order they were done). That order is what the numbers
/// in a `/todo` list refer to.
pub async fn list_all(
    conn: &mut SqliteConnection,
    chat_id: i64,
) -> Result<Vec<ListedTodo>, sqlx::Error> {
    // Two LEFT JOINs on `users`, under two names (`a` for the assignee, `c`
    // for whoever completed it), because a todo points at two people.
    // `as "x: DateTime<Utc>"` reads the stored text as a chrono time.
    let rows = sqlx::query!(
        r#"SELECT t.id, t.title,
                  a.display_name AS "assignee?",
                  c.display_name AS "completed_by?",
                  t.completed_at AS "completed_at?: DateTime<Utc>"
           FROM todos t
           LEFT JOIN users a ON a.id = t.assignee_id
           LEFT JOIN users c ON c.id = t.completed_by
           WHERE t.chat_id = ?
           ORDER BY t.status = 'completed', t.completed_at, t.created_at, t.id"#,
        chat_id
    )
    .fetch_all(conn)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| ListedTodo {
            id: r.id,
            title: r.title,
            assignee: r.assignee,
            // `zip` pairs two `Option`s: `Some` only if both are `Some`.
            done: r
                .completed_by
                .zip(r.completed_at)
                .map(|(by, at)| Completion { by, at }),
        })
        .collect())
}

/// Open todos whose title matches `words`, for `+done gym`: an exact title
/// (ignoring case) wins; otherwise every open todo whose title contains
/// it. The caller decides what to do with none, one or several.
pub async fn find_open(
    conn: &mut SqliteConnection,
    chat_id: i64,
    words: &str,
) -> Result<Vec<(i64, String)>, sqlx::Error> {
    let exact = sqlx::query!(
        "SELECT id, title FROM todos
         WHERE chat_id = ? AND status = 'pending' AND title = ? COLLATE NOCASE
         ORDER BY created_at, id",
        chat_id,
        words
    )
    .fetch_all(&mut *conn)
    .await?;
    if !exact.is_empty() {
        return Ok(exact.into_iter().map(|r| (r.id, r.title)).collect());
    }

    // `LIKE` is case-insensitive for ASCII in SQLite. `%` matches anything,
    // so `%gym%` finds "go gym". The `||` joins strings in SQL.
    let partial = sqlx::query!(
        "SELECT id, title FROM todos
         WHERE chat_id = ? AND status = 'pending' AND title LIKE '%' || ? || '%'
         ORDER BY created_at, id",
        chat_id,
        words
    )
    .fetch_all(conn)
    .await?;
    Ok(partial.into_iter().map(|r| (r.id, r.title)).collect())
}

/// (done, total) for the chat, for the progress bar.
pub async fn progress(
    conn: &mut SqliteConnection,
    chat_id: i64,
) -> Result<(i64, i64), sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT COUNT(*) FILTER (WHERE status = 'completed') AS "done!: i64",
                  COUNT(*) AS "total!: i64"
           FROM todos WHERE chat_id = ?"#,
        chat_id
    )
    .fetch_one(conn)
    .await?;
    Ok((row.done, row.total))
}

/// Deletes the chat's completed todos (`/todo clear done`). Returns how many.
pub async fn clear_done(conn: &mut SqliteConnection, chat_id: i64) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM todos WHERE chat_id = ? AND status = 'completed'",
        chat_id
    )
    .execute(conn)
    .await?;
    Ok(result.rows_affected())
}

/// Deletes every todo in the chat (`/todo clear`). Returns how many.
pub async fn clear_all(conn: &mut SqliteConnection, chat_id: i64) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!("DELETE FROM todos WHERE chat_id = ?", chat_id)
        .execute(conn)
        .await?;
    Ok(result.rows_affected())
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

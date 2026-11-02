//! The chats the bot works in, and who has been seen in each.

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;

/// Finds the chat with this WhatsApp ID, creating it on first use.
/// Returns its `chats.id`, which every list row points at.
///
/// `dm_owner` is the user whose DM this is; pass `None` for a group.
pub async fn find_or_create(
    conn: &mut SqliteConnection,
    wa_chat_id: &str,
    dm_owner: Option<i64>,
    now: DateTime<Utc>,
) -> Result<i64, sqlx::Error> {
    // Look first, insert only if missing. (An `INSERT ... ON CONFLICT DO
    // NOTHING` on every message would also work, but each attempt uses up
    // an AUTOINCREMENT number, leaving gaps in the ids.) The webhook runs
    // this inside a transaction, so nothing can sneak in between the two.
    let existing = sqlx::query_scalar!("SELECT id FROM chats WHERE wa_chat_id = ?", wa_chat_id)
        .fetch_optional(&mut *conn)
        .await?;
    if let Some(id) = existing {
        return Ok(id);
    }

    let kind = if dm_owner.is_some() { "dm" } else { "group" };
    sqlx::query_scalar!(
        r#"INSERT INTO chats (wa_chat_id, kind, dm_user_id, created_at)
           VALUES (?, ?, ?, ?)
           RETURNING id AS "id!""#,
        wa_chat_id,
        kind,
        dm_owner,
        now
    )
    .fetch_one(conn)
    .await
}

/// Records that `user_id` is in `chat_id`. Safe to call on every message.
pub async fn add_member(
    conn: &mut SqliteConnection,
    chat_id: i64,
    user_id: i64,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO chat_members (chat_id, user_id, joined_at)
         VALUES (?, ?, ?)
         ON CONFLICT (chat_id, user_id) DO NOTHING",
        chat_id,
        user_id,
        now
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Saves a group's name, as WhatsApp has it. Does nothing for a group the
/// bot hasn't been used in yet (no `chats` row), and never touches a DM.
pub async fn set_group_name(
    conn: &mut SqliteConnection,
    wa_chat_id: &str,
    name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE chats SET name = ? WHERE wa_chat_id = ? AND kind = 'group'",
        name,
        wa_chat_id
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
    use crate::repo::users;

    #[tokio::test]
    async fn group_names_are_saved_for_groups_only() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let group_id = "120363000000000001@g.us";
        let group = find_or_create(&mut conn, group_id, None, now)
            .await
            .unwrap();

        set_group_name(&mut conn, group_id, "Family").await.unwrap();
        // A group the bot hasn't been used in: nothing to update, no error.
        set_group_name(&mut conn, "120363000000000009@g.us", "Other")
            .await
            .unwrap();

        let names: Vec<(i64, Option<String>)> = sqlx::query_as("SELECT id, name FROM chats")
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        assert_eq!(names, [(group, Some("Family".to_string()))]);
    }

    #[tokio::test]
    async fn same_chat_id_gives_the_same_chat() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();

        let first = find_or_create(&mut conn, "120363000000000001@g.us", None, now)
            .await
            .unwrap();
        let again = find_or_create(&mut conn, "120363000000000001@g.us", None, now)
            .await
            .unwrap();
        let other = find_or_create(&mut conn, "120363000000000002@g.us", None, now)
            .await
            .unwrap();
        assert_eq!(first, again);
        assert_ne!(first, other);
    }

    #[tokio::test]
    async fn dm_and_membership() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let jid = "447700900001@s.whatsapp.net";
        let sender = Person {
            name: "Bilal".to_string(),
            jid: jid.to_string(),
            lid: None,
        };
        let user = users::find_or_register(&mut conn, &sender, now)
            .await
            .unwrap()
            .unwrap();

        let dm = find_or_create(&mut conn, jid, Some(user.id), now)
            .await
            .unwrap();
        add_member(&mut conn, dm, user.id, now).await.unwrap();
        add_member(&mut conn, dm, user.id, now).await.unwrap(); // no error twice

        let names = users::member_names(&mut conn, dm).await.unwrap();
        assert_eq!(names, ["Bilal"]);
    }
}

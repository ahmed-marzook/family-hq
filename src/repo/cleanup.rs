//! Clearing a person's data (see `crate::cleanup` for when, and why).
//!
//! Deleted: their DM and everything in it (todos, shopping, settings,
//! reminders sent there, list numbers), their group memberships, their todo
//! reminders anywhere, their prayer log and their usage log. Then their
//! `users` row, unless a group's todo or shopping list still names them
//! (group lists are the family's and are never touched); in that case the
//! row stays with `cleared_at` set, until `delete_if_unnamed` can remove
//! it.

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;

use crate::cleanup::Activity;

/// Someone, with what the cleanup needs to know about them.
#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    pub id: i64,
    pub name: String,
    pub last_active: DateTime<Utc>,
    pub warned_at: Option<DateTime<Utc>>,
    pub cleared_at: Option<DateTime<Utc>>,
    /// Their DM's chat ID, where a warning goes. `None` without a DM.
    pub dm_chat: Option<String>,
}

impl Person {
    pub fn activity(&self) -> Activity {
        Activity {
            last_active: self.last_active,
            warned_at: self.warned_at,
            cleared_at: self.cleared_at,
            can_warn: self.dm_chat.is_some(),
        }
    }
}

/// Everyone.
pub async fn people(conn: &mut SqliteConnection) -> Result<Vec<Person>, sqlx::Error> {
    // `coalesce(a, b)`: `a`, or `b` if `a` is NULL. Rows from before
    // migration 0006 had their `last_active_at` filled in by it, but a
    // fallback costs nothing.
    sqlx::query_as!(
        Person,
        r#"SELECT u.id AS "id!", u.display_name AS name,
                  coalesce(u.last_active_at, u.created_at) AS "last_active!: DateTime<Utc>",
                  u.warned_at AS "warned_at?: DateTime<Utc>",
                  u.cleared_at AS "cleared_at?: DateTime<Utc>",
                  (SELECT c.wa_chat_id FROM chats c WHERE c.dm_user_id = u.id) AS "dm_chat?"
           FROM users u
           ORDER BY u.id"#
    )
    .fetch_all(conn)
    .await
}

pub async fn set_warned(
    conn: &mut SqliteConnection,
    user_id: i64,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query!("UPDATE users SET warned_at = ? WHERE id = ?", now, user_id)
        .execute(conn)
        .await?;
    Ok(())
}

/// What clearing did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cleared {
    /// The `users` row went too (no group list names them).
    pub deleted_user: bool,
}

/// Clears the person's data, but only if they haven't been active since
/// `last_active` (the cleanup read that a moment ago; a message since then
/// keeps their data). `None` if they were active, or don't exist.
///
/// Run it in a transaction, so it's all or nothing.
pub async fn clear_if_inactive(
    conn: &mut SqliteConnection,
    user_id: i64,
    last_active: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Option<Cleared>, sqlx::Error> {
    let current = sqlx::query_scalar!(
        r#"SELECT coalesce(last_active_at, created_at) AS "at!: DateTime<Utc>"
           FROM users WHERE id = ?"#,
        user_id
    )
    .fetch_optional(&mut *conn)
    .await?;
    if current != Some(last_active) {
        return Ok(None);
    }
    clear(conn, user_id, now).await.map(Some)
}

/// Clears the person's data now, whatever their activity (the admin
/// dashboard's "Clear now"). Run it in a transaction.
pub async fn clear(
    conn: &mut SqliteConnection,
    user_id: i64,
    now: DateTime<Utc>,
) -> Result<Cleared, sqlx::Error> {
    // Their DM and everything in it.
    let dm = sqlx::query_scalar!(
        r#"SELECT id AS "id!" FROM chats WHERE dm_user_id = ?"#,
        user_id
    )
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(dm) = dm {
        delete_chat(conn, dm).await?;
    }

    // What's theirs in groups (and anywhere else).
    sqlx::query!("DELETE FROM chat_members WHERE user_id = ?", user_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!("DELETE FROM todo_reminders WHERE user_id = ?", user_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!("DELETE FROM prayer_log WHERE user_id = ?", user_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!("DELETE FROM points WHERE user_id = ?", user_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!("DELETE FROM command_log WHERE user_id = ?", user_id)
        .execute(&mut *conn)
        .await?;
    // Their push topic and token go too, so a button on an old push stops
    // working.
    sqlx::query!("DELETE FROM push_subscriptions WHERE user_id = ?", user_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!("DELETE FROM pushes_sent WHERE user_id = ?", user_id)
        .execute(&mut *conn)
        .await?;

    if delete_if_unnamed(conn, user_id).await? {
        return Ok(Cleared { deleted_user: true });
    }
    sqlx::query!(
        "UPDATE users SET cleared_at = ?, warned_at = NULL WHERE id = ?",
        now,
        user_id
    )
    .execute(conn)
    .await?;
    Ok(Cleared {
        deleted_user: false,
    })
}

/// Deletes a chat (a DM, or a group the bot left) and everything in it:
/// its lists, settings, reminders sent there and list numbers, and, by
/// `ON DELETE CASCADE`, its members, todo reminders and usage log. Run it in
/// a transaction.
pub async fn delete_chat(conn: &mut SqliteConnection, chat_id: i64) -> Result<(), sqlx::Error> {
    // Contents first: the tables that point at a chat without `ON DELETE
    // CASCADE` would otherwise stop the chat being deleted.
    sqlx::query!("DELETE FROM todos WHERE chat_id = ?", chat_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!("DELETE FROM points WHERE chat_id = ?", chat_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!("DELETE FROM shopping_items WHERE chat_id = ?", chat_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!("DELETE FROM wa_message_context WHERE chat_id = ?", chat_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!("DELETE FROM chat_settings WHERE chat_id = ?", chat_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!("DELETE FROM prayer_reminders WHERE chat_id = ?", chat_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!("DELETE FROM report_settings WHERE chat_id = ?", chat_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!("DELETE FROM reports_sent WHERE chat_id = ?", chat_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!("DELETE FROM chats WHERE id = ?", chat_id)
        .execute(conn)
        .await?;
    Ok(())
}

/// A group the bot has been used in.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredGroup {
    pub id: i64,
    pub wa_chat_id: String,
    pub name: Option<String>,
    /// When the bot was first seen to be no longer in it.
    pub left_at: Option<DateTime<Utc>>,
    /// When someone last used the bot in it.
    pub last_active: DateTime<Utc>,
    /// When the bot warned the group it would leave.
    pub warned_at: Option<DateTime<Utc>>,
}

impl StoredGroup {
    /// The group's activity, in the terms `crate::cleanup` uses for people.
    /// A group can always be warned: the bot posts in it.
    pub fn activity(&self) -> Activity {
        Activity {
            last_active: self.last_active,
            warned_at: self.warned_at,
            cleared_at: None,
            can_warn: true,
        }
    }
}

pub async fn groups(conn: &mut SqliteConnection) -> Result<Vec<StoredGroup>, sqlx::Error> {
    sqlx::query_as!(
        StoredGroup,
        r#"SELECT id AS "id!", wa_chat_id, name, left_at AS "left_at?: DateTime<Utc>",
                  coalesce(last_active_at, created_at) AS "last_active!: DateTime<Utc>",
                  warned_at AS "warned_at?: DateTime<Utc>"
           FROM chats WHERE kind = 'group' ORDER BY id"#
    )
    .fetch_all(conn)
    .await
}

/// Notes when the bot was first seen to be out of the group (`None`: it's
/// back in).
pub async fn set_left(
    conn: &mut SqliteConnection,
    chat_id: i64,
    at: Option<DateTime<Utc>>,
) -> Result<(), sqlx::Error> {
    sqlx::query!("UPDATE chats SET left_at = ? WHERE id = ?", at, chat_id)
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn set_group_warned(
    conn: &mut SqliteConnection,
    chat_id: i64,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query!("UPDATE chats SET warned_at = ? WHERE id = ?", now, chat_id)
        .execute(conn)
        .await?;
    Ok(())
}

/// Whether nobody has used the bot in the group since `last_active` (read
/// a moment ago by the cleanup): a command in the meantime keeps it.
pub async fn group_still_inactive(
    conn: &mut SqliteConnection,
    chat_id: i64,
    last_active: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let current = sqlx::query_scalar!(
        r#"SELECT coalesce(last_active_at, created_at) AS "at!: DateTime<Utc>"
           FROM chats WHERE id = ? AND kind = 'group'"#,
        chat_id
    )
    .fetch_optional(conn)
    .await?;
    Ok(current == Some(last_active))
}

/// Deletes the person's `users` row if no todo or shopping item names them
/// any more. `true` if it did.
pub async fn delete_if_unnamed(
    conn: &mut SqliteConnection,
    user_id: i64,
) -> Result<bool, sqlx::Error> {
    let named = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM todos
                         WHERE created_by = ?1 OR assignee_id = ?1 OR closed_by = ?1)
               OR EXISTS(SELECT 1 FROM shopping_items
                         WHERE added_by = ?1 OR bought_by = ?1) AS "named!: bool""#,
        user_id
    )
    .fetch_one(&mut *conn)
    .await?;
    if named {
        return Ok(false);
    }
    let deleted = sqlx::query!("DELETE FROM users WHERE id = ?", user_id)
        .execute(conn)
        .await?;
    Ok(deleted.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;
    use crate::db::test_pool;
    use crate::geo::Coordinates;
    use crate::metrics::{Event, Kind};
    use crate::points::{Award, Reason, Source};
    use crate::repo::metrics::{Logged, record};
    use crate::repo::{chats, points, prayers, settings, shop, todo_reminders, todos, users};
    use crate::testkit::seed_family;

    async fn count(conn: &mut SqliteConnection, sql: &'static str, id: i64) -> i64 {
        sqlx::query_scalar(sql)
            .bind(id)
            .fetch_one(conn)
            .await
            .unwrap()
    }

    /// Bilal with a full DM (lists, settings, reminders, log) and a group
    /// with Khadija. Returns (Bilal, Khadija, group, Bilal's DM).
    async fn seed(conn: &mut SqliteConnection, now: DateTime<Utc>) -> (i64, i64, i64, i64) {
        let family = seed_family(conn, now).await;
        let (bilal, khadija) = (family.bilal.id, family.khadija.id);
        let (group, dm) = (family.group, family.dm);
        // Both are in the group already; Bilal is a member of his DM too.
        chats::add_member(conn, dm, bilal, now).await.unwrap();

        // Bilal's private things.
        todos::add(
            conn,
            dm,
            "dentist",
            bilal,
            Some(bilal),
            Default::default(),
            now,
        )
        .await
        .unwrap();
        shop::add(conn, dm, "gift", bilal, now).await.unwrap();
        let home = Coordinates {
            latitude: 51.1,
            longitude: -0.1,
        };
        settings::set_location(conn, dm, "Crawley", "GB", home, "Europe/London")
            .await
            .unwrap();
        settings::set_prayer_reminders(conn, dm, true)
            .await
            .unwrap();
        todo_reminders::enable(conn, dm, bilal, now).await.unwrap();
        todo_reminders::enable(conn, group, bilal, now)
            .await
            .unwrap();
        prayers::mark_prayed(
            conn,
            bilal,
            now.date_naive(),
            crate::prayer::Prayer::Asr,
            now,
        )
        .await
        .unwrap();
        let event = Event {
            kind: Kind::Command,
            name: "todo.add".to_string(),
            detail: None,
            typed: None,
        };
        for (chat, is_group) in [(dm, false), (group, true)] {
            let row = Logged {
                event: &event,
                chat_id: chat,
                user_id: bilal,
                is_group,
                ok: true,
                duration_ms: 1,
                at: now,
            };
            record(conn, &row).await.unwrap();
        }
        // Points: Bilal's in the group and his DM, Khadija's in the group.
        let award = Award {
            points: 2,
            reason: Reason::Done,
        };
        let today = now.date_naive();
        for (user, chat, todo) in [(bilal, group, 1), (bilal, dm, 2), (khadija, group, 3)] {
            points::award(conn, user, chat, Source::Todo, todo, award, today, now)
                .await
                .unwrap();
        }
        // Khadija's group todo, which must survive.
        todos::add(
            conn,
            group,
            "bins",
            khadija,
            Some(khadija),
            Default::default(),
            now,
        )
        .await
        .unwrap();
        (bilal, khadija, group, dm)
    }

    #[tokio::test]
    async fn clearing_deletes_everything_of_theirs_and_nothing_else() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let (bilal, khadija, group, dm) = seed(&mut conn, now).await;

        let cleared = clear(&mut conn, bilal, now).await.unwrap();
        // Nothing in a group names him, so his row went too.
        assert_eq!(cleared, Cleared { deleted_user: true });

        let c = &mut *conn;
        assert_eq!(
            count(c, "SELECT count(*) FROM users WHERE id = ?", bilal).await,
            0
        );
        assert_eq!(
            count(c, "SELECT count(*) FROM chats WHERE id = ?", dm).await,
            0
        );
        for table in [
            "SELECT count(*) FROM todos WHERE chat_id = ?",
            "SELECT count(*) FROM shopping_items WHERE chat_id = ?",
            "SELECT count(*) FROM chat_settings WHERE chat_id = ?",
        ] {
            assert_eq!(count(c, table, dm).await, 0, "{table}");
        }
        for table in [
            "SELECT count(*) FROM chat_members WHERE user_id = ?",
            "SELECT count(*) FROM todo_reminders WHERE user_id = ?",
            "SELECT count(*) FROM prayer_log WHERE user_id = ?",
            "SELECT count(*) FROM command_log WHERE user_id = ?",
            "SELECT count(*) FROM points WHERE user_id = ?",
        ] {
            assert_eq!(count(c, table, bilal).await, 0, "{table}");
        }
        assert_eq!(
            count(c, "SELECT count(*) FROM points WHERE user_id = ?", khadija).await,
            1,
            "Khadija keeps her points"
        );
        // Khadija, the group and its list are untouched.
        assert_eq!(
            count(c, "SELECT count(*) FROM users WHERE id = ?", khadija).await,
            1
        );
        assert_eq!(
            count(
                c,
                "SELECT count(*) FROM chat_members WHERE user_id = ?",
                khadija
            )
            .await,
            1
        );
        assert_eq!(
            count(c, "SELECT count(*) FROM todos WHERE chat_id = ?", group).await,
            1
        );
    }

    #[tokio::test]
    async fn named_in_a_group_list_keeps_only_the_row_until_it_is_not() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let (bilal, _, group, _) = seed(&mut conn, now).await;
        let tap = todos::add(
            &mut conn,
            group,
            "fix the tap",
            bilal,
            None,
            Default::default(),
            now,
        )
        .await
        .unwrap();

        let cleared = clear(&mut conn, bilal, now).await.unwrap();
        assert_eq!(
            cleared,
            Cleared {
                deleted_user: false
            }
        );
        let kept = people(&mut conn).await.unwrap();
        let bilal_now = kept.iter().find(|p| p.id == bilal).unwrap();
        assert!(bilal_now.cleared_at.is_some());
        assert_eq!(bilal_now.dm_chat, None);
        // The group's todo is still there, with his name on it.
        assert_eq!(
            count(&mut conn, "SELECT count(*) FROM todos WHERE id = ?", tap).await,
            1
        );
        assert!(!delete_if_unnamed(&mut conn, bilal).await.unwrap());

        // Once the group clears its list, the row can go.
        todos::clear_all(&mut conn, group).await.unwrap();
        assert!(delete_if_unnamed(&mut conn, bilal).await.unwrap());
        assert_eq!(
            count(&mut conn, "SELECT count(*) FROM users WHERE id = ?", bilal).await,
            0
        );
    }

    #[tokio::test]
    async fn activity_since_the_check_keeps_their_data() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let (bilal, _, _, dm) = seed(&mut conn, now - Duration::days(30)).await;
        let seen = people(&mut conn)
            .await
            .unwrap()
            .into_iter()
            .find(|p| p.id == bilal)
            .unwrap();

        // A message arrives after the check read them.
        users::touch(&mut conn, bilal, now).await.unwrap();
        let result = clear_if_inactive(&mut conn, bilal, seen.last_active, now)
            .await
            .unwrap();
        assert_eq!(result, None);
        assert_eq!(
            count(&mut conn, "SELECT count(*) FROM chats WHERE id = ?", dm).await,
            1
        );

        // Still as inactive as when read: cleared.
        let seen = people(&mut conn)
            .await
            .unwrap()
            .into_iter()
            .find(|p| p.id == bilal)
            .unwrap();
        let result = clear_if_inactive(&mut conn, bilal, seen.last_active, now)
            .await
            .unwrap();
        assert_eq!(result, Some(Cleared { deleted_user: true }));
    }

    #[tokio::test]
    async fn people_lists_activity_and_dm() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now: DateTime<Utc> = "2026-11-02T09:00:00Z".parse().unwrap();
        let (bilal, khadija, _, _) = seed(&mut conn, now).await;
        set_warned(&mut conn, bilal, now).await.unwrap();

        let all = people(&mut conn).await.unwrap();
        let b = all.iter().find(|p| p.id == bilal).unwrap();
        assert_eq!(b.dm_chat.as_deref(), Some("447700900001@s.whatsapp.net"));
        assert_eq!(b.warned_at, Some(now));
        assert!(b.activity().can_warn);
        let k = all.iter().find(|p| p.id == khadija).unwrap();
        assert_eq!(k.dm_chat, None);
        assert_eq!(k.last_active, now, "falls back to when they joined");
    }

    #[tokio::test]
    async fn deleting_a_group_takes_everything_in_it() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let (bilal, khadija, group, dm) = seed(&mut conn, now).await;
        shop::add(&mut conn, group, "milk", khadija, now)
            .await
            .unwrap();
        settings::set_prayer_reminders(&mut conn, group, true)
            .await
            .unwrap();

        set_left(&mut conn, group, Some(now)).await.unwrap();
        assert_eq!(groups(&mut conn).await.unwrap()[0].left_at, Some(now));
        delete_chat(&mut conn, group).await.unwrap();

        let c = &mut *conn;
        assert!(groups(c).await.unwrap().is_empty());
        for table in [
            "SELECT count(*) FROM todos WHERE chat_id = ?",
            "SELECT count(*) FROM shopping_items WHERE chat_id = ?",
            "SELECT count(*) FROM chat_settings WHERE chat_id = ?",
            "SELECT count(*) FROM chat_members WHERE chat_id = ?",
            "SELECT count(*) FROM todo_reminders WHERE chat_id = ?",
            "SELECT count(*) FROM command_log WHERE chat_id = ?",
            "SELECT count(*) FROM points WHERE chat_id = ?",
        ] {
            assert_eq!(count(c, table, group).await, 0, "{table}");
        }
        assert_eq!(
            count(c, "SELECT count(*) FROM points WHERE chat_id = ?", dm).await,
            1,
            "points in Bilal's DM stay"
        );
        // People stay (the inactive cleanup deals with them), and so does
        // Bilal's DM.
        assert_eq!(
            count(c, "SELECT count(*) FROM users WHERE id = ?", bilal).await,
            1
        );
        assert_eq!(
            count(c, "SELECT count(*) FROM chats WHERE id = ?", dm).await,
            1
        );
        // Khadija's only list items were in the group, so nothing names her.
        assert!(delete_if_unnamed(c, khadija).await.unwrap());
    }
}

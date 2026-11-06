//! Read-only queries for the admin dashboard (`routes::admin`).
//!
//! A DM's lists are private, so they're only ever *counted* here: nothing
//! in this file returns a DM's todo titles or shopping items, and `group`
//! finds group chats only.

use std::cmp::Reverse;

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;

/// One person, with how much they use the bot.
#[derive(Debug, Clone, PartialEq)]
pub struct UserSummary {
    pub id: i64,
    pub name: String,
    /// When they first messaged the bot.
    pub first_seen: DateTime<Utc>,
    /// Their latest command, list reply or reaction; `None` if they've only
    /// ever chatted (which isn't logged).
    pub last_seen: Option<DateTime<Utc>>,
    /// How many times they used the bot since the `since` passed in.
    pub uses_since: i64,
    /// Groups the bot has seen them in.
    pub groups: i64,
    /// Whether they've messaged the bot privately.
    pub has_dm: bool,
    /// When they last used the bot (`crate::cleanup`'s idea of activity,
    /// which also counts any message in their DM).
    pub last_active: DateTime<Utc>,
    /// When they were warned their data will be cleared; `None` if not.
    pub warned_at: Option<DateTime<Utc>>,
    /// When their data was cleared (only their name is left); `None` if not.
    pub cleared_at: Option<DateTime<Utc>>,
}

/// Everyone (or just user `only`), most recently active first.
pub async fn users(
    conn: &mut SqliteConnection,
    only: Option<i64>,
    since: DateTime<Utc>,
) -> Result<Vec<UserSummary>, sqlx::Error> {
    // Each `(SELECT ...)` in the column list is a *subquery*: it runs once
    // per user and gives one value. `datetime(...)` puts both times in the
    // same text form before comparing, as in docs/METRICS.md.
    // `?1 IS NULL OR u.id = ?1`: with no `only`, every user matches.
    let mut rows = sqlx::query_as!(
        UserSummary,
        r#"SELECT u.id AS "id!", u.display_name AS name,
                  u.created_at AS "first_seen: DateTime<Utc>",
                  (SELECT max(l.at) FROM command_log l WHERE l.user_id = u.id)
                      AS "last_seen?: DateTime<Utc>",
                  (SELECT count(*) FROM command_log l
                   WHERE l.user_id = u.id AND datetime(l.at) >= datetime(?2)) AS "uses_since!: i64",
                  (SELECT count(*) FROM chat_members m JOIN chats c ON c.id = m.chat_id
                   WHERE m.user_id = u.id AND c.kind = 'group') AS "groups!: i64",
                  EXISTS(SELECT 1 FROM chats c WHERE c.dm_user_id = u.id) AS "has_dm!: bool",
                  coalesce(u.last_active_at, u.created_at) AS "last_active!: DateTime<Utc>",
                  u.warned_at AS "warned_at?: DateTime<Utc>",
                  u.cleared_at AS "cleared_at?: DateTime<Utc>"
           FROM users u
           WHERE ?1 IS NULL OR u.id = ?1
           ORDER BY u.display_name"#,
        only,
        since
    )
    .fetch_all(conn)
    .await?;

    // Most recent first. `Reverse` flips the order; `Option` sorts `None`
    // before any `Some`, so flipped, the never-seen come last. The sort is
    // stable: people with the same time stay A to Z.
    rows.sort_by_key(|u| Reverse(u.last_seen));
    Ok(rows)
}

/// A chat someone is in, with how much is on its lists.
#[derive(Debug, Clone, PartialEq)]
pub struct UserChat {
    pub id: i64,
    /// `"group"` or `"dm"`.
    pub kind: String,
    /// The group's name; `None` for a DM, or a group whose name isn't known.
    pub name: Option<String>,
    pub open_todos: i64,
    pub done_todos: i64,
    pub to_buy: i64,
    pub bought: i64,
    /// `/todo reminders on`, for this person in this chat.
    pub todo_reminders: bool,
    /// When someone last used the bot in the chat (for a group; a DM's
    /// activity is its owner's).
    pub last_active: DateTime<Utc>,
    /// When the bot warned the group it would leave.
    pub warned_at: Option<DateTime<Utc>>,
    /// When the bot was found to be no longer in the group.
    pub left_at: Option<DateTime<Utc>>,
}

/// The person's DM (if any) and the groups they've been seen in, DM first,
/// then groups A to Z. The counts are for the whole chat's lists.
pub async fn chats_of(
    conn: &mut SqliteConnection,
    user_id: i64,
) -> Result<Vec<UserChat>, sqlx::Error> {
    // `ORDER BY c.kind = 'group'`: false (0) for the DM, so it comes first.
    sqlx::query_as!(
        UserChat,
        r#"SELECT c.id AS "id!", c.kind, c.name,
                  (SELECT count(*) FROM todos t
                   WHERE t.chat_id = c.id AND t.status = 'pending') AS "open_todos!: i64",
                  (SELECT count(*) FROM todos t
                   WHERE t.chat_id = c.id AND t.status = 'completed') AS "done_todos!: i64",
                  (SELECT count(*) FROM shopping_items s
                   WHERE s.chat_id = c.id AND s.bought_at IS NULL) AS "to_buy!: i64",
                  (SELECT count(*) FROM shopping_items s
                   WHERE s.chat_id = c.id AND s.bought_at IS NOT NULL) AS "bought!: i64",
                  EXISTS(SELECT 1 FROM todo_reminders r
                         WHERE r.chat_id = c.id AND r.user_id = ?1) AS "todo_reminders!: bool",
                  coalesce(c.last_active_at, c.created_at) AS "last_active!: DateTime<Utc>",
                  c.warned_at AS "warned_at?: DateTime<Utc>",
                  c.left_at AS "left_at?: DateTime<Utc>"
           FROM chats c
           WHERE c.dm_user_id = ?1
              OR (c.kind = 'group'
                  AND c.id IN (SELECT chat_id FROM chat_members WHERE user_id = ?1))
           ORDER BY c.kind = 'group', c.name, c.id"#,
        user_id
    )
    .fetch_all(conn)
    .await
}

/// A group chat.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub id: i64,
    pub name: Option<String>,
}

/// The group with this id. `None` if there's no such chat **or it's a
/// DM**, so a DM's lists can't be fetched through the dashboard.
pub async fn group(
    conn: &mut SqliteConnection,
    chat_id: i64,
) -> Result<Option<Group>, sqlx::Error> {
    sqlx::query_as!(
        Group,
        r#"SELECT id AS "id!", name FROM chats WHERE id = ? AND kind = 'group'"#,
        chat_id
    )
    .fetch_optional(conn)
    .await
}

/// A group, with how much is on its lists and how much the bot is used
/// there.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupSummary {
    pub id: i64,
    /// `None` if GOWA hasn't told us the group's name yet.
    pub name: Option<String>,
    /// People the bot has seen in the group.
    pub members: i64,
    pub open_todos: i64,
    pub done_todos: i64,
    pub to_buy: i64,
    pub bought: i64,
    /// Uses of the bot in the group since the `since` passed in.
    pub uses_since: i64,
    /// When someone last used the bot in the group.
    pub last_active: DateTime<Utc>,
    /// When the bot warned the group it would leave.
    pub warned_at: Option<DateTime<Utc>>,
    /// When the bot was found to be no longer in the group.
    pub left_at: Option<DateTime<Utc>>,
}

/// Every group (or just group `only`), most recently active first. Never a
/// DM: `kind = 'group'`.
pub async fn groups(
    conn: &mut SqliteConnection,
    only: Option<i64>,
    since: DateTime<Utc>,
) -> Result<Vec<GroupSummary>, sqlx::Error> {
    let mut rows = sqlx::query_as!(
        GroupSummary,
        r#"SELECT c.id AS "id!", c.name,
                  (SELECT count(*) FROM chat_members m WHERE m.chat_id = c.id) AS "members!: i64",
                  (SELECT count(*) FROM todos t
                   WHERE t.chat_id = c.id AND t.status = 'pending') AS "open_todos!: i64",
                  (SELECT count(*) FROM todos t
                   WHERE t.chat_id = c.id AND t.status = 'completed') AS "done_todos!: i64",
                  (SELECT count(*) FROM shopping_items s
                   WHERE s.chat_id = c.id AND s.bought_at IS NULL) AS "to_buy!: i64",
                  (SELECT count(*) FROM shopping_items s
                   WHERE s.chat_id = c.id AND s.bought_at IS NOT NULL) AS "bought!: i64",
                  (SELECT count(*) FROM command_log l
                   WHERE l.chat_id = c.id AND datetime(l.at) >= datetime(?2)) AS "uses_since!: i64",
                  coalesce(c.last_active_at, c.created_at) AS "last_active!: DateTime<Utc>",
                  c.warned_at AS "warned_at?: DateTime<Utc>",
                  c.left_at AS "left_at?: DateTime<Utc>"
           FROM chats c
           WHERE c.kind = 'group' AND (?1 IS NULL OR c.id = ?1)
           ORDER BY c.name, c.id"#,
        only,
        since
    )
    .fetch_all(conn)
    .await?;

    rows.sort_by_key(|g| Reverse(g.last_active));
    Ok(rows)
}

/// Someone in a group.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub id: i64,
    pub name: String,
}

/// The people the bot has seen in a chat, A to Z.
pub async fn members_of(
    conn: &mut SqliteConnection,
    chat_id: i64,
) -> Result<Vec<Member>, sqlx::Error> {
    sqlx::query_as!(
        Member,
        r#"SELECT u.id AS "id!", u.display_name AS name
           FROM chat_members m JOIN users u ON u.id = m.user_id
           WHERE m.chat_id = ?
           ORDER BY u.display_name"#,
        chat_id
    )
    .fetch_all(conn)
    .await
}

/// A person's phone-number ID (`wa_jid`), which is also their DM's chat
/// ID. `None` if there's no such user.
pub async fn user_jid(
    conn: &mut SqliteConnection,
    user_id: i64,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar!("SELECT wa_jid FROM users WHERE id = ?", user_id)
        .fetch_optional(conn)
        .await
}

/// Whether there's a chat (a group or a DM) with this id.
pub async fn chat_exists(conn: &mut SqliteConnection, chat_id: i64) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM chats WHERE id = ?) AS "exists!: bool""#,
        chat_id
    )
    .fetch_one(conn)
    .await
}

/// The id of the person's DM with the bot, if they have one.
pub async fn dm_of(conn: &mut SqliteConnection, user_id: i64) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT id AS "id!" FROM chats WHERE dm_user_id = ?"#,
        user_id
    )
    .fetch_optional(conn)
    .await
}

/// Whether the chat is the person's DM or a group they've been seen in:
/// the chats whose todo reminders the dashboard may change for them.
pub async fn is_in_chat(
    conn: &mut SqliteConnection,
    user_id: i64,
    chat_id: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS(
               SELECT 1 FROM chats c
               WHERE c.id = ?2
                 AND (c.dm_user_id = ?1
                      OR (c.kind = 'group' AND EXISTS(
                          SELECT 1 FROM chat_members m
                          WHERE m.chat_id = c.id AND m.user_id = ?1)))
           ) AS "in_chat!: bool""#,
        user_id,
        chat_id
    )
    .fetch_one(conn)
    .await
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;
    use crate::db::test_pool;
    use crate::metrics::{Event, Kind};
    use crate::repo::metrics::{Logged, record};
    use crate::repo::{chats, shop, todo_reminders, todos, users};
    use crate::testkit::person;

    const GROUP: &str = "120363000000000001@g.us";

    async fn log_use(conn: &mut SqliteConnection, chat_id: i64, user_id: i64, at: DateTime<Utc>) {
        let event = Event {
            kind: Kind::Command,
            name: "todo.add".to_string(),
            detail: None,
            typed: None,
        };
        let row = Logged {
            event: &event,
            chat_id,
            user_id,
            is_group: true,
            ok: true,
            duration_ms: 1,
            at,
        };
        record(conn, &row).await.unwrap();
    }

    #[tokio::test]
    async fn users_with_activity_most_recent_first() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let bilal = users::find_or_register(&mut conn, &person("Bilal", "447700900001"), now)
            .await
            .unwrap()
            .unwrap();
        let khadija = users::find_or_register(&mut conn, &person("Khadija", "447700900002"), now)
            .await
            .unwrap()
            .unwrap();
        let zara = users::find_or_register(&mut conn, &person("Zara", "447700900003"), now)
            .await
            .unwrap()
            .unwrap();

        let group = chats::find_or_create(&mut conn, GROUP, None, now)
            .await
            .unwrap();
        chats::add_member(&mut conn, group, bilal.id, now)
            .await
            .unwrap();
        chats::add_member(&mut conn, group, khadija.id, now)
            .await
            .unwrap();
        chats::find_or_create(&mut conn, &bilal.wa_jid, Some(bilal.id), now)
            .await
            .unwrap();

        // Bilal: once 40 days ago, once yesterday. Khadija: an hour ago.
        log_use(&mut conn, group, bilal.id, now - Duration::days(40)).await;
        log_use(&mut conn, group, bilal.id, now - Duration::days(1)).await;
        log_use(&mut conn, group, khadija.id, now - Duration::hours(1)).await;

        let all = users(&mut conn, None, now - Duration::days(30))
            .await
            .unwrap();
        let names: Vec<&str> = all.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, ["Khadija", "Bilal", "Zara"]); // Zara: never used it

        let b = &all[1];
        assert_eq!(b.uses_since, 1, "the use 40 days ago is too old");
        assert_eq!(b.groups, 1);
        assert!(b.has_dm);
        assert_eq!(
            b.last_seen.map(|t| t.date_naive()),
            Some((now - Duration::days(1)).date_naive())
        );
        assert_eq!(all[2].last_seen, None);
        assert!(!all[0].has_dm);

        let only = users(&mut conn, Some(zara.id), now).await.unwrap();
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].name, "Zara");
        assert!(users(&mut conn, Some(999), now).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn chats_of_counts_lists_dm_first() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let bilal = users::find_or_register(&mut conn, &person("Bilal", "447700900001"), now)
            .await
            .unwrap()
            .unwrap();
        let khadija = users::find_or_register(&mut conn, &person("Khadija", "447700900002"), now)
            .await
            .unwrap()
            .unwrap();
        let group = chats::find_or_create(&mut conn, GROUP, None, now)
            .await
            .unwrap();
        chats::add_member(&mut conn, group, bilal.id, now)
            .await
            .unwrap();
        chats::set_group_name(&mut conn, GROUP, "Family")
            .await
            .unwrap();
        let dm = chats::find_or_create(&mut conn, &bilal.wa_jid, Some(bilal.id), now)
            .await
            .unwrap();
        chats::add_member(&mut conn, dm, bilal.id, now)
            .await
            .unwrap();
        // Khadija's DM must never show up under Bilal.
        chats::find_or_create(&mut conn, &khadija.wa_jid, Some(khadija.id), now)
            .await
            .unwrap();

        let tap = todos::add(
            &mut conn,
            group,
            "fix the tap",
            bilal.id,
            None,
            Default::default(),
            now,
        )
        .await
        .unwrap();
        todos::add(
            &mut conn,
            group,
            "bins",
            bilal.id,
            None,
            Default::default(),
            now,
        )
        .await
        .unwrap();
        todos::complete(&mut conn, tap, group, bilal.id, now)
            .await
            .unwrap();
        let milk = shop::add(&mut conn, dm, "milk", bilal.id, now)
            .await
            .unwrap();
        shop::add(&mut conn, dm, "eggs", bilal.id, now)
            .await
            .unwrap();
        shop::add(&mut conn, dm, "bread", bilal.id, now)
            .await
            .unwrap();
        shop::mark_bought(&mut conn, milk, dm, bilal.id, now)
            .await
            .unwrap();
        todo_reminders::enable(&mut conn, group, bilal.id, now)
            .await
            .unwrap();

        let found = chats_of(&mut conn, bilal.id).await.unwrap();
        // Activity is checked elsewhere; here, the counts.
        let found: Vec<UserChat> = found
            .into_iter()
            .map(|c| UserChat {
                last_active: now,
                warned_at: None,
                left_at: None,
                ..c
            })
            .collect();
        assert_eq!(
            found,
            [
                UserChat {
                    id: dm,
                    kind: "dm".to_string(),
                    name: None,
                    open_todos: 0,
                    done_todos: 0,
                    to_buy: 2,
                    bought: 1,
                    todo_reminders: false,
                    last_active: now,
                    warned_at: None,
                    left_at: None,
                },
                UserChat {
                    id: group,
                    kind: "group".to_string(),
                    name: Some("Family".to_string()),
                    open_todos: 1,
                    done_todos: 1,
                    to_buy: 0,
                    bought: 0,
                    todo_reminders: true,
                    last_active: now,
                    warned_at: None,
                    left_at: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn groups_with_counts_and_members() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let bilal = users::find_or_register(&mut conn, &person("Bilal", "447700900001"), now)
            .await
            .unwrap()
            .unwrap();
        let khadija = users::find_or_register(&mut conn, &person("Khadija", "447700900002"), now)
            .await
            .unwrap()
            .unwrap();
        let family = chats::find_or_create(&mut conn, GROUP, None, now)
            .await
            .unwrap();
        chats::set_group_name(&mut conn, GROUP, "Family")
            .await
            .unwrap();
        let quiet = chats::find_or_create(
            &mut conn,
            "120363000000000002@g.us",
            None,
            now - Duration::days(5),
        )
        .await
        .unwrap();
        for user in [khadija.id, bilal.id] {
            chats::add_member(&mut conn, family, user, now)
                .await
                .unwrap();
        }
        let dm = chats::find_or_create(&mut conn, &bilal.wa_jid, Some(bilal.id), now)
            .await
            .unwrap();
        todos::add(
            &mut conn,
            family,
            "bins",
            bilal.id,
            None,
            Default::default(),
            now,
        )
        .await
        .unwrap();
        shop::add(&mut conn, family, "milk", bilal.id, now)
            .await
            .unwrap();
        log_use(&mut conn, family, bilal.id, now - Duration::days(40)).await;
        log_use(&mut conn, family, bilal.id, now - Duration::hours(1)).await;
        chats::touch(&mut conn, family, now).await.unwrap();

        let all = groups(&mut conn, None, now - Duration::days(30))
            .await
            .unwrap();
        // Most recently active first; the DM isn't a group.
        let ids: Vec<i64> = all.iter().map(|g| g.id).collect();
        assert_eq!(ids, [family, quiet]);
        let f = &all[0];
        assert_eq!(f.name.as_deref(), Some("Family"));
        assert_eq!(f.members, 2);
        assert_eq!((f.open_todos, f.to_buy), (1, 1));
        assert_eq!(f.uses_since, 1, "the use 40 days ago is too old");
        assert_eq!(all[1].members, 0);

        assert_eq!(
            groups(&mut conn, Some(quiet), now).await.unwrap()[0].id,
            quiet
        );
        assert!(groups(&mut conn, Some(dm), now).await.unwrap().is_empty());

        let members = members_of(&mut conn, family).await.unwrap();
        let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["Bilal", "Khadija"]);
    }

    #[tokio::test]
    async fn group_never_finds_a_dm() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let bilal = users::find_or_register(&mut conn, &person("Bilal", "447700900001"), now)
            .await
            .unwrap()
            .unwrap();
        let group_id = chats::find_or_create(&mut conn, GROUP, None, now)
            .await
            .unwrap();
        let dm = chats::find_or_create(&mut conn, &bilal.wa_jid, Some(bilal.id), now)
            .await
            .unwrap();

        assert_eq!(
            group(&mut conn, group_id).await.unwrap(),
            Some(Group {
                id: group_id,
                name: None
            })
        );
        assert_eq!(group(&mut conn, dm).await.unwrap(), None);
        assert_eq!(group(&mut conn, 999).await.unwrap(), None);
    }

    #[tokio::test]
    async fn who_is_in_which_chat() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let bilal = users::find_or_register(&mut conn, &person("Bilal", "447700900001"), now)
            .await
            .unwrap()
            .unwrap();
        let khadija = users::find_or_register(&mut conn, &person("Khadija", "447700900002"), now)
            .await
            .unwrap()
            .unwrap();
        let group = chats::find_or_create(&mut conn, GROUP, None, now)
            .await
            .unwrap();
        chats::add_member(&mut conn, group, bilal.id, now)
            .await
            .unwrap();
        let dm = chats::find_or_create(&mut conn, &bilal.wa_jid, Some(bilal.id), now)
            .await
            .unwrap();

        assert_eq!(
            user_jid(&mut conn, bilal.id).await.unwrap().as_deref(),
            Some("447700900001@s.whatsapp.net")
        );
        assert_eq!(user_jid(&mut conn, 999).await.unwrap(), None);
        assert_eq!(dm_of(&mut conn, bilal.id).await.unwrap(), Some(dm));
        assert_eq!(dm_of(&mut conn, khadija.id).await.unwrap(), None);

        assert!(is_in_chat(&mut conn, bilal.id, group).await.unwrap());
        assert!(is_in_chat(&mut conn, bilal.id, dm).await.unwrap());
        // Not her group, and certainly not his DM.
        assert!(!is_in_chat(&mut conn, khadija.id, group).await.unwrap());
        assert!(!is_in_chat(&mut conn, khadija.id, dm).await.unwrap());
        assert!(!is_in_chat(&mut conn, bilal.id, 999).await.unwrap());
    }
}

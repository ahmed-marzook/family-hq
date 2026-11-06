//! Opening the SQLite database and bringing its schema up to date.
//!
//! The rest of the program gets a `SqlitePool` from `connect` and can
//! assume every table in `migrations/` exists.

use std::str::FromStr;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// Opens (creating if needed) the database at `url`, e.g.
/// `sqlite:family-hq.db`, and runs any migrations it hasn't had yet.
///
/// A *pool* is a set of open connections that handlers borrow and give
/// back. Opening a connection is slow; borrowing one is fast. `SqlitePool`
/// is an `Arc` inside, so cloning it shares the same pool.
pub async fn connect(url: &str) -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::from_str(url)?
        .create_if_missing(true)
        // WAL ("write-ahead log") lets readers carry on while someone
        // writes, instead of everyone waiting on one lock.
        .journal_mode(SqliteJournalMode::Wal)
        // SQLx turns this on by default; said here so it's clear we rely
        // on it. Without it SQLite ignores every REFERENCES clause.
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new().connect_with(options).await?;
    migrate(&pool).await?;
    Ok(pool)
}

/// Runs the migrations in `migrations/` that this database hasn't had.
///
/// `sqlx::migrate!` reads the folder at *compile time* and builds the SQL
/// into the binary, so the Docker image needs no migration files. SQLx
/// records what has run in its own `_sqlx_migrations` table, and refuses
/// to start if an already-run file has been edited (its checksum changed).
pub async fn migrate(pool: &SqlitePool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!().run(pool).await
}

/// A fresh, fully migrated database in memory, for tests anywhere in the
/// crate. `#[cfg(test)]` leaves it out of the real binary; `pub(crate)`
/// means "visible inside this crate only".
///
/// One connection only: every connection to `sqlite::memory:` gets its
/// *own* empty database, so a pool of several would lose tables between
/// queries.
#[cfg(test)]
pub(crate) async fn test_pool() -> SqlitePool {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    migrate(&pool).await.unwrap();
    pool
}

/// A database in a real file, opened by `connect` exactly as the app opens
/// its own (WAL, a pool of several connections). For tests about several
/// requests at once, which `test_pool`'s single in-memory connection can't
/// show: there everyone simply queues for the one connection.
#[cfg(test)]
pub(crate) struct TestFile {
    pub(crate) pool: SqlitePool,
    path: std::path::PathBuf,
}

/// Creates a `TestFile` in the system's temp folder. `name` and the time
/// make the file name unique, so tests running side by side never share
/// one. Call `remove` at the end of the test.
#[cfg(test)]
pub(crate) async fn test_file(name: &str) -> TestFile {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "family-hq-test-{name}-{}-{nanos}.db",
        std::process::id()
    ));
    let pool = connect(&format!("sqlite:{}", path.display()))
        .await
        .unwrap();
    TestFile { pool, path }
}

#[cfg(test)]
impl TestFile {
    /// Closes every connection, then deletes the file and the two that WAL
    /// keeps next to it (`-wal`, `-shm`). Taking `self` by value means the
    /// pool can't be used after this. If a test panics before getting
    /// here, the files are left in the temp folder; that's harmless.
    pub(crate) async fn remove(self) {
        self.pool.close().await;
        for suffix in ["", "-wal", "-shm"] {
            let mut name = self.path.clone().into_os_string();
            name.push(suffix);
            // Already gone is fine, so the result is ignored.
            let _ = std::fs::remove_file(name);
        }
    }
}

#[cfg(test)]
mod tests {
    //! Tests for the schema's constraints: each one tries a good row and
    //! some bad ones, and checks SQLite accepts or rejects them.
    //!
    //! Values go in through `?` placeholders and `.bind(...)`, never
    //! `format!`. SQLx 0.9 enforces this: `sqlx::query` only takes a
    //! `&'static str` (SQL written in the source), so SQL built at run time
    //! doesn't compile without an explicit `AssertSqlSafe`.

    use super::*;

    const NOW: &str = "2026-10-27T18:00:00Z";
    const LATER: &str = "2026-10-28T09:00:00Z";

    // IDs of the rows `seeded_db` creates.
    const BILAL: i64 = 1;
    const KHADIJA: i64 = 2;
    const FAMILY_GROUP: i64 = 1;

    async fn test_db() -> SqlitePool {
        test_pool().await
    }

    /// Bilal and Khadija, a family group with both in it, and Bilal's DM.
    async fn seeded_db() -> SqlitePool {
        let pool = test_db().await;
        add_user(
            &pool,
            "Bilal",
            "447700900001@s.whatsapp.net",
            Some("111@lid"),
        )
        .await
        .unwrap();
        add_user(&pool, "Khadija", "447700900002@s.whatsapp.net", None)
            .await
            .unwrap();
        add_chat(&pool, "120363000000000001@g.us", "group", None)
            .await
            .unwrap();
        add_chat(&pool, "447700900001@s.whatsapp.net", "dm", Some(BILAL))
            .await
            .unwrap();
        for user in [BILAL, KHADIJA] {
            add_member(&pool, FAMILY_GROUP, user).await.unwrap();
        }
        pool
    }

    // ---- insert helpers ----
    //
    // Each returns the `Result` so tests can assert success or failure.
    // `Option<T>` binds as NULL when it's `None`.

    async fn add_user(
        pool: &SqlitePool,
        name: &str,
        jid: &str,
        lid: Option<&str>,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO users (display_name, wa_jid, wa_lid, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(name)
        .bind(jid)
        .bind(lid)
        .bind(NOW)
        .execute(pool)
        .await
        .map(|_| ())
    }

    async fn add_chat(
        pool: &SqlitePool,
        wa_chat_id: &str,
        kind: &str,
        dm_user_id: Option<i64>,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO chats (wa_chat_id, kind, dm_user_id, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(wa_chat_id)
        .bind(kind)
        .bind(dm_user_id)
        .bind(NOW)
        .execute(pool)
        .await
        .map(|_| ())
    }

    async fn add_member(pool: &SqlitePool, chat_id: i64, user_id: i64) -> sqlx::Result<()> {
        sqlx::query("INSERT INTO chat_members (chat_id, user_id, joined_at) VALUES (?, ?, ?)")
            .bind(chat_id)
            .bind(user_id)
            .bind(NOW)
            .execute(pool)
            .await
            .map(|_| ())
    }

    async fn add_shop_item(
        pool: &SqlitePool,
        chat_id: i64,
        name: &str,
        bought: Option<(i64, &str)>,
    ) -> sqlx::Result<()> {
        // `Option<(a, b)>` → two separate `Option`s, one per column.
        let (bought_by, bought_at) = bought.unzip();
        sqlx::query(
            "INSERT INTO shopping_items (chat_id, name, added_by, bought_by, bought_at, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(chat_id)
        .bind(name)
        .bind(BILAL)
        .bind(bought_by)
        .bind(bought_at)
        .bind(NOW)
        .execute(pool)
        .await
        .map(|_| ())
    }

    async fn add_todo(
        pool: &SqlitePool,
        assignee: Option<i64>,
        status: &str,
        closed_by: Option<i64>,
        closed_at: Option<&str>,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO todos (chat_id, title, created_by, assignee_id, status,
                                closed_by, closed_at, created_at)
             VALUES (?, 'fix the tap', ?, ?, ?, ?, ?, ?)",
        )
        .bind(FAMILY_GROUP)
        .bind(BILAL)
        .bind(assignee)
        .bind(status)
        .bind(closed_by)
        .bind(closed_at)
        .bind(NOW)
        .execute(pool)
        .await
        .map(|_| ())
    }

    async fn add_list_position(pool: &SqlitePool, position: i64) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO wa_message_context
                (wa_message_id, chat_id, position, item_type, item_id, created_at)
             VALUES ('3EB0AA', ?, ?, 'shop', 1, ?)",
        )
        .bind(FAMILY_GROUP)
        .bind(position)
        .bind(NOW)
        .execute(pool)
        .await
        .map(|_| ())
    }

    // ---- tests ----

    #[tokio::test]
    async fn migrations_create_every_table() {
        let pool = test_db().await;
        // `query_scalar` reads the first column of each row into a plain
        // value, here a `String`.
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master
             WHERE type = 'table'
               AND name NOT IN ('_sqlx_migrations', 'sqlite_sequence')
             ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            names,
            [
                "chat_members",
                "chat_settings",
                "chats",
                "command_log",
                "points",
                "prayer_log",
                "prayer_reminders",
                "processed_messages",
                "push_subscriptions",
                "pushes_sent",
                "report_settings",
                "reports_sent",
                "shopping_items",
                "todo_reminders",
                "todos",
                "users",
                "wa_message_context",
            ]
        );
    }

    #[tokio::test]
    async fn migrating_twice_is_harmless() {
        let pool = test_db().await;
        migrate(&pool).await.unwrap();
    }

    #[tokio::test]
    async fn display_names_are_unique_ignoring_case() {
        let pool = seeded_db().await;
        let result = add_user(&pool, "KHADIJA", "447700900009@s.whatsapp.net", None).await;
        assert!(result.is_err());

        // ...and lookups ignore case too, which `@khadija` will rely on.
        let id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE display_name = ?")
            .bind("khadija")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(id, KHADIJA);
    }

    #[tokio::test]
    async fn user_ids_must_have_the_right_shape() {
        let pool = test_db().await;
        let phone = "447700900001@s.whatsapp.net";
        assert!(
            add_user(&pool, "A", "447700900001@g.us", None)
                .await
                .is_err()
        );
        assert!(
            add_user(&pool, "A", phone, Some(phone)).await.is_err(),
            "lid isn't a LID"
        );
        assert!(
            add_user(&pool, "  ", phone, None).await.is_err(),
            "blank name"
        );
        assert!(add_user(&pool, "A", phone, Some("111@lid")).await.is_ok());
    }

    #[tokio::test]
    async fn chat_kind_must_match_its_id_and_owner() {
        let pool = seeded_db().await;
        let group = "120363000000000002@g.us";
        let khadija_dm = "447700900002@s.whatsapp.net";

        assert!(
            add_chat(&pool, group, "group", Some(KHADIJA))
                .await
                .is_err(),
            "group with owner"
        );
        assert!(
            add_chat(&pool, khadija_dm, "dm", None).await.is_err(),
            "DM without owner"
        );
        assert!(
            add_chat(&pool, khadija_dm, "group", None).await.is_err(),
            "group without @g.us"
        );
        assert!(
            add_chat(&pool, group, "dm", Some(KHADIJA)).await.is_err(),
            "DM with @g.us"
        );
        assert!(
            add_chat(&pool, "447700900003@s.whatsapp.net", "dm", Some(BILAL))
                .await
                .is_err(),
            "Bilal already has a DM"
        );

        // A second group and Khadija's DM are fine: multiple groups work.
        assert!(add_chat(&pool, group, "group", None).await.is_ok());
        assert!(
            add_chat(&pool, khadija_dm, "dm", Some(KHADIJA))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn a_user_is_in_a_chat_at_most_once() {
        let pool = seeded_db().await;
        assert!(add_member(&pool, FAMILY_GROUP, BILAL).await.is_err());
    }

    #[tokio::test]
    async fn shopping_items_belong_to_an_existing_chat() {
        let pool = seeded_db().await;
        assert!(
            add_shop_item(&pool, FAMILY_GROUP, "milk", None)
                .await
                .is_ok()
        );
        assert!(
            add_shop_item(&pool, 99, "milk", None).await.is_err(),
            "no chat 99"
        );
        assert!(
            add_shop_item(&pool, FAMILY_GROUP, "   ", None)
                .await
                .is_err(),
            "blank name"
        );
    }

    #[tokio::test]
    async fn bought_needs_both_who_and_when() {
        let pool = seeded_db().await;
        assert!(
            add_shop_item(&pool, FAMILY_GROUP, "milk", Some((KHADIJA, LATER)))
                .await
                .is_ok()
        );

        // Half-bought rows can't be built with the helper, so write them out.
        let half_bought = sqlx::query(
            "INSERT INTO shopping_items (chat_id, name, added_by, bought_by, bought_at, created_at)
             VALUES (?, 'milk', ?, ?, ?, ?)",
        );
        let only_who = half_bought.bind(FAMILY_GROUP).bind(BILAL).bind(KHADIJA);
        let only_who = only_who.bind(None::<&str>).bind(NOW);
        assert!(only_who.execute(&pool).await.is_err());
    }

    #[tokio::test]
    async fn todo_status_matches_closed_fields() {
        let pool = seeded_db().await;
        let who = Some(KHADIJA);
        let when = Some(LATER);

        assert!(add_todo(&pool, who, "pending", None, None).await.is_ok());
        assert!(add_todo(&pool, who, "completed", who, when).await.is_ok());
        assert!(add_todo(&pool, who, "skipped", who, when).await.is_ok());

        assert!(add_todo(&pool, who, "completed", None, None).await.is_err());
        assert!(add_todo(&pool, who, "pending", who, when).await.is_err());
        assert!(add_todo(&pool, who, "completed", None, when).await.is_err());
        assert!(add_todo(&pool, who, "skipped", None, None).await.is_err());
        assert!(add_todo(&pool, who, "abandoned", who, when).await.is_err());
    }

    #[tokio::test]
    async fn todo_due_date_and_priority_are_checked() {
        let pool = seeded_db().await;
        let insert = |due: Option<&'static str>, priority: Option<&'static str>| {
            sqlx::query(
                "INSERT INTO todos (chat_id, title, created_by, due_date, priority, created_at)
                 VALUES (?, 'bins', ?, ?, ?, ?)",
            )
            .bind(FAMILY_GROUP)
            .bind(BILAL)
            .bind(due)
            .bind(priority)
            .bind(NOW)
            .execute(&pool)
        };

        assert!(insert(None, None).await.is_ok());
        assert!(insert(Some("2026-11-13"), Some("high")).await.is_ok());
        assert!(insert(Some("2028-02-29"), Some("low")).await.is_ok());

        assert!(
            insert(Some("2026-02-30"), None).await.is_err(),
            "no such day"
        );
        assert!(insert(Some("13/11"), None).await.is_err());
        assert!(
            insert(Some("2026-11-13T10:00:00Z"), None).await.is_err(),
            "a time"
        );
        assert!(insert(None, Some("urgent")).await.is_err());
        assert!(insert(None, Some("High")).await.is_err(), "lowercase only");
    }

    #[tokio::test]
    async fn todo_assignee_must_be_a_real_user() {
        let pool = seeded_db().await;
        assert!(
            add_todo(&pool, Some(99), "pending", None, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn strict_tables_reject_wrong_types() {
        let pool = seeded_db().await;
        // Without STRICT, SQLite would happily store 'one' as a chat_id.
        let result = sqlx::query(
            "INSERT INTO todos (chat_id, title, created_by, created_at) VALUES (?, 'x', ?, ?)",
        )
        .bind("one")
        .bind(BILAL)
        .bind(NOW)
        .execute(&pool)
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn a_message_id_can_only_be_processed_once() {
        let pool = test_db().await;
        let insert = || {
            sqlx::query("INSERT INTO processed_messages (wa_message_id, received_at) VALUES (?, ?)")
                .bind("AC123")
                .bind(NOW)
                .execute(&pool)
        };
        assert!(insert().await.is_ok());
        assert!(insert().await.is_err(), "second insert is the duplicate");
    }

    #[tokio::test]
    async fn list_positions_start_at_one_and_are_unique_per_message() {
        let pool = seeded_db().await;
        assert!(add_list_position(&pool, 1).await.is_ok());
        assert!(
            add_list_position(&pool, 1).await.is_err(),
            "position 1 twice"
        );
        assert!(add_list_position(&pool, 0).await.is_err(), "position 0");
    }
}

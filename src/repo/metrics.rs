//! The usage log (`command_log`). Read it with the queries in
//! docs/METRICS.md.

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;

use crate::metrics::Event;

/// One row of the log: the event, plus where, who, when, and how it went.
pub struct Logged<'a> {
    pub event: &'a Event,
    pub chat_id: i64,
    pub user_id: i64,
    pub is_group: bool,
    pub ok: bool,
    pub duration_ms: i64,
    pub at: DateTime<Utc>,
}

pub async fn record(conn: &mut SqliteConnection, row: &Logged<'_>) -> Result<(), sqlx::Error> {
    let kind = row.event.kind.as_str();
    sqlx::query!(
        "INSERT INTO command_log
            (at, chat_id, user_id, kind, name, detail, typed, is_group, ok, duration_ms)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        row.at,
        row.chat_id,
        row.user_id,
        kind,
        row.event.name,
        row.event.detail,
        row.event.typed,
        row.is_group,
        row.ok,
        row.duration_ms
    )
    .execute(conn)
    .await?;
    Ok(())
}

// ---- Reading the log, for the admin dashboard ----
//
// Every query takes a `Filter`: only rows from `since` on (`None` for all
// time), and only one person's or one chat's if asked. `?1 IS NULL OR ...`
// makes each of those one query instead of two.
// `datetime(...)` puts both times in one text form before comparing, as
// docs/METRICS.md does. A "use" is a command, a list reply or a reaction:
// `unknown` rows (a `+word` the bot didn't understand) are counted apart.

/// Which rows of the log to read. `Copy`: it's a few small values, so it
/// can be passed to every query without `.clone()`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Filter {
    pub since: Option<DateTime<Utc>>,
    /// Only this person's uses.
    pub user: Option<i64>,
    /// Only uses in this chat.
    pub chat: Option<i64>,
}

/// When each use happened and who by, oldest first, for grouping by day
/// and hour on the dashboard's clock (`metrics::per_day`, `by_hour`).
pub async fn use_times(
    conn: &mut SqliteConnection,
    filter: Filter,
) -> Result<Vec<(DateTime<Utc>, i64)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT at AS "at: DateTime<Utc>", user_id
           FROM command_log
           WHERE kind != 'unknown' AND (?1 IS NULL OR datetime(at) >= datetime(?1))
             AND (?2 IS NULL OR user_id = ?2) AND (?3 IS NULL OR chat_id = ?3)
           ORDER BY at"#,
        filter.since,
        filter.user,
        filter.chat
    )
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|r| (r.at, r.user_id)).collect())
}

/// One command and how often it was used.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandUses {
    pub name: String,
    pub uses: i64,
}

/// Most used commands first.
pub async fn top_commands(
    conn: &mut SqliteConnection,
    filter: Filter,
) -> Result<Vec<CommandUses>, sqlx::Error> {
    sqlx::query_as!(
        CommandUses,
        r#"SELECT name, count(*) AS "uses!: i64"
           FROM command_log
           WHERE kind != 'unknown' AND (?1 IS NULL OR datetime(at) >= datetime(?1))
             AND (?2 IS NULL OR user_id = ?2) AND (?3 IS NULL OR chat_id = ?3)
           GROUP BY name
           ORDER BY count(*) DESC, name"#,
        filter.since,
        filter.user,
        filter.chat
    )
    .fetch_all(conn)
    .await
}

/// Someone, with when they last used the bot (ever, not just since).
#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    pub id: i64,
    pub name: String,
    pub last_seen: Option<DateTime<Utc>>,
}

/// The people the filter covers, A to Z: everyone (even people who never
/// used a command) with no filter; with a chat, the people who ever used the
/// bot there, and `last_seen` is their latest use there. `since` is ignored.
pub async fn people(
    conn: &mut SqliteConnection,
    filter: Filter,
) -> Result<Vec<Person>, sqlx::Error> {
    sqlx::query_as!(
        Person,
        r#"SELECT u.id AS "id!", u.display_name AS name,
                  (SELECT max(l.at) FROM command_log l
                   WHERE l.user_id = u.id AND l.kind != 'unknown'
                     AND (?2 IS NULL OR l.chat_id = ?2)) AS "last_seen?: DateTime<Utc>"
           FROM users u
           WHERE (?1 IS NULL OR u.id = ?1)
             AND (?2 IS NULL OR EXISTS(SELECT 1 FROM command_log l
                                       WHERE l.user_id = u.id AND l.chat_id = ?2))
           ORDER BY u.display_name"#,
        filter.user,
        filter.chat
    )
    .fetch_all(conn)
    .await
}

/// A chat, with how often the bot was used in it.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatUses {
    pub id: i64,
    /// `"group"` or `"dm"`.
    pub kind: String,
    /// The group's name, or for a DM, its owner's name.
    pub name: Option<String>,
    pub uses: i64,
}

/// Where the bot was used, most uses first. Uses only: never what was
/// said, so a DM appears here as a count, like on the users pages.
pub async fn chats(
    conn: &mut SqliteConnection,
    filter: Filter,
) -> Result<Vec<ChatUses>, sqlx::Error> {
    sqlx::query_as!(
        ChatUses,
        r#"SELECT c.id AS "id!", c.kind,
                  CASE WHEN c.kind = 'dm' THEN u.display_name ELSE c.name END AS "name?: String",
                  count(*) AS "uses!: i64"
           FROM command_log l
           JOIN chats c ON c.id = l.chat_id
           LEFT JOIN users u ON u.id = c.dm_user_id
           WHERE l.kind != 'unknown' AND (?1 IS NULL OR datetime(l.at) >= datetime(?1))
             AND (?2 IS NULL OR l.user_id = ?2) AND (?3 IS NULL OR l.chat_id = ?3)
           GROUP BY c.id
           ORDER BY count(*) DESC, c.id"#,
        filter.since,
        filter.user,
        filter.chat
    )
    .fetch_all(conn)
    .await
}

/// A `+word` or `/word` the bot didn't understand.
#[derive(Debug, Clone, PartialEq)]
pub struct Unknown {
    /// The word as typed, e.g. `+remind`.
    pub typed: String,
    pub times: i64,
    pub people: i64,
}

/// Unknown words, most typed first: the best ideas for new commands.
pub async fn unknown_words(
    conn: &mut SqliteConnection,
    filter: Filter,
) -> Result<Vec<Unknown>, sqlx::Error> {
    sqlx::query_as!(
        Unknown,
        r#"SELECT name AS typed, count(*) AS "times!: i64",
                  count(DISTINCT user_id) AS "people!: i64"
           FROM command_log
           WHERE kind = 'unknown' AND (?1 IS NULL OR datetime(at) >= datetime(?1))
             AND (?2 IS NULL OR user_id = ?2) AND (?3 IS NULL OR chat_id = ?3)
           GROUP BY name
           ORDER BY count(*) DESC, name"#,
        filter.since,
        filter.user,
        filter.chat
    )
    .fetch_all(conn)
    .await
}

/// A use that failed (the error itself is in the server log at `at`).
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub at: DateTime<Utc>,
    pub person: String,
    pub name: String,
    pub detail: Option<String>,
}

/// How many uses failed, and the latest `limit` of them, newest first.
pub async fn failures(
    conn: &mut SqliteConnection,
    filter: Filter,
    limit: i64,
) -> Result<(i64, Vec<Failure>), sqlx::Error> {
    let total = sqlx::query_scalar!(
        r#"SELECT count(*) AS "total!: i64" FROM command_log
           WHERE ok = 0 AND kind != 'unknown' AND (?1 IS NULL OR datetime(at) >= datetime(?1))
             AND (?2 IS NULL OR user_id = ?2) AND (?3 IS NULL OR chat_id = ?3)"#,
        filter.since,
        filter.user,
        filter.chat
    )
    .fetch_one(&mut *conn)
    .await?;
    let latest = sqlx::query_as!(
        Failure,
        r#"SELECT l.at AS "at: DateTime<Utc>", u.display_name AS person, l.name, l.detail
           FROM command_log l JOIN users u ON u.id = l.user_id
           WHERE l.ok = 0 AND l.kind != 'unknown'
             AND (?1 IS NULL OR datetime(l.at) >= datetime(?1))
             AND (?2 IS NULL OR l.user_id = ?2) AND (?3 IS NULL OR l.chat_id = ?3)
           ORDER BY l.at DESC
           LIMIT ?4"#,
        filter.since,
        filter.user,
        filter.chat,
        limit
    )
    .fetch_all(conn)
    .await?;
    Ok((total, latest))
}

/// How long a command takes to answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Speed {
    pub name: String,
    pub uses: i64,
    pub avg_ms: i64,
    pub max_ms: i64,
}

/// The slowest commands on average, slowest first (usually ones that call
/// an outside API: AlAdhan, Open-Meteo).
pub async fn slowest(
    conn: &mut SqliteConnection,
    filter: Filter,
    limit: i64,
) -> Result<Vec<Speed>, sqlx::Error> {
    // `CAST(round(...) AS INTEGER)`: `avg` gives a decimal; whole
    // milliseconds are plenty. `name AS "name!"`: SQLite can't tell the
    // macro that `name` is never NULL here, so the `!` says it.
    sqlx::query_as!(
        Speed,
        r#"SELECT name AS "name!", count(*) AS "uses!: i64",
                  CAST(round(avg(duration_ms)) AS INTEGER) AS "avg_ms!: i64",
                  max(duration_ms) AS "max_ms!: i64"
           FROM command_log
           WHERE kind IN ('command', 'list_reply') AND (?1 IS NULL OR datetime(at) >= datetime(?1))
             AND (?2 IS NULL OR user_id = ?2) AND (?3 IS NULL OR chat_id = ?3)
           GROUP BY name
           ORDER BY avg(duration_ms) DESC, name
           LIMIT ?4"#,
        filter.since,
        filter.user,
        filter.chat,
        limit
    )
    .fetch_all(conn)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::metrics::{self, Kind};
    use crate::parser::parse;
    use crate::person::Person;
    use crate::repo::{chats, users};

    #[tokio::test]
    async fn a_row_is_saved_with_its_details() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now: DateTime<Utc> = "2026-10-28T18:00:00Z".parse().unwrap();
        let bilal = Person {
            name: "Bilal".to_string(),
            jid: "447700900001@s.whatsapp.net".to_string(),
            lid: None,
        };
        let user = users::find_or_register(&mut conn, &bilal, now)
            .await
            .unwrap()
            .unwrap();
        let chat = chats::find_or_create(&mut conn, "120363000000000001@g.us", None, now)
            .await
            .unwrap();

        let event = metrics::command(&parse("/w week").unwrap()).typed_as("/w week");
        let row = Logged {
            event: &event,
            chat_id: chat,
            user_id: user.id,
            is_group: true,
            ok: true,
            duration_ms: 180,
            at: now,
        };
        record(&mut conn, &row).await.unwrap();
        let unknown = metrics::unknown("+remind 5pm").unwrap();
        record(
            &mut conn,
            &Logged {
                event: &unknown,
                ok: false,
                ..row
            },
        )
        .await
        .unwrap();

        let rows = sqlx::query!(
            r#"SELECT kind, name, detail, typed, ok AS "ok: bool", duration_ms
               FROM command_log ORDER BY id"#
        )
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, Kind::Command.as_str());
        assert_eq!(rows[0].name, "weather");
        assert_eq!(rows[0].detail.as_deref(), Some("week"));
        assert_eq!(rows[0].typed.as_deref(), Some("/w"));
        assert!(rows[0].ok);
        assert_eq!(rows[0].duration_ms, 180);
        assert_eq!(
            (rows[1].kind.as_str(), rows[1].name.as_str()),
            ("unknown", "+remind")
        );
        assert!(!rows[1].ok);
    }
}

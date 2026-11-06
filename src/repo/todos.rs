//! Todos. Every query is limited to one chat (`chat_id`), so one group can
//! never see or change another group's list.

use std::str::FromStr;

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqliteConnection;

use crate::todo_flags::Priority;

/// One open todo, as shown in a `/todo` list.
// Only the tests use this.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub struct OpenTodo {
    pub id: i64,
    pub title: String,
    /// The assignee's display name, if there is one.
    pub assignee: Option<String>,
}

/// The optional extras on a new todo (`-d fri -p high`).
///
/// `#[derive(Default)]` gives `Details::default()`, with both `None`: a
/// plain todo. Grouping them keeps `add`'s argument list short.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Details {
    pub due: Option<NaiveDate>,
    pub priority: Option<Priority>,
}

/// Adds a pending todo and returns its id.
pub async fn add(
    conn: &mut SqliteConnection,
    chat_id: i64,
    title: &str,
    created_by: i64,
    assignee_id: Option<i64>,
    details: Details,
    now: DateTime<Utc>,
) -> Result<i64, sqlx::Error> {
    // Stored as its text ("high"), or NULL. `map` with a function name
    // (`Priority::as_str`) is the same as `map(|p| p.as_str())`.
    let priority = details.priority.map(Priority::as_str);
    // `AS "id!"`: the `!` tells the macro this column is never NULL.
    // SQLite can't work that out for `RETURNING`, so without it the macro
    // would make it `Option<i64>`.
    sqlx::query_scalar!(
        r#"INSERT INTO todos (chat_id, title, created_by, assignee_id, due_date, priority,
                              created_at)
           VALUES (?, ?, ?, ?, ?, ?, ?)
           RETURNING id AS "id!""#,
        chat_id,
        title,
        created_by,
        assignee_id,
        details.due,
        priority,
        now
    )
    .fetch_one(conn)
    .await
}

/// The chat's pending todos, oldest first. That order is what the numbers
/// in a `/todo` list refer to.
// Only the tests use this.
#[cfg(test)]
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

/// One todo as `/todo` shows it: open, done or skipped.
#[derive(Debug, Clone, PartialEq)]
pub struct ListedTodo {
    pub id: i64,
    pub title: String,
    /// The assignee's display name, if there is one.
    pub assignee: Option<String>,
    /// Who closed it (done or skipped) and when; `None` while it's open.
    pub closed: Option<Closed>,
    /// The day it's due, if one was given (`-d fri`).
    pub due: Option<NaiveDate>,
    /// `-p high`, if given.
    pub priority: Option<Priority>,
}

impl ListedTodo {
    /// Still open, and its due date is before `today` (the reader's today).
    pub fn is_overdue(&self, today: NaiveDate) -> bool {
        self.closed.is_none() && self.due.is_some_and(|due| due < today)
    }

    /// Ticked off as done (not skipped, not open).
    pub fn is_done(&self) -> bool {
        self.closed.as_ref().is_some_and(|c| !c.skipped)
    }

    /// Skipped: closed as "won't be done".
    pub fn is_skipped(&self) -> bool {
        self.closed.as_ref().is_some_and(|c| c.skipped)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Closed {
    /// Display name of whoever ticked it off or skipped it.
    pub by: String,
    pub at: DateTime<Utc>,
    /// Skipped (won't be done) rather than done.
    pub skipped: bool,
}

/// The whole list, as `/todo` shows it (see `sort_for_list`): open todos,
/// soonest due first, then closed ones, done or skipped. That order is what
/// the numbers in a `/todo` list refer to.
pub async fn list_all(
    conn: &mut SqliteConnection,
    chat_id: i64,
) -> Result<Vec<ListedTodo>, sqlx::Error> {
    // Two LEFT JOINs on `users`, under two names (`a` for the assignee, `c`
    // for whoever closed it), because a todo points at two people.
    // `as "x: DateTime<Utc>"` reads the stored text as a chrono time.
    // `as "due_date: NaiveDate"` reads the stored "2026-11-13" as a date.
    let rows = sqlx::query!(
        r#"SELECT t.id, t.title, t.status, t.priority,
                  t.due_date AS "due_date: NaiveDate",
                  a.display_name AS "assignee?",
                  c.display_name AS "closed_by?",
                  t.closed_at AS "closed_at?: DateTime<Utc>"
           FROM todos t
           LEFT JOIN users a ON a.id = t.assignee_id
           LEFT JOIN users c ON c.id = t.closed_by
           WHERE t.chat_id = ?
           ORDER BY t.status != 'pending', t.closed_at, t.created_at, t.id"#,
        chat_id
    )
    .fetch_all(conn)
    .await?;

    let mut todos: Vec<ListedTodo> = rows
        .into_iter()
        .map(|r| ListedTodo {
            id: r.id,
            title: r.title,
            assignee: r.assignee,
            // `zip` pairs two `Option`s: `Some` only if both are `Some`.
            closed: r.closed_by.zip(r.closed_at).map(|(by, at)| Closed {
                by,
                at,
                skipped: r.status.parse() == Ok(TodoStatus::Skipped),
            }),
            due: r.due_date,
            // The schema only allows the three words `Priority::parse` knows.
            priority: r.priority.as_deref().and_then(Priority::parse),
        })
        .collect();
    sort_for_list(&mut todos);
    Ok(todos)
}

/// Puts the open todos in the order `/todo` numbers them: soonest due date
/// first (so overdue ones come first), then ones with no date; on the same
/// day, higher priority first. Closed todos stay after them as they are.
///
/// `todos` comes from SQL already split (open, then closed) with each part
/// oldest first. `sort_by_key` is *stable*: items with equal keys keep
/// their order, so ties stay oldest first, and every closed todo (all with
/// the same key, `None`) keeps the order it was closed in.
///
/// The key is a tuple. Tuples compare field by field, left to right, like
/// words in a dictionary: the second field only matters when the first
/// ones are equal.
pub fn sort_for_list(todos: &mut [ListedTodo]) {
    todos.sort_by_key(|todo| {
        let open_key = todo.closed.is_none().then(|| {
            (
                // `false` sorts before `true`: todos with a date first.
                todo.due.is_none(),
                todo.due,
                Priority::rank(todo.priority),
            )
        });
        // Open todos (`false`) before closed ones (`true`).
        (todo.closed.is_some(), open_key)
    });
}

/// Where a todo is, as `todos.status` stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    Completed,
    /// Closed as "won't be done".
    Skipped,
}

impl TodoStatus {
    /// The exact text stored in the database (and allowed by its CHECK).
    pub fn as_db_str(self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::Completed => "completed",
            TodoStatus::Skipped => "skipped",
        }
    }
}

impl FromStr for TodoStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        [
            TodoStatus::Pending,
            TodoStatus::Completed,
            TodoStatus::Skipped,
        ]
        .into_iter()
        .find(|t| t.as_db_str() == s)
        .ok_or_else(|| format!("unknown todo status '{s}'"))
    }
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

/// How far through its todos a chat is, for the progress bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub done: i64,
    pub skipped: i64,
    /// Every todo: open, done and skipped.
    pub total: i64,
}

/// The chat's `Progress`.
pub async fn progress(conn: &mut SqliteConnection, chat_id: i64) -> Result<Progress, sqlx::Error> {
    // `query_as!` fills the struct by matching column names to fields.
    sqlx::query_as!(
        Progress,
        r#"SELECT COUNT(*) FILTER (WHERE status = 'completed') AS "done!: i64",
                  COUNT(*) FILTER (WHERE status = 'skipped') AS "skipped!: i64",
                  COUNT(*) AS "total!: i64"
           FROM todos WHERE chat_id = ?"#,
        chat_id
    )
    .fetch_one(conn)
    .await
}

/// Deletes the chat's closed todos, done and skipped (`/todo clear done`).
/// Returns how many.
pub async fn clear_done(conn: &mut SqliteConnection, chat_id: i64) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM todos WHERE chat_id = ? AND status != 'pending'",
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

/// A todo that was just closed: what the reply names, and what its points
/// depend on (`points::for_todo`).
#[derive(Debug, Clone, PartialEq)]
pub struct ClosedTodo {
    pub title: String,
    pub priority: Option<Priority>,
    pub due: Option<NaiveDate>,
}

/// Marks a todo done. Returns it, or `None` if it isn't a pending todo in
/// this chat (already done or skipped, removed, or another chat's).
pub async fn complete(
    conn: &mut SqliteConnection,
    id: i64,
    chat_id: i64,
    by: i64,
    now: DateTime<Utc>,
) -> Result<Option<ClosedTodo>, sqlx::Error> {
    close(conn, id, chat_id, by, now, TodoStatus::Completed).await
}

/// Skips a todo: closes it as "won't be done", keeping it on record.
/// Returns it, or `None` like `complete`.
pub async fn skip(
    conn: &mut SqliteConnection,
    id: i64,
    chat_id: i64,
    by: i64,
    now: DateTime<Utc>,
) -> Result<Option<ClosedTodo>, sqlx::Error> {
    close(conn, id, chat_id, by, now, TodoStatus::Skipped).await
}

/// Closes a pending todo with `status` (`Completed` or `Skipped`; the
/// schema's CHECK refuses a closed todo that's `Pending`).
async fn close(
    conn: &mut SqliteConnection,
    id: i64,
    chat_id: i64,
    by: i64,
    now: DateTime<Utc>,
    status: TodoStatus,
) -> Result<Option<ClosedTodo>, sqlx::Error> {
    let status = status.as_db_str();
    // One statement checks and changes, so there's no gap between "is it
    // still pending?" and "close it" for someone else to slip into.
    let row = sqlx::query!(
        r#"UPDATE todos
           SET status = ?, closed_by = ?, closed_at = ?
           WHERE id = ? AND chat_id = ? AND status = 'pending'
           RETURNING title, priority, due_date AS "due_date: NaiveDate""#,
        status,
        by,
        now,
        id,
        chat_id
    )
    .fetch_optional(conn)
    .await?;
    // `Option::map`: a closed todo becomes `Some(ClosedTodo)`, `None` stays.
    Ok(row.map(|r| ClosedTodo {
        title: r.title,
        // The schema only allows the three words `Priority::parse` knows.
        priority: r.priority.as_deref().and_then(Priority::parse),
        due: r.due_date,
    }))
}

/// Sets (or, with `None`, takes off) an open todo's due date. Returns its
/// title, or `None` if it isn't a pending todo in this chat: a closed
/// todo's date doesn't matter any more.
pub async fn set_due(
    conn: &mut SqliteConnection,
    id: i64,
    chat_id: i64,
    due: Option<NaiveDate>,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "UPDATE todos SET due_date = ?
         WHERE id = ? AND chat_id = ? AND status = 'pending'
         RETURNING title",
        due,
        id,
        chat_id
    )
    .fetch_optional(conn)
    .await
}

/// Sets (or takes off) an open todo's priority. Returns its title, or
/// `None` like `set_due`.
pub async fn set_priority(
    conn: &mut SqliteConnection,
    id: i64,
    chat_id: i64,
    priority: Option<Priority>,
) -> Result<Option<String>, sqlx::Error> {
    let priority = priority.map(Priority::as_str);
    sqlx::query_scalar!(
        "UPDATE todos SET priority = ?
         WHERE id = ? AND chat_id = ? AND status = 'pending'
         RETURNING title",
        priority,
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
    use crate::repo::chats;
    use crate::testkit::register;

    #[test]
    fn todo_status_round_trips_through_its_db_text() {
        for status in [
            TodoStatus::Pending,
            TodoStatus::Completed,
            TodoStatus::Skipped,
        ] {
            assert_eq!(status.as_db_str().parse::<TodoStatus>(), Ok(status));
        }
        assert!("done".parse::<TodoStatus>().is_err());
        assert!("Completed".parse::<TodoStatus>().is_err());
    }

    /// A user and a group chat; returns (chat_id, user_id).
    async fn setup(conn: &mut SqliteConnection) -> (i64, i64) {
        let now = Utc::now();
        let user = register(conn, "Bilal", "447700900001", now).await;
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

        let gym = add(
            &mut conn,
            chat,
            "go gym",
            user,
            Some(user),
            Details::default(),
            now,
        )
        .await
        .unwrap();
        let tap = add(
            &mut conn,
            chat,
            "fix the tap",
            user,
            None,
            Details::default(),
            now,
        )
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
                .map(|t| t.title)
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

        let id = add(
            &mut conn,
            chat,
            "go gym",
            user,
            None,
            Details::default(),
            now,
        )
        .await
        .unwrap();
        assert!(list_open(&mut conn, other).await.unwrap().is_empty());
        assert_eq!(
            complete(&mut conn, id, other, user, now).await.unwrap(),
            None
        );
        assert_eq!(remove(&mut conn, id, other).await.unwrap(), None);
        assert_eq!(skip(&mut conn, id, other, user, now).await.unwrap(), None);
    }

    #[tokio::test]
    async fn due_date_and_priority_are_saved() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let (chat, user) = setup(&mut conn).await;
        let due = NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
        let details = Details {
            due: Some(due),
            priority: Some(Priority::High),
        };

        let id = add(&mut conn, chat, "bins", user, None, details, Utc::now())
            .await
            .unwrap();
        // Straight from the table: what's stored is the plain text.
        let row = sqlx::query!("SELECT due_date, priority FROM todos WHERE id = ?", id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(row.due_date.as_deref(), Some("2026-11-13"));
        assert_eq!(row.priority.as_deref(), Some("high"));
    }

    #[tokio::test]
    async fn skipped_todos_are_closed_but_not_done() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let (chat, user) = setup(&mut conn).await;
        let now = Utc::now();
        let plain = Details::default();
        let gym = add(&mut conn, chat, "go gym", user, None, plain, now)
            .await
            .unwrap();
        let bins = add(&mut conn, chat, "bins", user, None, plain, now)
            .await
            .unwrap();
        add(&mut conn, chat, "hoover", user, None, plain, now)
            .await
            .unwrap();

        assert_eq!(
            skip(&mut conn, gym, chat, user, now)
                .await
                .unwrap()
                .map(|t| t.title)
                .as_deref(),
            Some("go gym")
        );
        // Closed now: it can't be skipped again, or done.
        assert_eq!(skip(&mut conn, gym, chat, user, now).await.unwrap(), None);
        assert_eq!(
            complete(&mut conn, gym, chat, user, now).await.unwrap(),
            None
        );
        complete(&mut conn, bins, chat, user, now).await.unwrap();

        let progress = progress(&mut conn, chat).await.unwrap();
        assert_eq!(
            progress,
            Progress {
                done: 1,
                skipped: 1,
                total: 3
            }
        );
        // Open first, then closed in the order they were closed.
        let listed = list_all(&mut conn, chat).await.unwrap();
        let shown: Vec<(&str, bool, bool)> = listed
            .iter()
            .map(|t| (t.title.as_str(), t.is_done(), t.is_skipped()))
            .collect();
        assert_eq!(
            shown,
            [
                ("hoover", false, false),
                ("go gym", false, true),
                ("bins", true, false)
            ]
        );
        assert!(
            list_open(&mut conn, chat)
                .await
                .unwrap()
                .iter()
                .all(|t| t.title == "hoover")
        );

        // `/todo clear done` takes the skipped one too.
        assert_eq!(clear_done(&mut conn, chat).await.unwrap(), 2);
    }

    fn day(d: u32) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(2026, 11, d)
    }

    /// An open todo, nobody's.
    fn listed(title: &str, due: Option<NaiveDate>, priority: Option<Priority>) -> ListedTodo {
        ListedTodo {
            id: 0,
            title: title.to_string(),
            assignee: None,
            closed: None,
            due,
            priority,
        }
    }

    fn titles(todos: &[ListedTodo]) -> Vec<&str> {
        todos.iter().map(|t| t.title.as_str()).collect()
    }

    #[test]
    fn soonest_due_first_then_priority_then_oldest() {
        let mut done = listed("done", day(1), Some(Priority::High));
        done.closed = Some(Closed {
            by: "Bilal".to_string(),
            at: Utc::now(),
            skipped: false,
        });
        // In the order SQL gives them: open oldest first, then closed.
        let mut todos = vec![
            listed("no date", None, None),
            listed("no date, high", None, Some(Priority::High)),
            listed("13th, low", day(13), Some(Priority::Low)),
            listed("13th", day(13), None),
            listed("13th, high", day(13), Some(Priority::High)),
            listed("3rd (overdue)", day(3), None),
            listed("13th again", day(13), None),
            done,
        ];
        sort_for_list(&mut todos);
        assert_eq!(
            titles(&todos),
            [
                "3rd (overdue)",
                "13th, high",
                // Same day and priority (none counts as medium): oldest first.
                "13th",
                "13th again",
                "13th, low",
                "no date, high",
                "no date",
                // Closed ones stay last, whatever their date.
                "done",
            ]
        );
    }

    #[tokio::test]
    async fn list_all_is_in_due_order() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let (chat, user) = setup(&mut conn).await;
        let now = Utc::now();
        let due = |d| Details {
            due: day(d),
            priority: None,
        };
        add(
            &mut conn,
            chat,
            "whenever",
            user,
            None,
            Details::default(),
            now,
        )
        .await
        .unwrap();
        add(&mut conn, chat, "later", user, None, due(20), now)
            .await
            .unwrap();
        add(&mut conn, chat, "sooner", user, None, due(9), now)
            .await
            .unwrap();

        let listed = list_all(&mut conn, chat).await.unwrap();
        assert_eq!(titles(&listed), ["sooner", "later", "whenever"]);
        // Read back as a date and a priority, not text.
        assert_eq!(listed[0].due, day(9));
        assert_eq!(listed[2].due, None);
    }
}

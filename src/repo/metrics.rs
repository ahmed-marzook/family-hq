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

//! The admin dashboard's usage metrics: `GET /api/metrics`, read-only and
//! behind the password. The questions are the ones in docs/METRICS.md.
//!
//! Days and hours are on the server's default clock (`DEFAULT_TIMEZONE`),
//! not UTC: the times are grouped here, in Rust, so a UK summer evening
//! counts as the evening it was.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::AppError;
use crate::metrics::{by_hour, per_day};
use crate::repo::metrics as log;

/// How many failures and slow commands to list.
const LIST_LIMIT: i64 = 20;

/// The query string: `?days=7`. Leave it out for all time.
///
/// `Query<Period>` parses the part of the URL after `?` into this struct,
/// with serde. Axum answers 400 by itself if it doesn't fit (`?days=abc`).
#[derive(Debug, Deserialize)]
pub struct Period {
    days: Option<u16>,
}

#[derive(Debug, Serialize)]
pub struct Metrics {
    /// The period asked for; `null` means all time.
    days: Option<u16>,
    /// The clock `per_day` and `hours` use, e.g. "Europe/London".
    timezone: String,
    totals: Totals,
    /// One entry per day, quiet days included, oldest first.
    per_day: Vec<DayView>,
    /// Uses in each hour of the day, 0 to 23.
    hours: [i64; 24],
    commands: Vec<CommandView>,
    /// Everyone, including people with no uses in the period.
    people: Vec<PersonView>,
    unknown: Vec<UnknownView>,
    /// The latest failures, newest first (`totals.failures` counts them all).
    failures: Vec<FailureView>,
    slowest: Vec<SpeedView>,
}

#[derive(Debug, Serialize)]
struct Totals {
    uses: i64,
    people: i64,
    unknown: i64,
    failures: i64,
}

#[derive(Debug, Serialize)]
struct DayView {
    /// "2026-11-02".
    day: NaiveDate,
    uses: i64,
    people: i64,
}

#[derive(Debug, Serialize)]
struct CommandView {
    name: String,
    uses: i64,
}

#[derive(Debug, Serialize)]
struct PersonView {
    name: String,
    uses: i64,
    /// Days with at least one use, in the period.
    active_days: i64,
    /// Their latest use ever, not just in the period.
    last_seen: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
struct UnknownView {
    typed: String,
    times: i64,
    people: i64,
}

#[derive(Debug, Serialize)]
struct FailureView {
    at: DateTime<Utc>,
    person: String,
    name: String,
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
struct SpeedView {
    name: String,
    uses: i64,
    avg_ms: i64,
    max_ms: i64,
}

/// `GET /api/metrics?days=7`.
pub async fn metrics(
    State(state): State<Arc<AppState>>,
    Query(period): Query<Period>,
) -> Result<Json<Metrics>, AppError> {
    let timezone = state.apis.prayer_times.defaults.timezone;
    let now = Utc::now();
    // `?days=0` would be an empty period; treat it like no period at all.
    let days = period.days.filter(|days| *days > 0);
    let since = days.map(|days| now - Duration::days(i64::from(days)));

    let mut conn = state.db.acquire().await?;
    let uses = log::use_times(&mut conn, since).await?;
    let commands = log::top_commands(&mut conn, since).await?;
    let people = log::people(&mut conn).await?;
    let unknown = log::unknown_words(&mut conn, since).await?;
    let (failure_count, failures) = log::failures(&mut conn, since, LIST_LIMIT).await?;
    let slowest = log::slowest(&mut conn, since, LIST_LIMIT).await?;

    // The chart runs from the start of the period (or the first use ever)
    // to today, on the dashboard's clock.
    let today = now.with_timezone(&timezone).date_naive();
    let first_day = match since {
        Some(since) => since.with_timezone(&timezone).date_naive(),
        None => uses
            .first()
            .map_or(today, |(at, _)| at.with_timezone(&timezone).date_naive()),
    };

    // Each person's uses and active days, from the same list of uses.
    // `entry(...).or_default()` gets the person's slot, creating an empty
    // one the first time.
    let mut per_person: HashMap<i64, (i64, HashSet<NaiveDate>)> = HashMap::new();
    for (at, user) in &uses {
        let (count, days) = per_person.entry(*user).or_default();
        *count += 1;
        days.insert(at.with_timezone(&timezone).date_naive());
    }
    let mut people: Vec<PersonView> = people
        .into_iter()
        .map(|p| {
            let (uses, days) = per_person.remove(&p.id).unwrap_or_default();
            PersonView {
                name: p.name,
                uses,
                active_days: days.len() as i64,
                last_seen: p.last_seen,
            }
        })
        .collect();
    // Most uses first; `people` was A to Z, and the sort keeps that order
    // for equal counts.
    people.sort_by_key(|p| std::cmp::Reverse(p.uses));

    Ok(Json(Metrics {
        days,
        timezone: timezone.name().to_string(),
        totals: Totals {
            uses: uses.len() as i64,
            people: uses
                .iter()
                .map(|(_, user)| user)
                .collect::<HashSet<_>>()
                .len() as i64,
            unknown: unknown.iter().map(|u| u.times).sum(),
            failures: failure_count,
        },
        per_day: per_day(&uses, timezone, first_day, today)
            .into_iter()
            .map(|d| DayView {
                day: d.day,
                uses: d.uses,
                people: d.people,
            })
            .collect(),
        hours: by_hour(&uses, timezone),
        commands: commands
            .into_iter()
            .map(|c| CommandView {
                name: c.name,
                uses: c.uses,
            })
            .collect(),
        people,
        unknown: unknown
            .into_iter()
            .map(|u| UnknownView {
                typed: u.typed,
                times: u.times,
                people: u.people,
            })
            .collect(),
        failures: failures
            .into_iter()
            .map(|f| FailureView {
                at: f.at,
                person: f.person,
                name: f.name,
                detail: f.detail,
            })
            .collect(),
        slowest: slowest
            .into_iter()
            .map(|s| SpeedView {
                name: s.name,
                uses: s.uses,
                avg_ms: s.avg_ms,
                max_ms: s.max_ms,
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;
    use serde_json::Value;
    use sqlx::SqliteConnection;

    use super::*;
    use crate::db::test_pool;
    use crate::metrics::{Event, Kind};
    use crate::person::Person;
    use crate::repo::metrics::{Logged, record};
    use crate::repo::{chats, users};
    use crate::routes::api::start_server_with;

    async fn get(url: String) -> (StatusCode, String) {
        let response = reqwest::Client::new()
            .get(url)
            .basic_auth("test", Some("test"))
            .send()
            .await
            .unwrap();
        (response.status(), response.text().await.unwrap())
    }

    #[allow(clippy::too_many_arguments)]
    async fn log_row(
        conn: &mut SqliteConnection,
        chat: i64,
        user: i64,
        kind: Kind,
        name: &str,
        ok: bool,
        ms: i64,
        at: DateTime<Utc>,
    ) {
        let event = Event {
            kind,
            name: name.to_string(),
            detail: None,
            typed: None,
        };
        let row = Logged {
            event: &event,
            chat_id: chat,
            user_id: user,
            is_group: true,
            ok,
            duration_ms: ms,
            at,
        };
        record(conn, &row).await.unwrap();
    }

    /// Bilal and Khadija use the bot; Zara never has. Some uses are older
    /// than 7 days, one failed, one was a word the bot didn't know.
    async fn seed(pool: &sqlx::SqlitePool) {
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let mut ids = Vec::new();
        for (name, phone) in [
            ("Bilal", "447700900001"),
            ("Khadija", "447700900002"),
            ("Zara", "447700900003"),
        ] {
            let person = Person {
                name: name.to_string(),
                jid: format!("{phone}@s.whatsapp.net"),
                lid: None,
            };
            let user = users::find_or_register(&mut conn, &person, now)
                .await
                .unwrap()
                .unwrap();
            ids.push(user.id);
        }
        let (bilal, khadija) = (ids[0], ids[1]);
        let chat = chats::find_or_create(&mut conn, "120363000000000001@g.us", None, now)
            .await
            .unwrap();

        let ago = |days: i64| now - Duration::days(days);
        let c = &mut *conn;
        log_row(c, chat, bilal, Kind::Command, "todo.add", true, 5, ago(1)).await;
        log_row(c, chat, bilal, Kind::Command, "todo.add", true, 5, ago(2)).await;
        log_row(c, chat, bilal, Kind::Command, "weather", false, 900, ago(2)).await;
        log_row(
            c,
            chat,
            khadija,
            Kind::ListReply,
            "todo.reply_done",
            true,
            3,
            ago(3),
        )
        .await;
        log_row(c, chat, khadija, Kind::Unknown, "+remind", true, 0, ago(1)).await;
        // Older than a week:
        log_row(c, chat, bilal, Kind::Command, "shop.add", true, 4, ago(20)).await;
        log_row(
            c,
            chat,
            khadija,
            Kind::Reaction,
            "prayers.react",
            true,
            2,
            ago(40),
        )
        .await;
    }

    #[tokio::test]
    async fn a_week_of_metrics() {
        let pool = test_pool().await;
        seed(&pool).await;
        let server = start_server_with(pool.clone()).await;

        let (status, body) = get(format!("{server}/api/metrics?days=7")).await;
        assert_eq!(status, StatusCode::OK);
        let m: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(m["days"], 7);
        assert_eq!(m["timezone"], "Europe/London");
        assert_eq!(
            m["totals"],
            serde_json::json!({ "uses": 4, "people": 2, "unknown": 1, "failures": 1 })
        );
        // Today and the 7 days before it (the period starts partway through
        // its first day).
        assert_eq!(m["per_day"].as_array().unwrap().len(), 8);
        let per_day: i64 = m["per_day"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["uses"].as_i64().unwrap())
            .sum();
        assert_eq!(per_day, 4);
        let hours: i64 = m["hours"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h.as_i64().unwrap())
            .sum();
        assert_eq!(hours, 4);
        assert_eq!(
            m["commands"][0],
            serde_json::json!({ "name": "todo.add", "uses": 2 })
        );
        assert_eq!(m["unknown"][0]["typed"], "+remind");
        assert_eq!(m["failures"][0]["name"], "weather");
        assert_eq!(m["failures"][0]["person"], "Bilal");
        assert_eq!(m["slowest"][0]["name"], "weather");
        assert_eq!(m["slowest"][0]["max_ms"], 900);

        let people = m["people"].as_array().unwrap();
        let summary: Vec<(&str, i64, i64)> = people
            .iter()
            .map(|p| {
                (
                    p["name"].as_str().unwrap(),
                    p["uses"].as_i64().unwrap(),
                    p["active_days"].as_i64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [("Bilal", 3, 2), ("Khadija", 1, 1), ("Zara", 0, 0)]
        );
        assert_eq!(people[2]["last_seen"], Value::Null);
    }

    #[tokio::test]
    async fn totals_match_the_metrics_doc_queries() {
        let pool = test_pool().await;
        seed(&pool).await;
        let server = start_server_with(pool.clone()).await;

        for (query, days) in [
            ("?days=7", "-7 days"),
            ("?days=30", "-30 days"),
            ("", "-100 years"),
        ] {
            let (_, body) = get(format!("{server}/api/metrics{query}")).await;
            let m: Value = serde_json::from_str(&body).unwrap();

            // "Most used commands this week" from docs/METRICS.md, summed,
            // with the period swapped in.
            let mut conn = pool.acquire().await.unwrap();
            let (uses, people): (i64, i64) = sqlx::query_as(
                "SELECT count(*), count(DISTINCT user_id) FROM command_log
                 WHERE kind != 'unknown' AND datetime(at) >= datetime('now', ?)",
            )
            .bind(days)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
            let failures: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM command_log
                 WHERE ok = 0 AND kind != 'unknown' AND datetime(at) >= datetime('now', ?)",
            )
            .bind(days)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
            drop(conn);

            assert_eq!(m["totals"]["uses"], uses, "{query}");
            assert_eq!(m["totals"]["people"], people, "{query}");
            assert_eq!(m["totals"]["failures"], failures, "{query}");
        }
    }

    #[tokio::test]
    async fn all_time_and_bad_periods() {
        let pool = test_pool().await;
        seed(&pool).await;
        let server = start_server_with(pool).await;

        let (_, body) = get(format!("{server}/api/metrics")).await;
        let m: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(m["days"], Value::Null);
        assert_eq!(m["totals"]["uses"], 6);
        // From the first use (40 days ago) to today.
        assert_eq!(m["per_day"].as_array().unwrap().len(), 41);

        let (status, _) = get(format!("{server}/api/metrics?days=abc")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = get(format!("{server}/api/metrics?days=-1")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn no_uses_at_all() {
        let server = start_server_with(test_pool().await).await;
        let (status, body) = get(format!("{server}/api/metrics?days=30")).await;
        assert_eq!(status, StatusCode::OK);
        let m: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(m["totals"]["uses"], 0);
        assert_eq!(m["people"], serde_json::json!([]));
    }
}

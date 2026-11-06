//! Clearing a person's data from the admin dashboard, without waiting for
//! them to be inactive: `POST /api/users/{id}/clear`. The same clearing as
//! the automatic one (`crate::cleanup`), so the same things are deleted.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use chrono::Utc;
use serde::Serialize;
use tracing::info;

use crate::AppState;
use crate::cleanup::leave_and_delete;
use crate::error::AppError;
use crate::repo::{admin, cleanup};

#[derive(Debug, Serialize)]
pub struct ClearResult {
    /// `true` if they're gone completely; `false` if their name was kept
    /// because a group's list still mentions them.
    deleted: bool,
}

/// `POST /api/users/{id}/clear` (with a JSON body, `{}`): clears their data
/// now. 404 if there's no such person.
pub async fn clear(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<ClearResult>, AppError> {
    let mut tx = state.db.begin().await?;
    if admin::user_jid(&mut tx, id).await?.is_none() {
        return Err(AppError::NotFound);
    }
    let cleared = cleanup::clear(&mut tx, id, Utc::now()).await?;
    tx.commit().await?;
    info!(
        "cleared user {id}'s data{}",
        if cleared.deleted_user {
            ""
        } else {
            "; kept their name, a group list mentions them"
        }
    );
    Ok(Json(ClearResult {
        deleted: cleared.deleted_user,
    }))
}

/// `POST /api/chats/{id}/leave` (with `{}`): the bot leaves the group now,
/// and everything stored for it is deleted. 404 for a DM or no such chat.
/// If WhatsApp won't let the bot leave, nothing is deleted.
pub async fn leave(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    let wa_chat_id = {
        let mut conn = state.db.acquire().await?;
        cleanup::groups(&mut conn)
            .await?
            .into_iter()
            .find(|g| g.id == id)
            .ok_or(AppError::NotFound)?
            .wa_chat_id
    };
    leave_and_delete(&state, id, &wa_chat_id, None)
        .await
        .map_err(|err| match err {
            AppError::Http(err) => AppError::BadRequest(format!(
                "WhatsApp didn't let the bot leave, so nothing was deleted ({err})."
            )),
            other => other,
        })?;
    info!("left group {id} and deleted its data");
    Ok(Json(serde_json::json!({ "left": true })))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{DateTime, Duration, Utc};
    use hmac::{Hmac, KeyInit, Mac};
    use reqwest::StatusCode;
    use serde_json::{Value, json};
    use sha2::Sha256;
    use sqlx::SqlitePool;

    use crate::cleanup;
    use crate::db::test_pool;
    use crate::repo::{chats, todos};
    use crate::testkit::{fake_gowa, serve_state, test_state};

    const GROUP: &str = "120363000000000001@g.us";
    const BILAL: &str = "447700900001@s.whatsapp.net";

    /// Sends a message to the app's webhook, signed as GOWA would.
    async fn webhook(server: &str, id: &str, chat: &str, text: &str) {
        let body = json!({
            "event": "message",
            "payload": {
                "id": id, "chat_id": chat, "from": BILAL, "from_name": "Bilal",
                "body": text, "is_from_me": false, "timestamp": "2026-11-02T09:00:00Z"
            }
        })
        .to_string();
        let mut mac = Hmac::<Sha256>::new_from_slice(b"test").unwrap();
        mac.update(body.as_bytes());
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        let status = reqwest::Client::new()
            .post(format!("{server}/webhook/whatsapp"))
            .header("x-hub-signature-256", signature)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, StatusCode::OK, "{text}");
    }

    async fn last_active(pool: &SqlitePool) -> DateTime<Utc> {
        sqlx::query_scalar("SELECT last_active_at FROM users WHERE wa_jid = ?")
            .bind(BILAL)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn make_old(pool: &SqlitePool, days: i64) -> DateTime<Utc> {
        let old = Utc::now() - Duration::days(days);
        sqlx::query("UPDATE users SET last_active_at = ? WHERE wa_jid = ?")
            .bind(old)
            .bind(BILAL)
            .execute(pool)
            .await
            .unwrap();
        old
    }

    #[tokio::test]
    async fn using_the_bot_counts_and_group_chat_does_not() {
        let pool = test_pool().await;
        let gowa = fake_gowa().await;
        let server = serve_state(test_state(pool.clone(), &gowa.url)).await;

        // A new person starts active.
        webhook(&server, "M1", GROUP, "morning all").await;
        assert!(Utc::now() - last_active(&pool).await < Duration::minutes(1));

        let old = make_old(&pool, 20).await;
        webhook(&server, "M2", GROUP, "see you at 6").await;
        assert_eq!(last_active(&pool).await, old, "ordinary group chat");

        webhook(&server, "M3", GROUP, "+shop milk").await;
        assert!(last_active(&pool).await > old, "a command");

        let old = make_old(&pool, 20).await;
        webhook(&server, "M4", GROUP, "+remind me later").await;
        assert!(last_active(&pool).await > old, "a command it didn't know");

        let old = make_old(&pool, 20).await;
        webhook(&server, "M5", BILAL, "thanks!").await;
        assert!(last_active(&pool).await > old, "anything in their DM");
    }

    async fn users(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM users")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_sweep_warns_then_clears() {
        let pool = test_pool().await;
        let gowa = fake_gowa().await;
        let sent = gowa.sent.clone();
        let state = test_state(pool.clone(), &gowa.url);
        let server = serve_state(state.clone()).await;
        webhook(&server, "M1", BILAL, "hello").await; // registers Bilal, with a DM
        sent.lock().unwrap().clear();
        make_old(&pool, 20).await;

        let now = Utc::now();
        cleanup::check(&state, now).await.unwrap();
        {
            let sent = sent.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0].0, BILAL);
            assert!(sent[0].1.contains("cleared in 3 days"), "{}", sent[0].1);
        }

        // Not warned twice, and nothing cleared before 3 days are up.
        cleanup::check(&state, now + Duration::days(2))
            .await
            .unwrap();
        assert_eq!(sent.lock().unwrap().len(), 1);
        assert_eq!(users(&pool).await, 1);

        cleanup::check(&state, now + Duration::days(3))
            .await
            .unwrap();
        assert_eq!(users(&pool).await, 0, "cleared, and nothing kept his name");
    }

    #[tokio::test]
    async fn a_reply_to_the_warning_keeps_their_data() {
        let pool = test_pool().await;
        let gowa = fake_gowa().await;
        let state = test_state(pool.clone(), &gowa.url);
        let server = serve_state(state.clone()).await;
        webhook(&server, "M1", BILAL, "hello").await;
        make_old(&pool, 20).await;

        let now = Utc::now();
        cleanup::check(&state, now).await.unwrap(); // warned
        webhook(&server, "M2", BILAL, "still here").await;
        cleanup::check(&state, now + Duration::days(3))
            .await
            .unwrap();
        assert_eq!(users(&pool).await, 1);
    }

    #[tokio::test]
    async fn off_clears_nobody() {
        let pool = test_pool().await;
        let gowa = fake_gowa().await;
        let sent = gowa.sent.clone();
        let mut state = test_state(pool.clone(), &gowa.url);
        Arc::get_mut(&mut state).unwrap().config.inactive_days = 0;
        let server = serve_state(state.clone()).await;
        webhook(&server, "M1", BILAL, "hello").await;
        sent.lock().unwrap().clear();
        make_old(&pool, 400).await;

        cleanup::check(&state, Utc::now()).await.unwrap();
        assert!(sent.lock().unwrap().is_empty());
        assert_eq!(users(&pool).await, 1);
    }

    async fn post_clear(server: &str, id: i64) -> (StatusCode, Value) {
        let response = reqwest::Client::new()
            .post(format!("{server}/api/users/{id}/clear"))
            .basic_auth("test", Some("test"))
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        let status = response.status();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn get(server: &str, path: &str) -> (StatusCode, Value) {
        let response = reqwest::Client::new()
            .get(format!("{server}{path}"))
            .basic_auth("test", Some("test"))
            .send()
            .await
            .unwrap();
        let status = response.status();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn the_dashboard_shows_when_and_can_clear_now() {
        let pool = test_pool().await;
        let gowa = fake_gowa().await;
        let state = test_state(pool.clone(), &gowa.url);
        let server = serve_state(state.clone()).await;
        webhook(&server, "M1", GROUP, "+todo fix the tap").await;
        let id: i64 = sqlx::query_scalar("SELECT id FROM users")
            .fetch_one(&pool)
            .await
            .unwrap();

        let (_, list) = get(&server, "/api/users").await;
        assert_eq!(
            list["cleanup"],
            json!({ "inactive_days": 14, "warn_days": 3 })
        );
        let row = &list["users"][0];
        assert!(row["clears_on"].is_string(), "{row}");
        assert_eq!(row["cleared_at"], Value::Null);

        // His group todo keeps his name: cleared, but the row stays.
        let (status, answer) = post_clear(&server, id).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(answer, json!({ "deleted": false }));
        let (status, page) = get(&server, &format!("/api/users/{id}")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page["user"]["cleared_at"].is_string());
        assert_eq!(page["user"]["clears_on"], Value::Null);
        assert_eq!(page["chats"], json!([]), "no group memberships left");

        // Once the group's list no longer names him, clearing removes him.
        let group: i64 = sqlx::query_scalar("SELECT id FROM chats")
            .fetch_one(&pool)
            .await
            .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        todos::clear_all(&mut conn, group).await.unwrap();
        assert!(
            chats::find_or_create(&mut conn, GROUP, None, Utc::now())
                .await
                .is_ok()
        );
        drop(conn);
        let (_, answer) = post_clear(&server, id).await;
        assert_eq!(answer, json!({ "deleted": true }));
        let (status, _) = get(&server, &format!("/api/users/{id}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = post_clear(&server, 999).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    async fn groups(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM chats WHERE kind = 'group'")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn make_group_old(pool: &SqlitePool, days: i64) {
        sqlx::query("UPDATE chats SET last_active_at = ? WHERE kind = 'group'")
            .bind(Utc::now() - Duration::days(days))
            .execute(pool)
            .await
            .unwrap();
    }

    /// A time that isn't on the hour, so `check` doesn't also ask GOWA for
    /// its groups (that's tested on its own).
    fn not_on_the_hour() -> DateTime<Utc> {
        use chrono::Timelike;
        Utc::now().with_minute(5).unwrap()
    }

    #[tokio::test]
    async fn a_quiet_group_is_warned_then_left_and_deleted() {
        let pool = test_pool().await;
        let gowa = fake_gowa().await;
        let state = test_state(pool.clone(), &gowa.url);
        let server = serve_state(state.clone()).await;
        webhook(&server, "M1", GROUP, "+shop milk").await;
        gowa.sent.lock().unwrap().clear();
        make_group_old(&pool, 20).await;

        let now = not_on_the_hour();
        cleanup::check(&state, now).await.unwrap();
        {
            let sent = gowa.sent.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0].0, GROUP);
            assert!(sent[0].1.contains("leave it in 3 days"), "{}", sent[0].1);
        }
        cleanup::check(&state, now + Duration::days(2))
            .await
            .unwrap();
        assert!(gowa.left.lock().unwrap().is_empty());

        cleanup::check(&state, now + Duration::days(3))
            .await
            .unwrap();
        assert_eq!(*gowa.left.lock().unwrap(), [GROUP]);
        assert_eq!(groups(&pool).await, 0);
        let items: i64 = sqlx::query_scalar("SELECT count(*) FROM shopping_items")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(items, 0);
    }

    #[tokio::test]
    async fn a_command_after_the_group_warning_keeps_the_bot() {
        let pool = test_pool().await;
        let gowa = fake_gowa().await;
        let state = test_state(pool.clone(), &gowa.url);
        let server = serve_state(state.clone()).await;
        webhook(&server, "M1", GROUP, "+shop milk").await;
        make_group_old(&pool, 20).await;

        let now = not_on_the_hour();
        cleanup::check(&state, now).await.unwrap(); // warned
        webhook(&server, "M2", GROUP, "/help").await;
        cleanup::check(&state, now + Duration::days(3))
            .await
            .unwrap();
        assert!(gowa.left.lock().unwrap().is_empty());
        assert_eq!(groups(&pool).await, 1);
    }

    #[tokio::test]
    async fn a_group_the_bot_was_removed_from_goes_a_day_later() {
        let pool = test_pool().await;
        let gowa = fake_gowa().await;
        let state = test_state(pool.clone(), &gowa.url);
        let server = serve_state(state.clone()).await;
        webhook(&server, "M1", GROUP, "+shop milk").await;
        let now = Utc::now();

        // GOWA lists no groups at all: not trusted, nothing happens.
        cleanup::check_groups(&state, now).await.unwrap();
        let left_at: Option<DateTime<Utc>> =
            sqlx::query_scalar("SELECT left_at FROM chats WHERE kind = 'group'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(left_at, None);

        // It lists another group, but not this one: the bot was removed.
        gowa.listed
            .lock()
            .unwrap()
            .push("120363000000000099@g.us".to_string());
        cleanup::check_groups(&state, now).await.unwrap();
        cleanup::check_groups(&state, now + Duration::hours(23))
            .await
            .unwrap();
        assert_eq!(groups(&pool).await, 1, "kept for a day");
        cleanup::check_groups(&state, now + Duration::hours(24))
            .await
            .unwrap();
        assert_eq!(groups(&pool).await, 0);
        assert!(gowa.left.lock().unwrap().is_empty(), "nothing to leave");
    }

    #[tokio::test]
    async fn a_group_that_comes_back_is_kept() {
        let pool = test_pool().await;
        let gowa = fake_gowa().await;
        let state = test_state(pool.clone(), &gowa.url);
        let server = serve_state(state.clone()).await;
        webhook(&server, "M1", GROUP, "+shop milk").await;
        let now = Utc::now();

        gowa.listed
            .lock()
            .unwrap()
            .push("120363000000000099@g.us".to_string());
        cleanup::check_groups(&state, now).await.unwrap(); // missing
        gowa.listed.lock().unwrap().push(GROUP.to_string());
        cleanup::check_groups(&state, now + Duration::hours(2))
            .await
            .unwrap(); // back
        gowa.listed.lock().unwrap().retain(|g| g != GROUP);
        cleanup::check_groups(&state, now + Duration::hours(25))
            .await
            .unwrap(); // missing again
        assert_eq!(groups(&pool).await, 1, "the day starts again");
    }

    #[tokio::test]
    async fn the_dashboard_can_make_the_bot_leave_now() {
        let pool = test_pool().await;
        let gowa = fake_gowa().await;
        let state = test_state(pool.clone(), &gowa.url);
        let server = serve_state(state.clone()).await;
        webhook(&server, "M1", GROUP, "+shop milk").await;
        webhook(&server, "M2", BILAL, "hello").await;
        let group: i64 = sqlx::query_scalar("SELECT id FROM chats WHERE kind = 'group'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let dm: i64 = sqlx::query_scalar("SELECT id FROM chats WHERE kind = 'dm'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let leave = |chat: i64| {
            reqwest::Client::new()
                .post(format!("{server}/api/chats/{chat}/leave"))
                .basic_auth("test", Some("test"))
                .json(&json!({}))
                .send()
        };

        // The user page says when the bot would leave.
        let (_, page) = get(&server, "/api/users/1").await;
        let card = page["chats"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == group)
            .unwrap()
            .clone();
        assert!(card["leaves_on"].is_string(), "{card}");

        // WhatsApp refuses: nothing is deleted, and the reason says so.
        *gowa.refuse_leave.lock().unwrap() = true;
        let response = leave(group).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value = response.json().await.unwrap();
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("nothing was deleted")
        );
        assert_eq!(groups(&pool).await, 1);

        *gowa.refuse_leave.lock().unwrap() = false;
        let response = leave(group).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(*gowa.left.lock().unwrap(), [GROUP]);
        assert_eq!(groups(&pool).await, 0);

        // A DM isn't a group to leave.
        let response = leave(dm).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

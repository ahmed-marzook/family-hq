//! `POST /actions/...`: the buttons on push notifications (`crate::push`).
//!
//! The phone showing the push sends these itself when a button is tapped,
//! so this is the one part of family-hq meant to be reachable from the
//! internet (the Cloudflare tunnel should pass only `/actions/`). Every
//! request must carry the person's token as `Authorization: Bearer ...`;
//! without one we know, it's 401 and nothing happens.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use chrono::{DateTime, Duration, Utc};
use tracing::{Span, info, warn};

use crate::AppState;
use crate::error::AppError;
use crate::metrics::{self, Event};
use crate::push::Action;
use crate::repo::metrics::Logged;
use crate::repo::{self, chats, prayers, push as push_repo, users};

/// The token from `Authorization: Bearer <token>`, if there is one.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

/// `POST /actions/{*path}`: works out whose button it is and what it asks
/// for (`Action::from_path`), then does it. Answers with a few words, which
/// the ntfy app shows only if something went wrong.
///
/// `{*path}` is a *wildcard*: it matches the rest of the URL, slashes and
/// all, so each `Action` decides its own path shape.
#[tracing::instrument(
    name = "button",
    skip_all,
    fields(path = %path, user = tracing::field::Empty),
    err
)]
pub async fn action(
    State(state): State<Arc<AppState>>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Result<(StatusCode, &'static str), AppError> {
    let now = Utc::now();
    let Some(token) = bearer(&headers) else {
        warn!("no token");
        return Ok((StatusCode::UNAUTHORIZED, "Not allowed"));
    };
    let mut tx = state.db.begin().await?;
    let Some(user_id) = push_repo::user_for_token(&mut tx, token).await? else {
        // A wrong token, or one from before `/push off`.
        warn!("unknown token");
        return Ok((StatusCode::UNAUTHORIZED, "Not allowed"));
    };
    Span::current().record("user", user_id);
    let Some(action) = Action::from_path(&path) else {
        warn!("no such action");
        return Ok((StatusCode::NOT_FOUND, "Unknown button"));
    };

    let (event, reply) = match action {
        Action::Prayed { date, prayer } => {
            // A button only ever exists for a reminder that was sent, so a
            // date after tomorrow (allowing for timezones) is made up.
            if date > (now + Duration::days(1)).date_naive() {
                return Ok((StatusCode::BAD_REQUEST, "That day hasn't come yet"));
            }
            let status = prayers::mark_prayed(&mut tx, user_id, date, prayer, now).await?;
            info!("prayed {prayer} on {date}");
            let reply = if status.is_some() {
                "Marked as prayed"
            } else {
                "Already marked"
            };
            (metrics::push_button("prayers.push"), reply)
        }
        Action::Test => (metrics::push_button("push.test"), "The button works"),
    };

    // Tapping a button is using the bot, so it keeps their data (see
    // `crate::cleanup`), and it's counted, under their DM.
    users::touch(&mut tx, user_id, now).await?;
    let dm = chats::dm_of(&mut tx, user_id).await?;
    if let Some((chat_id, _)) = &dm {
        record(&mut tx, &event, *chat_id, user_id, now).await?;
    }
    tx.commit().await?;

    // The test button also answers in WhatsApp, so they can see the whole
    // round trip worked. Saved already, so a failure here is only logged.
    if action == Action::Test
        && let Some((_, wa_chat_id)) = &dm
    {
        let text = "✅ The button on your test push reached me. Push notifications are all set.";
        if let Err(err) = state.gowa.send_text(wa_chat_id, text, None, &[]).await {
            warn!("couldn't confirm the test button: {err}");
        }
    }
    Ok((StatusCode::OK, reply))
}

async fn record(
    conn: &mut sqlx::SqliteConnection,
    event: &Event,
    chat_id: i64,
    user_id: i64,
    at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let row = Logged {
        event,
        chat_id,
        user_id,
        is_group: false,
        ok: true,
        duration_ms: 0,
        at,
    };
    repo::metrics::record(conn, &row).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::prayer::Prayer;
    use crate::repo::push::Subscription;
    use crate::testkit::{fake_gowa, register, serve_state, start_server_with, test_state};
    use chrono::NaiveDate;

    const TOKEN: &str = "a-token-that-is-at-least-32-characters-long";

    /// Someone with push on and a DM, on a server; returns the server's
    /// address, the pool and their id.
    async fn setup() -> (String, sqlx::SqlitePool, i64) {
        let pool = test_pool().await;
        let user_id = subscriber(&pool).await;
        (start_server_with(pool.clone()).await, pool, user_id)
    }

    /// Amina, with push on and a DM, in `pool`; returns her id.
    async fn subscriber(pool: &sqlx::SqlitePool) -> i64 {
        let mut conn = pool.acquire().await.unwrap();
        let now = Utc::now();
        let user = register(&mut conn, "Amina", "447700900001", now).await;
        chats::find_or_create(&mut conn, &user.wa_jid, Some(user.id), now)
            .await
            .unwrap();
        let sub = Subscription {
            topic: "fhq-".to_string() + &"t".repeat(32),
            token: TOKEN.to_string(),
        };
        push_repo::subscribe(&mut conn, user.id, &sub, now)
            .await
            .unwrap();
        // The test pool has one connection, and the server needs it.
        // (`conn` would be dropped at the end of this function anyway; the
        // `drop` just says so.)
        drop(conn);
        user.id
    }

    async fn press(server: &str, path: &str, token: Option<&str>) -> StatusCode {
        let mut request = reqwest::Client::new().post(format!("{server}/actions/{path}"));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.send().await.unwrap().status()
    }

    #[tokio::test]
    async fn a_button_needs_a_known_token() {
        let (server, _, _) = setup().await;
        assert_eq!(press(&server, "test", None).await, StatusCode::UNAUTHORIZED);
        assert_eq!(
            press(&server, "test", Some("not-the-token")).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(press(&server, "test", Some(TOKEN)).await, StatusCode::OK);
        // GET isn't a button press.
        let get = reqwest::get(format!("{server}/actions/test"))
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn unknown_buttons_are_404() {
        let (server, _, _) = setup().await;
        for path in ["nope", "prayers/prayed/2026-11-05/lunch", "prayers/prayed"] {
            assert_eq!(
                press(&server, path, Some(TOKEN)).await,
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn prayed_ticks_it_off_once() {
        let (server, pool, user_id) = setup().await;
        let today = Utc::now().date_naive();
        let action = Action::Prayed {
            date: today,
            prayer: Prayer::Asr,
        };
        assert_eq!(
            press(&server, &action.path(), Some(TOKEN)).await,
            StatusCode::OK
        );
        // Tapped again (or a second push's button): still fine.
        assert_eq!(
            press(&server, &action.path(), Some(TOKEN)).await,
            StatusCode::OK
        );

        let mut conn = pool.acquire().await.unwrap();
        let prayed = prayers::prayed_on(&mut conn, user_id, today).await.unwrap();
        assert!(prayed.contains(&Prayer::Asr));
        // Counted as a use, under their DM.
        let (count,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM command_log WHERE name = 'prayers.push'")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(count, 2);
    }

    /// The test button also answers in WhatsApp. If GOWA can't send that,
    /// the press was already saved, so it's still a 200; the failure is
    /// only logged.
    #[tokio::test]
    async fn test_button_is_ok_even_if_whatsapp_fails() {
        let pool = test_pool().await;
        let user_id = subscriber(&pool).await;
        let gowa = fake_gowa().await;
        *gowa.fail_sends.lock().unwrap() = true;
        let server = serve_state(test_state(pool.clone(), &gowa.url)).await;

        let response = reqwest::Client::new()
            .post(format!("{server}/actions/test"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "The button works");

        // It did try to confirm, in Amina's DM. `clone` copies the list out,
        // so the lock is let go at the end of this line.
        let sent = gowa.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "447700900001@s.whatsapp.net");
        // And the press was saved before that: counted, under her DM.
        let (count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM command_log WHERE name = 'push.test' AND user_id = ?",
        )
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn a_day_that_hasnt_come_is_refused() {
        let (server, _, _) = setup().await;
        let later = Action::Prayed {
            date: NaiveDate::from_ymd_opt(2099, 1, 1).unwrap(),
            prayer: Prayer::Fajr,
        };
        assert_eq!(
            press(&server, &later.path(), Some(TOKEN)).await,
            StatusCode::BAD_REQUEST
        );
    }
}

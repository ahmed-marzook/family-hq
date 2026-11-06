//! Shared fixtures for the tests: small helpers that many test modules
//! used to write out for themselves (parsing a time, registering Bilal,
//! starting a fake GOWA, ...). One copy here means a change to, say, how a
//! person is registered is made once.
//!
//! `lib.rs` declares this module with `#[cfg(test)]`, so it's only compiled
//! for `cargo test` and isn't part of the real program at all. `pub(crate)`
//! on each item means "visible anywhere in this crate, nowhere else": the
//! test modules can `use crate::testkit::...`, and nothing outside can.

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

use crate::AppState;
use crate::person::Person;
use crate::repo::users::{self, User};

// ---- Times and dates ------------------------------------------------------

/// A UTC time from text such as `"2026-11-02T12:00:00Z"`. `parse` works
/// out which type to build from the return type, `DateTime<Utc>`.
pub(crate) fn utc(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

/// A calendar date from text such as `"2026-11-02"`.
pub(crate) fn date(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

// ---- People ---------------------------------------------------------------

/// The WhatsApp ID for a phone number: `447700900001` →
/// `447700900001@s.whatsapp.net`.
pub(crate) fn jid(phone: &str) -> String {
    format!("{phone}@s.whatsapp.net")
}

/// Someone as the webhook would see them: a name and a phone number, no LID.
pub(crate) fn person(name: &str, phone: &str) -> Person {
    Person {
        name: name.to_string(),
        jid: jid(phone),
        lid: None,
    }
}

/// Registers `person(name, phone)` at `now`, as their first message would,
/// and returns their row. Both `unwrap`s are safe in a test: the first is
/// the database working, the second is "they have a phone number", which
/// `person` always gives them.
pub(crate) async fn register(
    conn: &mut SqliteConnection,
    name: &str,
    phone: &str,
    now: DateTime<Utc>,
) -> User {
    users::find_or_register(conn, &person(name, phone), now)
        .await
        .unwrap()
        .unwrap()
}

// ---- The app on a free port ----------------------------------------------

/// Runs the real router, on a free port with `pool` as its database, and
/// returns its address. For tests of anything under `/api`. The login is
/// `test:test`, and GOWA is a fake (`fake_gowa`) that says yes to everything.
pub(crate) async fn start_server_with(pool: sqlx::SqlitePool) -> String {
    serve_for_test(pool, None).await
}

/// The same, with town lookups going to a fake geocoder at `geocoding_url`.
pub(crate) async fn start_server_with_geocoder(
    pool: sqlx::SqlitePool,
    geocoding_url: &str,
) -> String {
    serve_for_test(pool, Some(geocoding_url)).await
}

/// The app's state for tests: `pool` as its database, GOWA at `gowa_url`
/// (usually a `fake_gowa`'s `url`), login `test:test`, webhook secret
/// `test`, Milton Keynes defaults, clearing after 14 days.
pub(crate) fn test_state(pool: sqlx::SqlitePool, gowa_url: &str) -> Arc<AppState> {
    use crate::config::Config;
    use crate::settings::Settings;

    let config = Config {
        port: 0,
        webhook_secret: "test".to_string(),
        gowa_url: gowa_url.to_string(),
        gowa_basic_auth: "test:test".to_string(),
        database_url: "sqlite::memory:".to_string(),
        inactive_days: 14,
        backup_dir: None,
        backup_keep: 7,
        push: None,
        defaults: Settings {
            timezone: chrono_tz::Europe::London,
            city: "Milton Keynes".to_string(),
            country: "GB".to_string(),
            coordinates: None,
            prayer_method: 3,
            prayer_school: 0,
            prayer_ending_minutes: 20,
        },
    };
    AppState::new(config, pool)
}

/// Serves the real app for `state` on a free port; returns its address.
pub(crate) async fn serve_state(state: Arc<AppState>) -> String {
    serve(crate::app(state)).await
}

async fn serve_for_test(pool: sqlx::SqlitePool, geocoding_url: Option<&str>) -> String {
    // A fake GOWA that answers at once. An address nothing listens on (like
    // `http://localhost:1`) works too, but Windows takes about 2 seconds to
    // give up on each connection, which made the tests slow. Nobody looks at
    // what this one records, so its handle is dropped; the server itself
    // keeps running, because `tokio::spawn` owns it.
    let gowa = fake_gowa().await;
    let mut state = test_state(pool, &gowa.url);
    if let Some(url) = geocoding_url {
        // `Arc::get_mut` gives a `&mut` only while nothing else shares the
        // `Arc`, which is still true here, before the server starts.
        Arc::get_mut(&mut state).expect("not shared yet").apis.geo =
            crate::geo::Geo::with_geocoding_url(url);
    }
    serve_state(state).await
}

/// Serves any router on a free port in the background; returns its address.
async fn serve(app: Router) -> String {
    // Port 0 asks the operating system for any free port.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

// ---- Fake outside services ------------------------------------------------

/// A fake GOWA for tests, on a free port: remembers each message the bot
/// sends as (to, text) and each group it leaves, says yes to reactions
/// (without keeping them; no test needs them yet); lists the groups in
/// `listed`; refuses to leave a group (with a 500, as a broken GOWA would)
/// while `refuse_leave` is set; and refuses to send while `fail_sends` is
/// set. A refused message is still kept in `sent`, so a test can see the
/// bot tried.
///
/// Each field is an `Arc<Mutex<...>>`: the `Arc` lets the test and the
/// fake's handlers share one value, and the `Mutex` lets either change it.
/// A plain `std::sync::Mutex` is fine here, because nobody holds the lock
/// across an `.await`.
pub(crate) struct FakeGowa {
    pub(crate) url: String,
    pub(crate) sent: Arc<Mutex<Vec<(String, String)>>>,
    pub(crate) listed: Arc<Mutex<Vec<String>>>,
    pub(crate) left: Arc<Mutex<Vec<String>>>,
    pub(crate) refuse_leave: Arc<Mutex<bool>>,
    pub(crate) fail_sends: Arc<Mutex<bool>>,
}

/// Starts a `FakeGowa`. It answers the paths `crate::whatsapp` calls, with
/// the same reply shapes as the real GOWA.
pub(crate) async fn fake_gowa() -> FakeGowa {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let listed = Arc::new(Mutex::new(Vec::<String>::new()));
    let left = Arc::new(Mutex::new(Vec::new()));
    let refuse_leave = Arc::new(Mutex::new(false));
    let fail_sends = Arc::new(Mutex::new(false));
    // Each handler gets its own clone of the `Arc`s it needs: `move`
    // closures take ownership, and the originals go back to the test.
    let (log, fail, groups, leaves, refuse) = (
        sent.clone(),
        fail_sends.clone(),
        listed.clone(),
        left.clone(),
        refuse_leave.clone(),
    );
    let app = Router::new()
        .route(
            "/send/message",
            post(move |Json(body): Json<Value>| async move {
                let to = body["phone"].as_str().unwrap_or_default().to_string();
                let text = body["message"].as_str().unwrap_or_default().to_string();
                log.lock().unwrap().push((to, text));
                if *fail.lock().unwrap() {
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({})));
                }
                (
                    StatusCode::OK,
                    Json(json!({ "code": "SUCCESS", "results": { "message_id": "FAKE1" } })),
                )
            }),
        )
        .route(
            "/message/{id}/reaction",
            post(|| async { Json(json!({ "code": "SUCCESS" })) }),
        )
        .route(
            "/user/my/groups",
            get(move || async move {
                let data: Vec<Value> = groups
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|jid| json!({ "JID": jid, "Name": "Family" }))
                    .collect();
                Json(json!({ "code": "SUCCESS", "results": { "data": data } }))
            }),
        )
        .route(
            "/group/leave",
            post(move |Json(body): Json<Value>| async move {
                if *refuse.lock().unwrap() {
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({})));
                }
                let group = body["group_id"].as_str().unwrap_or_default().to_string();
                leaves.lock().unwrap().push(group);
                (StatusCode::OK, Json(json!({ "code": "SUCCESS" })))
            }),
        );
    FakeGowa {
        url: serve(app).await,
        sent,
        listed,
        left,
        refuse_leave,
        fail_sends,
    }
}

/// A fake Open-Meteo geocoder that only knows Leicester. Returns its
/// base URL.
pub(crate) async fn fake_geocoder() -> String {
    use axum::extract::Query;

    async fn search(Query(q): Query<Vec<(String, String)>>) -> Json<Value> {
        let name = q.iter().find(|(k, _)| k == "name").map(|(_, v)| v.as_str());
        if name.is_some_and(|n| n.eq_ignore_ascii_case("leicester")) {
            Json(json!({ "results": [{
                "name": "Leicester", "country_code": "gb",
                "latitude": 52.6369, "longitude": -1.1398,
                "timezone": "Europe/London"
            }] }))
        } else {
            Json(json!({}))
        }
    }
    serve(Router::new().route("/search", get(search))).await
}

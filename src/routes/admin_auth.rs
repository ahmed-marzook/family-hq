//! The admin dashboard's password, and its guard against other websites.
//!
//! The login is GOWA's own `user:password` (`GOWA_BASIC_AUTH`), so there's
//! one login to set up rather than two. The browser asks for it with its
//! built-in sign-in box (HTTP Basic auth), so the dashboard needs no login
//! page or sessions.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::AppState;

/// What the browser's sign-in box shows.
const CHALLENGE: &str = r#"Basic realm="Family HQ Admin", charset="UTF-8""#;

/// Puts both guards in front of every route in `router`, including its
/// fallback.
///
/// Layers wrap a router like an onion: the one added *last* is the
/// outermost, so it runs first. Here the password is checked before
/// anything else.
pub fn protect(router: Router<Arc<AppState>>, state: Arc<AppState>) -> Router<Arc<AppState>> {
    router
        .layer(axum::middleware::from_fn(require_json_for_changes))
        .layer(axum::middleware::from_fn_with_state(
            state,
            require_password,
        ))
}

/// Middleware: lets the request through only with the right password.
async fn require_password(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let header = request
        .headers()
        .get(AUTHORIZATION)
        // `to_str` fails on bytes that aren't visible ASCII; that's not a
        // valid login either.
        .and_then(|value| value.to_str().ok());

    if !credentials_match(header, &state.config.gowa_basic_auth) {
        // The `WWW-Authenticate` header is what makes the browser show its
        // sign-in box.
        return (
            StatusCode::UNAUTHORIZED,
            [(WWW_AUTHENTICATE, CHALLENGE)],
            Json(json!({ "error": "password needed" })),
        )
            .into_response();
    }
    next.run(request).await
}

/// Middleware: anything that changes data must send JSON.
///
/// Browsers attach a saved Basic auth login to *every* request to the site,
/// even one started by another website. That site can make the browser
/// send a plain form, but not JSON (the browser would ask this server
/// first, and get no permission). So requiring JSON stops those requests.
async fn require_json_for_changes(request: Request, next: Next) -> Response {
    let reads_only = matches!(*request.method(), Method::GET | Method::HEAD);
    if !reads_only && !is_json(request.headers()) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(json!({ "error": "send JSON" })),
        )
            .into_response();
    }
    next.run(request).await
}

/// `true` if the request says its body is JSON (`application/json`,
/// optionally with `; charset=utf-8`).
fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"))
}

/// `true` if `header` is `Basic <base64 of user:password>` and that equals
/// `expected` (`user:password`).
pub fn credentials_match(header: Option<&str>, expected: &str) -> bool {
    let Some((scheme, encoded)) = header.and_then(|h| h.split_once(' ')) else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("basic") {
        return false;
    }
    let Ok(given) = STANDARD.decode(encoded.trim()) else {
        return false;
    };
    // Compares the SHA-256 of each rather than the logins themselves. `==`
    // stops at the first byte that differs, so its timing could hint how
    // much of a guess was right; on hashes, that says nothing useful.
    Sha256::digest(&given) == Sha256::digest(expected.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basic(login: &str) -> String {
        format!("Basic {}", STANDARD.encode(login))
    }

    #[test]
    fn right_login_matches() {
        assert!(credentials_match(
            Some(&basic("admin:s3cret")),
            "admin:s3cret"
        ));
    }

    #[test]
    fn password_with_a_colon_matches() {
        assert!(credentials_match(
            Some(&basic("admin:pa:ss")),
            "admin:pa:ss"
        ));
    }

    #[test]
    fn scheme_ignores_case() {
        let header = basic("admin:s3cret").replace("Basic", "basic");
        assert!(credentials_match(Some(&header), "admin:s3cret"));
    }

    #[test]
    fn wrong_or_missing_logins_dont_match() {
        let expected = "admin:s3cret";
        assert!(!credentials_match(None, expected));
        assert!(!credentials_match(Some(""), expected));
        assert!(!credentials_match(Some(&basic("admin:wrong")), expected));
        assert!(!credentials_match(Some(&basic("other:s3cret")), expected));
        assert!(!credentials_match(Some(&basic("admin:s3cret ")), expected));
        assert!(!credentials_match(Some(&basic("")), expected));
        assert!(!credentials_match(Some("Basic not-base64!"), expected));
        assert!(!credentials_match(Some("Basic"), expected));
        // The right login, but not as Basic auth.
        let bearer = basic(expected).replace("Basic", "Bearer");
        assert!(!credentials_match(Some(&bearer), expected));
    }
}

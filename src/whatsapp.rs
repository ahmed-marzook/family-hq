//! A small client for GOWA's REST API: the bot's way of *doing* things in
//! WhatsApp (the webhook is how it *hears* things).

use serde_json::json;

use crate::error::AppError;
use crate::reminders::engine::Messenger;

/// Reaction the bot puts on a message it understood as a command.
pub const ACK: &str = "👍";

/// Talks to one GOWA server.
#[derive(Debug, Clone)]
pub struct Gowa {
    http: reqwest::Client,
    /// e.g. `http://gowa:3000` inside Compose, `http://localhost:3001` from
    /// `cargo run`.
    base_url: String,
    user: String,
    password: String,
}

impl Gowa {
    /// `basic_auth` is GOWA's `user:password`, the same as `GOWA_BASIC_AUTH`.
    pub fn new(base_url: &str, basic_auth: &str) -> Self {
        Gowa::with_client(base_url, basic_auth, crate::http::client())
    }

    /// For tests: the same, but giving up on a request after `timeout`, so
    /// a test of a GOWA that never answers doesn't wait the full 15 seconds.
    #[cfg(test)]
    pub(crate) fn with_timeout(
        base_url: &str,
        basic_auth: &str,
        timeout: std::time::Duration,
    ) -> Self {
        Gowa::with_client(base_url, basic_auth, crate::http::client_with(timeout))
    }

    /// Builds a `Gowa` around an HTTP client made elsewhere, so `new` and
    /// the test constructor share the rest.
    fn with_client(base_url: &str, basic_auth: &str, http: reqwest::Client) -> Self {
        // "admin:pa:ss" → ("admin", "pa:ss"): only the first `:` splits.
        let (user, password) = basic_auth.split_once(':').unwrap_or((basic_auth, ""));

        Gowa {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            user: user.to_string(),
            password: password.to_string(),
        }
    }

    /// Reacts to `message_id` in `chat_id` with `emoji`.
    pub async fn react(
        &self,
        chat_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), reqwest::Error> {
        self.http
            .post(format!("{}/message/{message_id}/reaction", self.base_url))
            .basic_auth(&self.user, Some(&self.password))
            .json(&json!({ "phone": chat_id, "emoji": emoji }))
            .send()
            .await?
            // `send` only fails if GOWA couldn't be reached. A 401 or 500
            // is still a "successful" HTTP exchange, so turn those into
            // errors too.
            .error_for_status()?;
        Ok(())
    }

    /// Sends `text` to `chat_id`, as a quoted reply to `reply_to` if given.
    /// Returns the new message's WhatsApp ID (needed to save list numbers).
    ///
    /// `mentions` are phone numbers (`447700900001`) to @mention. Write
    /// `@447700900001` in `text` where each should appear; WhatsApp then
    /// shows it as the person's name and notifies them.
    pub async fn send_text(
        &self,
        chat_id: &str,
        text: &str,
        reply_to: Option<&str>,
        mentions: &[String],
    ) -> Result<String, reqwest::Error> {
        let mut body = json!({ "phone": chat_id, "message": text });
        if let Some(id) = reply_to {
            body["reply_message_id"] = json!(id);
        }
        if !mentions.is_empty() {
            body["mentions"] = json!(mentions);
        }

        let response: SendResponse = self
            .http
            .post(format!("{}/send/message", self.base_url))
            .basic_auth(&self.user, Some(&self.password))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            // Parses the JSON reply into `SendResponse`; a reply without
            // `results.message_id` is an error too.
            .json()
            .await?;
        Ok(response.results.message_id)
    }

    /// Makes the bot leave a group. It can only come back if someone in the
    /// group adds its number again.
    pub async fn leave_group(&self, group_id: &str) -> Result<(), reqwest::Error> {
        self.http
            .post(format!("{}/group/leave", self.base_url))
            .basic_auth(&self.user, Some(&self.password))
            .json(&json!({ "group_id": group_id }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// The groups the bot is in, with their names (`/user/my/groups`).
    ///
    /// Gives up after a few seconds, because the admin dashboard waits on
    /// it and the names are only nice to have.
    pub async fn groups(&self) -> Result<Vec<Group>, reqwest::Error> {
        let response: GroupsResponse = self
            .http
            .get(format!("{}/user/my/groups", self.base_url))
            .basic_auth(&self.user, Some(&self.password))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(response.results.data)
    }
}

/// How the reminder loop's engine sends: through `send_text` above.
impl Messenger for Gowa {
    async fn send_text(
        &self,
        chat_id: &str,
        text: &str,
        reply_to: Option<&str>,
        mentions: &[String],
    ) -> Result<String, AppError> {
        Ok(Gowa::send_text(self, chat_id, text, reply_to, mentions).await?)
    }
}

/// One group from `/user/my/groups`. GOWA sends many more fields.
#[derive(Debug, PartialEq, serde::Deserialize)]
pub struct Group {
    /// The group's chat ID, e.g. `120363000000000001@g.us`.
    #[serde(rename = "JID")]
    pub jid: String,
    #[serde(rename = "Name")]
    pub name: String,
}

/// `{"code":"SUCCESS","results":{"data":[{"JID":"...","Name":"..."}]}}`.
#[derive(serde::Deserialize)]
struct GroupsResponse {
    results: GroupsResults,
}

#[derive(serde::Deserialize)]
struct GroupsResults {
    data: Vec<Group>,
}

/// GOWA's reply to `/send/message`:
/// `{"code":"SUCCESS","results":{"message_id":"3EB0...","status":"..."}}`.
/// Only the field we need; serde ignores the rest.
#[derive(serde::Deserialize)]
struct SendResponse {
    results: SendResults,
}

#[derive(serde::Deserialize)]
struct SendResults {
    message_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_group_names_from_gowa() {
        // Trimmed from a real v9.7.0 reply, with a fake ID; the extra
        // fields are there to show serde skips what it wasn't asked for.
        let body = r#"{"code":"SUCCESS","message":"Success get list groups","results":{"data":[
            {"JID":"120363000000000001@g.us","Name":"Family","Topic":"","Participants":[],"IsLocked":false}
        ]}}"#;
        let response: GroupsResponse = serde_json::from_str(body).unwrap();
        assert_eq!(
            response.results.data,
            [Group {
                jid: "120363000000000001@g.us".to_string(),
                name: "Family".to_string(),
            }]
        );
    }

    /// A GOWA that accepts the connection but never replies (hung, or
    /// stuck behind a dead proxy) must not stall the caller forever: the
    /// reminder loop awaits every send in turn, so one hang stops them all.
    #[tokio::test]
    async fn gives_up_on_a_gowa_that_never_answers() {
        use std::time::Duration;

        // Port 0 asks the OS for any free port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        // Accept connections and keep them open without ever answering.
        // The sockets are kept in a `Vec` so they aren't closed (dropping a
        // socket closes it, which reqwest would see as an error at once).
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });

        let gowa = Gowa::with_timeout(&url, "user:pass", Duration::from_millis(300));
        // The outer timeout only stops the test hanging if the client has
        // none of its own; it should never be the one that fires.
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            gowa.send_text("447700900001@s.whatsapp.net", "hi", None, &[]),
        )
        .await
        .expect("send_text should give up on its own, long before 10 seconds");
        let err = result.expect_err("a GOWA that never answers is an error");
        assert!(err.is_timeout(), "expected a timeout, got {err:?}");
    }
}

//! A small client for GOWA's REST API: the bot's way of *doing* things in
//! WhatsApp (the webhook is how it *hears* things).

use serde_json::json;

/// Reaction the bot puts on a message it understood as a command.
pub const ACK: &str = "👍";

/// Talks to one GOWA server.
///
/// `#[derive(Clone)]` is cheap here: `reqwest::Client` is an `Arc` inside,
/// so clones share one connection pool rather than opening new ones.
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
        // "admin:pa:ss" → ("admin", "pa:ss"): only the first `:` splits.
        let (user, password) = basic_auth.split_once(':').unwrap_or((basic_auth, ""));

        Gowa {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            user: user.to_string(),
            password: password.to_string(),
        }
    }

    /// Reacts to `message_id` in `chat_id` with `emoji`.
    ///
    /// `async` because it waits on the network. Each `.await` hands the
    /// thread back to Tokio while the request is in flight, so other
    /// webhooks keep being served in the meantime.
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

//! `POST /webhook/whatsapp`: every WhatsApp event GOWA sees arrives here.
//!
//! The order matters:
//! 1. Check the signature against the *raw bytes* of the body.
//! 2. Only then parse the JSON.
//! 3. Decide whether the event needs any action.

use std::sync::Arc;

use axum::{body::Bytes, extract::State, http::HeaderMap, http::StatusCode};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha256;

use crate::AppState;
use crate::parser::{self, BotCommand};
use crate::person::{Assignee, Person};
use crate::whatsapp;

/// HMAC using SHA-256 as its hash. A type alias just gives a long type a
/// short name.
type HmacSha256 = Hmac<Sha256>;

/// The JSON GOWA sends, e.g.
/// `{"event":"message","payload":{...},"device_id":"...","session_id":"..."}`.
///
/// `tag` and `content` make this an *adjacently tagged* enum: serde reads
/// `"event"` to pick the variant, then parses `"payload"` into that
/// variant's struct. Fields not mentioned (`device_id`, `session_id`) are
/// ignored. Any other `"event"` value is a parse error, which the handler
/// treats as "not for us".
#[derive(Debug, Deserialize)]
#[serde(tag = "event", content = "payload")]
pub enum WebhookEvent {
    #[serde(rename = "message")]
    Message(Message),
    #[serde(rename = "message.reaction")]
    Reaction(Reaction),
}

/// A text, sticker, image, ... message. Only the fields we use so far.
#[derive(Debug, Deserialize)]
pub struct Message {
    /// WhatsApp's message ID, unique per message. The dedupe key later.
    pub id: String,
    /// Where it was sent: ends in `@g.us` for a group,
    /// `@s.whatsapp.net` for a DM.
    pub chat_id: String,
    /// Who sent it, e.g. `447700900123@s.whatsapp.net`.
    pub from: String,
    /// The same sender as a LID, e.g. `100000000000001@lid`.
    pub from_lid: Option<String>,
    /// The sender's WhatsApp profile name, e.g. "Bilal".
    pub from_name: Option<String>,
    /// The text. `Option` because stickers and media have no `body`.
    pub body: Option<String>,
    /// `true` for messages the bot itself sent. `#[serde(default)]` means
    /// "use `false` if the field is missing".
    #[serde(default)]
    pub is_from_me: bool,
    pub timestamp: String,
}

impl Message {
    /// Who sent this message, as a `Person`.
    ///
    /// `&self` borrows the message, so the fields are cloned into the new
    /// `Person`, leaving the message usable afterwards.
    pub fn sender(&self) -> Person {
        Person {
            // No profile name (rare): fall back to the phone-number ID.
            name: self.from_name.clone().unwrap_or_else(|| self.from.clone()),
            jid: self.from.clone(),
            lid: self.from_lid.clone(),
        }
    }
}

/// Someone reacted to a message with an emoji.
#[derive(Debug, Deserialize)]
pub struct Reaction {
    pub id: String,
    pub chat_id: String,
    pub from: String,
    /// The emoji. Empty when a reaction is removed.
    pub reaction: String,
    /// The message that was reacted to.
    pub reacted_message_id: String,
    #[serde(default)]
    pub is_from_me: bool,
}

/// The webhook handler.
///
/// Each argument is an *extractor*: Axum looks at the type and pulls that
/// piece out of the request. `State` is the shared `AppState`, `HeaderMap`
/// all the headers, and `Bytes` the body exactly as sent. We want raw
/// bytes, not `Json<...>`, because the signature covers the exact bytes and
/// re-serialising parsed JSON could change spacing or key order.
///
/// Returning a `StatusCode` gives an empty response with that status.
pub async fn whatsapp(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let config = &state.config;

    // `headers.get` gives an `Option<&HeaderValue>`; `to_str` fails on
    // non-ASCII, and `.ok()` turns that failure into `None`.
    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|value| value.to_str().ok());

    if !signature_is_valid(config.webhook_secret.as_bytes(), &body, signature) {
        println!("webhook: rejected, bad or missing signature");
        return StatusCode::UNAUTHORIZED;
    }

    // The sender is genuine, but the body might still be an event type or
    // shape we don't know. Answer 200 anyway: a 4xx/5xx would make GOWA
    // retry a body that will never parse.
    let event: WebhookEvent = match serde_json::from_slice(&body) {
        Ok(event) => event,
        Err(err) => {
            println!("webhook: ignored, unknown event or unreadable body: {err}");
            return StatusCode::OK;
        }
    };

    handle_event(&state, event).await;
    StatusCode::OK
}

/// Decides what, if anything, to do with an event. For now it logs and
/// acknowledges commands; later stages put the database and replies here.
///
/// `async` because acknowledging means calling GOWA over the network.
async fn handle_event(state: &AppState, event: WebhookEvent) {
    match event {
        // The bot's own messages come back through the webhook too.
        // Reacting to them could make the bot answer itself forever.
        WebhookEvent::Message(msg) if msg.is_from_me => {
            println!("webhook: skipped own message {}", msg.id);
        }
        WebhookEvent::Message(msg) => match msg.body.as_deref().map(parser::parse) {
            Some(Some(command)) => {
                let command = assign_to_sender(command, msg.sender());
                println!(
                    "webhook: message {} from {} in {}: {command:?}",
                    msg.id, msg.from, msg.chat_id
                );

                // Thumbs-up on the message so the sender knows it was seen.
                // A failed reaction is logged, not fatal: the command still
                // counts, and returning an error would make GOWA resend it.
                if let Err(err) = state.gowa.react(&msg.chat_id, &msg.id, whatsapp::ACK).await {
                    println!("webhook: couldn't react to {}: {err}", msg.id);
                }
            }
            Some(None) => println!("webhook: message {} is chat, not a command", msg.id),
            None => println!("webhook: skipped message {} with no text", msg.id),
        },
        WebhookEvent::Reaction(r) if r.is_from_me => {
            println!("webhook: skipped own reaction {}", r.id);
        }
        WebhookEvent::Reaction(r) => println!(
            "webhook: {} reacted {:?} to {} in {}",
            r.from, r.reaction, r.reacted_message_id, r.chat_id
        ),
    }
}

/// A `+todo` without `@name` is for whoever sent it.
///
/// Lives here rather than in the parser because the parser only sees the
/// text, not who sent it. `command` and `sender` are taken by value (moved
/// in), so nothing is cloned: unchanged commands just pass through.
fn assign_to_sender(command: BotCommand, sender: Person) -> BotCommand {
    match command {
        BotCommand::AddTodo {
            title,
            assignee: None,
        } => BotCommand::AddTodo {
            title,
            assignee: Some(Assignee::Sender(sender)),
        },
        other => other,
    }
}

/// `true` if `header` is `sha256=<hex>` and the hex is the HMAC-SHA256 of
/// `body` keyed with `secret`.
///
/// A pure function (bytes in, bool out), so it's easy to test without a
/// server. `let ... else` binds the value if the pattern matches and runs
/// the `else` block (which must return) if it doesn't.
pub fn signature_is_valid(secret: &[u8], body: &[u8], header: Option<&str>) -> bool {
    let Some(hex_signature) = header.and_then(|h| h.strip_prefix("sha256=")) else {
        return false;
    };
    let Ok(signature) = hex::decode(hex_signature) else {
        return false;
    };

    // HMAC accepts a key of any length, so this can't fail.
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC takes any key length");
    mac.update(body);
    // `verify_slice` compares in constant time. A plain `==` stops at the
    // first wrong byte, and that timing difference can leak the answer.
    mac.verify_slice(&signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"test-secret";

    // Shapes copied from real GOWA v9.7.0 webhooks, with fake numbers.
    const TEXT_DM: &str = r#"{"device_id":"447700900001@s.whatsapp.net","event":"message","payload":{"body":"+shop milk","chat_id":"447700900002@s.whatsapp.net","chat_lid":"111@lid","from":"447700900002@s.whatsapp.net","from_lid":"111@lid","from_name":"Test","id":"AC0000000000000000000000000000A1","is_from_me":false,"sender_display_name":"Test","timestamp":"2026-10-27T14:40:30Z"},"session_id":"s1"}"#;
    const STICKER: &str = r#"{"device_id":"447700900001@s.whatsapp.net","event":"message","payload":{"chat_id":"447700900002@s.whatsapp.net","from":"447700900002@s.whatsapp.net","id":"AC0000000000000000000000000000A2","is_from_me":false,"sticker":{"url":"https://example.com/x"},"timestamp":"2026-10-27T14:40:34Z"},"session_id":"s1"}"#;
    const REACTION: &str = r#"{"device_id":"447700900001@s.whatsapp.net","event":"message.reaction","payload":{"chat_id":"447700900002@s.whatsapp.net","from":"447700900002@s.whatsapp.net","id":"AC0000000000000000000000000000A3","is_from_me":false,"reacted_message_id":"3EB007CD8AB557CCBD3054","reaction":"❤️","timestamp":"2026-10-27T14:40:42Z"},"session_id":"s1"}"#;

    /// Signs `body` the way GOWA does.
    fn sign(secret: &[u8], body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn correct_signature_is_valid() {
        let header = sign(SECRET, TEXT_DM.as_bytes());
        assert!(signature_is_valid(
            SECRET,
            TEXT_DM.as_bytes(),
            Some(&header)
        ));
    }

    #[test]
    fn wrong_secret_is_invalid() {
        let header = sign(b"other-secret", TEXT_DM.as_bytes());
        assert!(!signature_is_valid(
            SECRET,
            TEXT_DM.as_bytes(),
            Some(&header)
        ));
    }

    #[test]
    fn changed_body_is_invalid() {
        let header = sign(SECRET, TEXT_DM.as_bytes());
        let tampered = TEXT_DM.replace("milk", "beer");
        assert!(!signature_is_valid(
            SECRET,
            tampered.as_bytes(),
            Some(&header)
        ));
    }

    #[test]
    fn missing_or_malformed_header_is_invalid() {
        let body = TEXT_DM.as_bytes();
        let hex_only = sign(SECRET, body).replace("sha256=", "");
        assert!(!signature_is_valid(SECRET, body, None));
        assert!(!signature_is_valid(SECRET, body, Some("")));
        assert!(!signature_is_valid(SECRET, body, Some(&hex_only)));
        assert!(!signature_is_valid(SECRET, body, Some("sha256=not-hex")));
    }

    #[test]
    fn parses_text_message() {
        let event: WebhookEvent = serde_json::from_str(TEXT_DM).unwrap();
        let WebhookEvent::Message(msg) = event else {
            panic!("expected a message, got {event:?}");
        };
        assert_eq!(msg.body.as_deref(), Some("+shop milk"));
        assert_eq!(msg.id, "AC0000000000000000000000000000A1");
        assert!(!msg.is_from_me);
    }

    #[test]
    fn sticker_has_no_body() {
        let event: WebhookEvent = serde_json::from_str(STICKER).unwrap();
        let WebhookEvent::Message(msg) = event else {
            panic!("expected a message, got {event:?}");
        };
        assert_eq!(msg.body, None);
    }

    #[test]
    fn parses_reaction() {
        let event: WebhookEvent = serde_json::from_str(REACTION).unwrap();
        let WebhookEvent::Reaction(r) = event else {
            panic!("expected a reaction, got {event:?}");
        };
        assert_eq!(r.reaction, "❤️");
        assert_eq!(r.reacted_message_id, "3EB007CD8AB557CCBD3054");
    }

    fn bilal() -> Person {
        Person {
            name: "Bilal".to_string(),
            jid: "447700900002@s.whatsapp.net".to_string(),
            lid: Some("111@lid".to_string()),
        }
    }

    #[test]
    fn sender_is_built_from_the_message() {
        let event: WebhookEvent = serde_json::from_str(TEXT_DM).unwrap();
        let WebhookEvent::Message(msg) = event else {
            panic!("expected a message, got {event:?}");
        };
        assert_eq!(
            msg.sender(),
            Person {
                name: "Test".to_string(),
                jid: "447700900002@s.whatsapp.net".to_string(),
                lid: Some("111@lid".to_string()),
            }
        );
    }

    #[test]
    fn sender_without_profile_name_uses_jid() {
        // The STICKER payload has no from_name or from_lid.
        let event: WebhookEvent = serde_json::from_str(STICKER).unwrap();
        let WebhookEvent::Message(msg) = event else {
            panic!("expected a message, got {event:?}");
        };
        let sender = msg.sender();
        assert_eq!(sender.name, "447700900002@s.whatsapp.net");
        assert_eq!(sender.lid, None);
    }

    #[test]
    fn todo_without_assignee_goes_to_sender() {
        let command = BotCommand::AddTodo {
            title: "go gym".to_string(),
            assignee: None,
        };
        assert_eq!(
            assign_to_sender(command, bilal()),
            BotCommand::AddTodo {
                title: "go gym".to_string(),
                assignee: Some(Assignee::Sender(bilal())),
            }
        );
    }

    #[test]
    fn mentioned_assignee_is_kept() {
        let khadija = Some(Assignee::Mentioned("Khadija".to_string()));
        let command = BotCommand::AddTodo {
            title: "book dentist".to_string(),
            assignee: khadija.clone(),
        };
        let BotCommand::AddTodo { assignee, .. } = assign_to_sender(command, bilal()) else {
            panic!("expected AddTodo");
        };
        assert_eq!(assignee, khadija);
    }

    #[test]
    fn other_commands_are_unchanged() {
        assert_eq!(
            assign_to_sender(BotCommand::ListShop, bilal()),
            BotCommand::ListShop
        );
    }

    #[test]
    fn unknown_event_does_not_parse() {
        let body = r#"{"event":"group.participants","payload":{"anything":1}}"#;
        assert!(serde_json::from_str::<WebhookEvent>(body).is_err());
    }
}

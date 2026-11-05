//! Push notifications through ntfy (Stage 2.1), for any feature.
//!
//! Anyone can turn push on in their DM with `/push on`. They get their own
//! ntfy topic, a long random name their phone subscribes to in the ntfy
//! app. A feature that wants to reach them describes what to say as a
//! [`Notice`] (a title, a message, and buttons) and hands it to
//! [`send_once`]; only this module knows how ntfy wants it. Prayer
//! reminders are the first feature to use it (`prayer::reminder_notice`).
//!
//! A button is an [`Action`]. Tapping it makes the *phone* send a request
//! to `{PUBLIC_URL}/actions/<the action's path>` (`routes::actions`),
//! carrying that person's token, so the bot knows who tapped it and what
//! for. A new kind of button is a new `Action` variant and one more arm in
//! `routes::actions`.
//!
//! Off unless `NTFY_URL` and `PUBLIC_URL` are set (`config::PushConfig`).
//! The SQL is `repo::push`.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sqlx::SqliteConnection;
use tracing::{info, warn};

use crate::config::PushConfig;
use crate::error::AppError;
use crate::prayer::Prayer;
use crate::repo::push::{self as repo, Subscription};

const BELL: &str = "🔔";

/// Which feature a push is from, for `pushes_sent.feature`. Habits, todos
/// and `+remind` are allowed in the table already, ready to be added here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Prayers,
}

impl Feature {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Feature::Prayers => "prayers",
        }
    }
}

/// What a button does when it's tapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Tick a prayer off for that day, as reacting to its reminder does.
    Prayed { date: NaiveDate, prayer: Prayer },
    /// The button on `/push test`'s push: proves the way back works.
    Test,
}

impl Action {
    /// Where the button sends its request, under `/actions/`:
    /// `prayers/prayed/2026-11-05/asr`, `test`.
    pub fn path(&self) -> String {
        match self {
            Action::Prayed { date, prayer } => {
                format!("prayers/prayed/{date}/{}", prayer.as_db_str())
            }
            Action::Test => "test".to_string(),
        }
    }

    /// The other way: the action a path asks for, or `None` for anything
    /// that isn't one (including a bad date or prayer name).
    pub fn from_path(path: &str) -> Option<Action> {
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        match parts.as_slice() {
            ["prayers", "prayed", date, prayer] => Some(Action::Prayed {
                date: date.parse().ok()?,
                prayer: prayer.parse().ok()?,
            }),
            ["test"] => Some(Action::Test),
            _ => None,
        }
    }
}

/// A button on a notice: what it says, and what it does.
#[derive(Debug, Clone, PartialEq)]
pub struct Button {
    pub label: String,
    pub action: Action,
}

/// What a feature wants to tell someone. Nothing ntfy-specific, so a
/// feature only says *what*; `Ntfy::message` deals with *how*.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub title: String,
    pub message: String,
    /// Emoji short names ntfy shows in front of the title ("mosque" → 🕌).
    pub tags: Vec<&'static str>,
    /// 1 (min) to 5 (max); 3 is normal. 4 makes a sound even if the phone
    /// would otherwise keep it quiet.
    pub priority: u8,
    pub buttons: Vec<Button>,
}

/// One push, in the JSON shape ntfy's publish API takes
/// (<https://docs.ntfy.sh/publish/#publish-as-json>).
///
/// `#[derive(Serialize)]` writes the JSON for us; `skip_serializing_if`
/// leaves a field out rather than sending `[]`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Message {
    topic: String,
    title: String,
    message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<&'static str>,
    priority: u8,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    actions: Vec<HttpAction>,
}

/// A button in ntfy's terms. `"action": "http"` means the phone sends this
/// request itself when it's tapped, without opening anything.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct HttpAction {
    action: &'static str,
    label: String,
    url: String,
    method: &'static str,
    /// Sent with the request. The person's token goes here rather than in
    /// the URL, so it doesn't end up in Cloudflare's request logs.
    headers: Headers,
    /// Dismiss the notification once the button worked.
    clear: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct Headers {
    #[serde(rename = "Authorization")]
    authorization: String,
}

/// Talks to one ntfy server.
#[derive(Debug, Clone)]
pub struct Ntfy {
    http: reqwest::Client,
    config: PushConfig,
}

impl Ntfy {
    pub fn new(config: PushConfig) -> Self {
        Ntfy {
            http: reqwest::Client::new(),
            config,
        }
    }

    pub fn config(&self) -> &PushConfig {
        &self.config
    }

    /// `notice` as ntfy's JSON, for `subscription`'s topic, with its buttons
    /// pointing back at `{PUBLIC_URL}/actions/...` as that person.
    pub fn message(&self, subscription: &Subscription, notice: &Notice) -> Message {
        let base = self.config.public_url.trim_end_matches('/');
        Message {
            topic: subscription.topic.clone(),
            title: notice.title.clone(),
            message: notice.message.clone(),
            tags: notice.tags.clone(),
            priority: notice.priority,
            actions: notice
                .buttons
                .iter()
                .map(|b| HttpAction {
                    action: "http",
                    label: b.label.clone(),
                    url: format!("{base}/actions/{}", b.action.path()),
                    method: "POST",
                    headers: Headers {
                        authorization: format!("Bearer {}", subscription.token),
                    },
                    clear: true,
                })
                .collect(),
        }
    }

    /// Sends one push. JSON goes to the server's root, with the topic in
    /// the body.
    pub async fn publish(&self, message: &Message) -> Result<(), reqwest::Error> {
        let mut request = self
            .http
            .post(self.config.ntfy_url.trim_end_matches('/'))
            .json(message);
        if let Some(token) = &self.config.ntfy_token {
            request = request.bearer_auth(token);
        }
        request.send().await?.error_for_status()?;
        Ok(())
    }
}

/// Sends `notice` to `user_id` if they have push on and haven't had
/// `feature`'s push `key` yet: claimed first, so it goes once even if two
/// chats ask (a DM and a group) or the server restarts. A failed send is
/// logged, not returned, and not retried.
pub async fn send_once(
    ntfy: &Ntfy,
    conn: &mut SqliteConnection,
    user_id: i64,
    feature: Feature,
    key: &str,
    notice: &Notice,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    let Some(subscription) = repo::subscription(conn, user_id).await? else {
        return Ok(());
    };
    if !repo::claim(conn, user_id, feature, key, now).await? {
        return Ok(());
    }
    match ntfy.publish(&ntfy.message(&subscription, notice)).await {
        Ok(()) => info!("pushed {} {key} to user {user_id}", feature.as_db_str()),
        Err(err) => warn!(
            "couldn't push {} {key} to user {user_id}: {err}",
            feature.as_db_str()
        ),
    }
    Ok(())
}

/// `bytes` random bytes as hex (twice as many characters), from the
/// operating system's secure random source. Used for topics and tokens,
/// which must be impossible to guess.
fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("the OS random source works");
    hex::encode(buf)
}

/// A new topic and token for someone turning push on. The topic starts
/// `fhq-` so it's recognisable in the ntfy app.
pub fn new_subscription() -> Subscription {
    Subscription {
        topic: format!("fhq-{}", random_hex(16)),
        token: random_hex(32),
    }
}

/// Where to get the ntfy app.
const ANDROID_APP: &str = "https://play.google.com/store/apps/details?id=io.heckel.ntfy";
const IPHONE_APP: &str = "https://apps.apple.com/app/ntfy/id1625396347";

/// The replies to `/push on`: the steps, from installing the ntfy app to
/// checking the button works, then the server and the topic as messages of
/// their own. WhatsApp copies a whole message at a time, so each of those
/// is exactly what goes in the app, with no formatting. Sent again by a
/// second `/push on` (same topic), e.g. for a new phone.
///
/// No `ntfy://` link: WhatsApp only makes http(s) links tappable.
pub fn subscribe_messages(config: &PushConfig, topic: &str) -> Vec<String> {
    let server = config.ntfy_public_url.trim_end_matches('/');
    let steps = format!(
        "{BELL} *Push notifications are on*\n\
         Prayer reminders now also come to your phone through the free *ntfy* app, \
         with a *Prayed ✅* button. WhatsApp reminders carry on as before.\n\n\
         *1. Install ntfy*\n\
         Android: {ANDROID_APP}\n\
         iPhone: {IPHONE_APP}\n\n\
         *2. Subscribe to your topic*\n\
         I'll send the server and your topic next, each on its own so you can \
         copy it (press and hold it, then Copy). In ntfy, tap *+*, then:\n\
         • Topic: paste your topic (the one starting fhq-)\n\
         • Turn on *Use another server* and paste the server\n\
         • Tap *Subscribe*, and allow notifications if it asks.\n\
         On Android, also say yes if it asks to ignore battery optimisation, \
         or pushes can arrive late.\n\n\
         *3. Check it*\n\
         Send `/push test`. A push should arrive within a few seconds. Tap its \
         *Test the button* button (on iPhone, press and hold the notification to see it) \
         and I'll reply here.\n\n\
         Keep the topic to yourself: anyone who has it can read your pushes. \
         `/push off` stops them; `/push on` again later gives you a new topic."
    );
    vec![steps, server.to_string(), topic.to_string()]
}

/// What `/push test` sends: a button that checks the way back.
pub fn test_notice() -> Notice {
    Notice {
        title: "Family HQ push works".to_string(),
        message: "Tap the button to check it can reach the bot too.".to_string(),
        tags: vec!["bell"],
        priority: 3,
        buttons: vec![Button {
            label: "Test the button".to_string(),
            action: Action::Test,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ntfy() -> Ntfy {
        Ntfy::new(PushConfig {
            ntfy_url: "http://ntfy:80".to_string(),
            ntfy_public_url: "https://ntfy.example.co.uk/".to_string(),
            ntfy_token: None,
            public_url: "https://fhq.example.co.uk/".to_string(),
        })
    }

    fn sub() -> Subscription {
        Subscription {
            topic: "fhq-topic".to_string(),
            token: "secret-token".to_string(),
        }
    }

    #[test]
    fn secrets_are_long_and_different() {
        let (a, b) = (new_subscription(), new_subscription());
        assert!(a.topic.starts_with("fhq-") && a.topic.len() == 36);
        assert_eq!(a.token.len(), 64);
        assert_ne!(a.topic, b.topic);
        assert_ne!(a.token, b.token);
    }

    #[test]
    fn actions_round_trip_through_their_paths() {
        let prayed = Action::Prayed {
            date: NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(),
            prayer: Prayer::Asr,
        };
        assert_eq!(prayed.path(), "prayers/prayed/2026-11-05/asr");
        for action in [prayed, Action::Test] {
            assert_eq!(Action::from_path(&action.path()), Some(action));
        }
    }

    #[test]
    fn bad_paths_are_no_action() {
        for path in [
            "",
            "prayers/prayed/2026-11-05",
            "prayers/prayed/2026-13-01/asr",
            "prayers/prayed/2026-11-05/lunch",
            "prayers/skipped/2026-11-05/asr",
            "test/extra",
        ] {
            assert_eq!(Action::from_path(path), None, "{path}");
        }
    }

    #[test]
    fn the_json_is_what_ntfy_expects() {
        let json = serde_json::to_value(ntfy().message(&sub(), &test_notice())).unwrap();
        assert_eq!(json["topic"], "fhq-topic");
        assert_eq!(json["tags"], serde_json::json!(["bell"]));
        let action = &json["actions"][0];
        assert_eq!(action["action"], "http");
        assert_eq!(action["method"], "POST");
        assert_eq!(action["url"], "https://fhq.example.co.uk/actions/test");
        assert_eq!(action["headers"]["Authorization"], "Bearer secret-token");
        assert_eq!(action["clear"], true);
        // The token is only ever in the header.
        assert!(!action["url"].as_str().unwrap().contains("secret-token"));
    }

    #[test]
    fn a_notice_without_buttons_or_tags_leaves_them_out() {
        let notice = Notice {
            title: "t".to_string(),
            message: "m".to_string(),
            tags: vec![],
            priority: 3,
            buttons: vec![],
        };
        let json = serde_json::to_value(ntfy().message(&sub(), &notice)).unwrap();
        assert!(json.get("actions").is_none() && json.get("tags").is_none());
    }

    #[test]
    fn subscribing_gives_the_app_server_and_topic() {
        let messages = subscribe_messages(ntfy().config(), "fhq-topic");
        // A slice pattern: exactly three messages, named in order.
        let [steps, server, topic] = messages.as_slice() else {
            panic!("expected 3 messages, got {messages:?}");
        };
        assert!(steps.contains(ANDROID_APP) && steps.contains(IPHONE_APP));
        // No run of spaces from a line break that lost its `\`.
        assert!(!steps.contains("  "), "{steps}");
        // Exactly what goes in the app: no formatting, and no trailing /.
        assert_eq!(server, "https://ntfy.example.co.uk");
        assert_eq!(topic, "fhq-topic");
    }
}

//! `POST /webhook/whatsapp`: every WhatsApp event GOWA sees arrives here.
//!
//! The order matters:
//! 1. Check the signature against the *raw bytes* of the body.
//! 2. Only then parse the JSON.
//! 3. Decide whether the event needs any action.

use std::sync::Arc;
use std::time::Instant;

use axum::{body::Bytes, extract::State, http::HeaderMap, http::StatusCode};
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha256;
use tracing::{Span, debug, info, warn};

use crate::AppState;
use crate::bot::{self, Context, Offline, Outcome, Prepared, Ran};
use crate::error::AppError;
use crate::metrics::{self, Event, ReactionChange};
use crate::parser::{self, BotCommand, ReplyCommand};
use crate::person::{Assignee, Person};
use crate::prayer::stats::Status;
use crate::prayer::{self, Tick};
use crate::repo::messages::{self, SentList};
use crate::repo::metrics::Logged;
use crate::repo::{self, chats, prayers, users};
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
    /// When this is a (swipe) reply: the ID of the message it quotes.
    pub replied_to_id: Option<String>,
    /// A shared location (📎 → Location). GOWA forwards WhatsApp's own
    /// `LocationMessage`, so the field names are its camelCase ones.
    pub location: Option<SharedLocation>,
    /// The same for a *live* location; its first position is used.
    pub live_location: Option<SharedLocation>,
    /// `true` for messages the bot itself sent. `#[serde(default)]` means
    /// "use `false` if the field is missing".
    #[serde(default)]
    pub is_from_me: bool,
    /// When it was sent, e.g. `2026-10-27T14:40:30Z`. Nothing reads it
    /// yet, but it's a required field, so a payload without it is rejected.
    /// `allow(dead_code)` quiets the "never read" warning on purpose.
    #[allow(dead_code)]
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

/// `{"degreesLatitude":52.04,"degreesLongitude":-0.76,"name":"..."}`.
/// `rename_all = "camelCase"` maps `degrees_latitude` to `degreesLatitude`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedLocation {
    pub degrees_latitude: f64,
    pub degrees_longitude: f64,
}

/// Someone reacted to a message with an emoji.
#[derive(Debug, Deserialize)]
pub struct Reaction {
    pub id: String,
    pub chat_id: String,
    pub from: String,
    pub from_lid: Option<String>,
    pub from_name: Option<String>,
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
        warn!("rejected, bad or missing signature");
        return StatusCode::UNAUTHORIZED;
    }

    // The sender is genuine, but the body might still be an event type or
    // shape we don't know. Answer 200 anyway: a 4xx/5xx would make GOWA
    // retry a body that will never parse.
    let event: WebhookEvent = match serde_json::from_slice(&body) {
        Ok(event) => event,
        Err(err) => {
            warn!("ignored, unknown event or unreadable body: {err}");
            return StatusCode::OK;
        }
    };

    // The clock is read once, here at the edge, and passed down, so
    // everything below can be tested with a fixed time.
    let result = match event {
        WebhookEvent::Message(msg) => handle_message(&state, msg, Utc::now()).await,
        WebhookEvent::Reaction(r) => handle_reaction(&state, r, Utc::now()).await,
    };

    // A database error means nothing was saved (the transaction rolled
    // back), so answer 500 and let GOWA retry. The error itself is already
    // logged, by `instrument(err)` on the handler.
    match result {
        Ok(()) => StatusCode::OK,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// What a message asks the bot to do.
enum Action {
    Command(BotCommand),
    /// A reply to one of the bot's lists: what the list was and showed, and
    /// the reply.
    ListReply(SentList, ReplyCommand),
}

/// An `Action` with its lookups done (`bot::prepare`), ready to run
/// inside the transaction.
///
/// `Box` keeps the prepared command on the heap. It holds whole days of
/// prayer times, so without it every `Ready`, even a small list reply,
/// would take up that much room.
enum Ready {
    Command(Box<Prepared>),
    ListReply(SentList, ReplyCommand),
}

/// Handles one incoming message: register who sent it and where, work out
/// whether it's for the bot, act on it, then react and reply.
///
/// In three steps, so that nothing slow happens while the database is
/// locked for writing (`bot::prepare` says why that matters):
/// 1. register the sender and the chat, in a short transaction of its own;
/// 2. look up what the command needs from outside services, with nothing
///    locked;
/// 3. run the command and mark the message handled, in one transaction:
///    if any of it fails, none of it is saved, and GOWA's retry runs it
///    again.
///
/// Replies go out after that, once everything is saved.
///
/// `#[instrument]` runs the whole function inside a *span* named
/// `message`: every log line from in here, or from anything it calls,
/// carries these fields, e.g. `message{id=3EB0... chat=...@g.us user=Bilal}`.
/// `skip_all` stops it logging every argument (the whole message, the
/// state); the fields are picked by hand instead. `user` starts `Empty` and
/// is filled in once we know who it is. `err` logs a returned `Err` as an
/// error, inside the span.
#[tracing::instrument(
    name = "message",
    skip_all,
    fields(id = %msg.id, chat = %msg.chat_id, user = tracing::field::Empty),
    err
)]
async fn handle_message(
    state: &AppState,
    msg: Message,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    // The bot's own messages come back through the webhook too. Answering
    // them could make the bot talk to itself forever.
    if msg.is_from_me {
        debug!("skipped, the bot's own message");
        return Ok(());
    }
    // `.or(...)`: the first of the two that's present.
    let shared = msg.location.as_ref().or(msg.live_location.as_ref());
    if msg.body.is_none() && shared.is_none() {
        debug!("skipped, no text");
        return Ok(());
    }

    // Step 1: who and where. Dropping `tx` without `commit` rolls it back.
    // `&mut tx` works where a `&mut SqliteConnection` is expected, because
    // a transaction derefs to its connection.
    //
    // `BEGIN IMMEDIATE`, rather than SQLx's `begin()` (a plain, "deferred"
    // `BEGIN`), takes SQLite's write lock as the transaction starts. SQLite
    // has one writer at a time. A deferred transaction that reads first and
    // writes later has to upgrade to the write lock part-way, and if another
    // connection is writing (or has written since we read), SQLite can't
    // safely wait for it: it fails at once with "database is locked". Taking
    // the lock up front means a second writer simply waits its turn (up to
    // SQLx's 5 second busy timeout) and never fails half-way through.
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;

    let sender = msg.sender();
    let Some(user) = users::find_or_register(&mut tx, &sender, now).await? else {
        warn!("can't register {} (no phone number)", msg.from);
        return Ok(());
    };
    Span::current().record("user", user.display_name.as_str());
    // `then_some`: `Some(user.id)` if this isn't a group, else `None`.
    let dm_owner = (!msg.chat_id.ends_with("@g.us")).then_some(user.id);
    let chat_id = chats::find_or_create(&mut tx, &msg.chat_id, dm_owner, now).await?;
    chats::add_member(&mut tx, chat_id, user.id, now).await?;

    let action = match (msg.body.as_deref(), shared) {
        (Some(text), _) => action_for(&mut tx, &msg, text, chat_id, sender).await?,
        // A location shared in a DM sets that person's place. In a group
        // people share locations for other reasons ("I'm here"), so it's
        // left alone there.
        (None, Some(at)) if dm_owner.is_some() => {
            Some(Action::Command(BotCommand::SharedLocation {
                latitude: at.degrees_latitude,
                longitude: at.degrees_longitude,
            }))
        }
        _ => None,
    };
    let is_group = dm_owner.is_none();
    let Some(action) = action else {
        // Using the bot keeps a person's data (see `crate::cleanup`): anything
        // in their DM, or a command (even one it didn't know) or list reply in
        // a group. Ordinary group chat doesn't count.
        let tried_a_command = msg
            .body
            .as_deref()
            .and_then(metrics::command_word)
            .is_some();
        if !is_group || tried_a_command {
            users::touch(&mut tx, user.id, now).await?;
        }
        // The same in a group keeps the group's data, and the bot in it.
        if is_group && tried_a_command {
            chats::touch(&mut tx, chat_id, now).await?;
        }
        // Ordinary chat: keep the registration, nothing else to do. But a
        // `+word` or `/word` the bot didn't understand is worth knowing
        // about: it's what people expect the bot to do.
        if let Some(event) = msg.body.as_deref().and_then(metrics::unknown) {
            let row = Logged {
                event: &event,
                chat_id,
                user_id: user.id,
                is_group,
                ok: false,
                duration_ms: 0,
                at: now,
            };
            repo::metrics::record(&mut tx, &row).await?;
        }
        tx.commit().await?;
        debug!("chat, not a command");
        return Ok(());
    };
    // A command: the registration is saved now, so step 2 holds no lock.
    // Every part of it is safe to repeat, so if the command then fails and
    // GOWA retries, doing it again changes nothing.
    tx.commit().await?;

    let ctx = Context {
        chat_id,
        is_group,
        sender: user,
        now,
    };
    // Named before running, because running uses the command up.
    let event = match &action {
        Action::Command(command) => metrics::command(command),
        Action::ListReply(list, reply) => metrics::list_reply(list.kind, reply),
    };
    let event = match msg.body.as_deref() {
        Some(text) => event.typed_as(text),
        None => event,
    };
    // One line for the command log: whether it worked and how long it took.
    let log_row = |ok: bool, duration_ms: i64| Logged {
        event: &event,
        chat_id,
        user_id: ctx.sender.id,
        is_group,
        ok,
        duration_ms,
        at: now,
    };

    // `Instant` is a monotonic clock, made for measuring how long
    // something took (unlike `Utc::now()`, it never jumps if the system
    // clock is changed).
    let started = Instant::now();
    // Step 2: the lookups, on a plain connection from the pool (not a
    // transaction), which only reads. The block gives it back to the pool
    // at its end, before step 3 asks for one.
    let ready = match action {
        Action::Command(command) => {
            // `?command` logs it with `{:?}`. Only at debug level, because
            // it includes the item text (what's on someone's list).
            debug!(?command, "running");
            let prepared = {
                let mut conn = state.db.acquire().await?;
                bot::prepare(&mut conn, &ctx, &state.apis, command).await
            };
            match prepared {
                Ok(prepared) => Ready::Command(Box::new(prepared)),
                // Only the database fails here (a service being down is
                // an answer, not an error), so this is a 500 and a retry.
                Err(err) => {
                    log_failure(state, &log_row(false, elapsed_ms(started))).await;
                    return Err(err);
                }
            }
        }
        Action::ListReply(list, reply) => {
            debug!(?reply, "list reply");
            Ready::ListReply(list, reply)
        }
    };

    // Step 3: one transaction, for the command and the record that this
    // message was handled.
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    // A retried webhook stops here. Returning drops `tx`, so nothing from
    // this second delivery is saved.
    if !messages::mark_processed(&mut tx, &msg.id, now).await? {
        info!("skipped, already handled (GOWA retried it)");
        return Ok(());
    }
    // Using the bot keeps the person's (and the group's) data: see the
    // ordinary-chat case above.
    users::touch(&mut tx, ctx.sender.id, now).await?;
    if is_group {
        chats::touch(&mut tx, chat_id, now).await?;
    }

    let offline = Offline::of(&state.apis);
    let result = match ready {
        Ready::Command(prepared) => bot::execute(&mut tx, &ctx, &offline, *prepared).await,
        Ready::ListReply(list, reply) => {
            bot::run_list_reply(&mut tx, &ctx, offline.defaults, &list, reply)
                .await
                .map(Ran::Done)
        }
    };
    let duration_ms = elapsed_ms(started);
    // One line per command: which one, whether it worked, how long it took.
    // `%` logs a field with `{}`; a plain name logs it as it is.
    info!(command = %event.name, ok = result.is_ok(), ms = duration_ms, "handled");
    let row = log_row(result.is_ok(), duration_ms);

    let ran = match result {
        Ok(ran) => ran,
        Err(err) => {
            // The transaction rolls back (nothing saved, GOWA retries), so
            // the failure is logged separately. Only best effort: the
            // original error is what matters.
            drop(tx);
            log_failure(state, &row).await;
            return Err(err);
        }
    };
    // Saved with everything else, so a retried webhook isn't counted twice.
    repo::metrics::record(&mut tx, &row).await?;
    tx.commit().await?;

    // Saved. From here on, failures are logged but not returned: a 500
    // would make GOWA retry, and the retry would be skipped as a duplicate.
    // `finish` sends anything that had to wait for the commit (`/push
    // test`'s push).
    let outcome = ran.finish(&state.apis).await;
    react(state, &msg).await;
    match outcome {
        Outcome::Reply(text) => {
            send(state, &msg, &text, &[]).await;
        }
        Outcome::Mentioning { text, mentions } => {
            send(state, &msg, &text, &mentions).await;
        }
        Outcome::Several(texts) => {
            // In order, each waiting for the one before, so they arrive
            // in that order too.
            for text in &texts {
                send(state, &msg, text, &[]).await;
            }
        }
        Outcome::List {
            text,
            kind,
            item_ids,
        } => {
            // Remember what each number in this list means, so replies like
            // `done 2` or `1 3` can find the right rows later.
            if let Some(list_id) = send(state, &msg, &text, &[]).await {
                // Like `log_failure`: a failed `acquire` and a failed save
                // end up as the same `Err`, so one `if let` logs either.
                let saved = match state.db.acquire().await {
                    Ok(mut conn) => {
                        messages::save_list(&mut conn, &list_id, chat_id, kind, &item_ids, now)
                            .await
                    }
                    Err(err) => Err(err),
                };
                if let Err(err) = saved {
                    // The list was sent; only replying to it by number
                    // (`done 2`) won't work.
                    warn!(
                        "couldn't remember list {list_id} in chat {chat_id} (message {}): {err}",
                        msg.id
                    );
                }
            }
        }
    }
    Ok(())
}

/// Milliseconds since `started`. `try_from` because `as_millis` is a u128;
/// anything that doesn't fit an i64 (290 million years) is capped.
fn elapsed_ms(started: Instant) -> i64 {
    i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)
}

/// Records a failed command outside the (rolled back) transaction. Any
/// error here is printed, not returned.
async fn log_failure(state: &AppState, row: &Logged<'_>) {
    let result = match state.db.acquire().await {
        Ok(mut conn) => repo::metrics::record(&mut conn, row).await,
        Err(err) => Err(err),
    };
    if let Err(err) = result {
        warn!("couldn't log a failed {}: {err}", row.event.name);
    }
}

/// Handles a reaction. The only reactions that mean anything are on prayer
/// reminders: any emoji marks that prayer as prayed for whoever reacted,
/// and removing it (an empty `reaction`) unmarks it.
#[tracing::instrument(
    name = "reaction",
    skip_all,
    fields(id = %r.id, chat = %r.chat_id, user = tracing::field::Empty),
    err
)]
async fn handle_reaction(
    state: &AppState,
    r: Reaction,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    if r.is_from_me {
        return Ok(());
    }

    // Immediate, as in `handle_message`: this may write later.
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let Some((date, prayer)) =
        prayers::find_reminder(&mut tx, &r.reacted_message_id, &r.chat_id).await?
    else {
        debug!("{} reacted {:?}, not to a reminder", r.from, r.reaction);
        return Ok(());
    };

    let sender = Person {
        name: r.from_name.clone().unwrap_or_else(|| r.from.clone()),
        jid: r.from.clone(),
        lid: r.from_lid.clone(),
    };
    let Some(user) = users::find_or_register(&mut tx, &sender, now).await? else {
        warn!("can't register {} (no phone number)", r.from);
        return Ok(());
    };
    Span::current().record("user", user.display_name.as_str());
    // Reacting to a reminder is using the bot.
    users::touch(&mut tx, user.id, now).await?;

    // Adding the same reaction twice, or removing one that's already
    // gone, is harmless: both repo calls are safe to repeat, and say
    // whether anything changed. So retried webhooks need no dedupe here,
    // and only a real change gets a reply.
    // An empty emoji means the reaction was taken off.
    let change = if r.reaction.is_empty() {
        ReactionChange::Removed
    } else {
        ReactionChange::Added
    };
    // The chat is already known (the reminder was sent there), so this only
    // looks it up.
    let is_group = r.chat_id.ends_with("@g.us");
    let dm_owner = (!is_group).then_some(user.id);
    let chat_id = chats::find_or_create(&mut tx, &r.chat_id, dm_owner, now).await?;
    if is_group {
        chats::touch(&mut tx, chat_id, now).await?;
    }
    let event: Event = metrics::reaction(change);
    let row = Logged {
        event: &event,
        chat_id,
        user_id: user.id,
        is_group,
        ok: true,
        duration_ms: 0,
        at: now,
    };
    repo::metrics::record(&mut tx, &row).await?;

    // `None` if nothing changed.
    let tick = match change {
        ReactionChange::Removed => {
            let changed = prayers::unmark_prayed(&mut tx, user.id, date, prayer).await?;
            info!("unticked {prayer} on {date}");
            changed.then_some(Tick::Unticked)
        }
        ReactionChange::Added => {
            let status = prayers::mark_prayed(&mut tx, user.id, date, prayer, now).await?;
            info!("prayed {prayer} on {date}");
            // Its time had ended and it was marked missed: made up.
            status.map(|s| match s {
                Status::Late => Tick::MadeUp,
                _ => Tick::Prayed,
            })
        }
    };
    let prayed = prayers::prayed_on(&mut tx, user.id, date).await?.len();
    tx.commit().await?;

    // Swapping 👍 for ❤️ changes nothing, so it gets no second reply.
    let Some(tick) = tick else {
        return Ok(());
    };
    // In a group, @mention them by their phone number (from `users`, as the
    // reminders do: a group reaction's `from` may be a LID, not a number).
    let phone = user
        .wa_jid
        .split_once('@')
        .map_or(user.wa_jid.as_str(), |(number, _)| number);
    let mention = is_group.then_some(phone);
    let text = prayer::reaction_reply(prayer, tick, prayed, mention);
    let mentions: Vec<String> = mention.map(str::to_string).into_iter().collect();
    // Saved already, so a failure here is logged, not returned: a 500
    // would only make GOWA retry, and the retry changes nothing.
    if let Err(err) = state
        .gowa
        .send_text(&r.chat_id, &text, Some(&r.reacted_message_id), &mentions)
        .await
    {
        warn!("couldn't confirm {prayer}: {err}");
    }
    Ok(())
}

/// Works out what the message asks for, if anything.
///
/// A reply like `done 2` only counts when it quotes one of the bot's lists
/// in this chat; otherwise the text is read as a normal command.
async fn action_for(
    conn: &mut sqlx::SqliteConnection,
    msg: &Message,
    text: &str,
    chat_id: i64,
    sender: Person,
) -> Result<Option<Action>, AppError> {
    // `if let ... && let ...` (let chains): all must match to enter. The
    // list is only looked up once the first two have matched.
    if let Some(quoted) = &msg.replied_to_id
        && let Some(reply) = parser::parse_reply(text)
        && let Some(list) = messages::list_items(conn, quoted, chat_id).await?
    {
        return Ok(Some(Action::ListReply(list, reply)));
    }

    Ok(parser::parse(text).map(|command| Action::Command(assign_to_sender(command, sender))))
}

/// Thumbs-up on the message so the sender knows it was seen.
async fn react(state: &AppState, msg: &Message) {
    if let Err(err) = state.gowa.react(&msg.chat_id, &msg.id, whatsapp::ACK).await {
        warn!("couldn't react: {err}");
    }
}

/// Replies in the same chat, quoting `msg`. Returns the sent message's ID.
/// `mentions` are phone numbers to @mention (see `Gowa::send_text`).
async fn send(state: &AppState, msg: &Message, text: &str, mentions: &[String]) -> Option<String> {
    match state
        .gowa
        .send_text(&msg.chat_id, text, Some(&msg.id), mentions)
        .await
    {
        Ok(id) => Some(id),
        Err(err) => {
            warn!("couldn't reply: {err}");
            None
        }
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
            titles,
            assignee: None,
            due,
            priority,
        } => BotCommand::AddTodo {
            titles,
            assignee: Some(Assignee::Sender(sender)),
            due,
            priority,
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
        use crate::todo_flags::{Due, Priority};
        // The flags come through unchanged.
        let command = BotCommand::AddTodo {
            titles: vec!["go gym".to_string()],
            assignee: None,
            due: Some(Due::Tomorrow),
            priority: Some(Priority::High),
        };
        assert_eq!(
            assign_to_sender(command, bilal()),
            BotCommand::AddTodo {
                titles: vec!["go gym".to_string()],
                assignee: Some(Assignee::Sender(bilal())),
                due: Some(Due::Tomorrow),
                priority: Some(Priority::High),
            }
        );
    }

    #[test]
    fn mentioned_assignee_is_kept() {
        let khadija = Some(Assignee::Mentioned("Khadija".to_string()));
        let command = BotCommand::AddTodo {
            titles: vec!["book dentist".to_string()],
            assignee: khadija.clone(),
            due: None,
            priority: None,
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
    fn parses_a_shared_location() {
        // The shape GOWA v9.7.0 forwards: WhatsApp's LocationMessage, with
        // its camelCase field names. No `body`.
        let body = r#"{"event":"message","payload":{"chat_id":"447700900002@s.whatsapp.net",
            "from":"447700900002@s.whatsapp.net","id":"AC0000000000000000000000000000A4",
            "is_from_me":false,"timestamp":"2026-10-27T21:00:00Z",
            "location":{"degreesLatitude":52.0406,"degreesLongitude":-0.7594,
                        "name":"","address":"","JPEGThumbnail":"/9j/4AAQ"}}}"#;
        let WebhookEvent::Message(msg) = serde_json::from_str(body).unwrap() else {
            panic!("expected a message");
        };
        assert_eq!(msg.body, None);
        let at = msg.location.expect("location parsed");
        assert_eq!(
            (at.degrees_latitude, at.degrees_longitude),
            (52.0406, -0.7594)
        );
    }

    #[test]
    fn unknown_event_does_not_parse() {
        let body = r#"{"event":"group.participants","payload":{"anything":1}}"#;
        assert!(serde_json::from_str::<WebhookEvent>(body).is_err());
    }

    /// Posts a text message from Bilal in a group to the real app,
    /// signed with `test_state`'s secret; returns the status.
    async fn post_text(server: &str, id: &str, text: &str) -> reqwest::StatusCode {
        let payload = serde_json::json!({
            "id": id, "chat_id": crate::testkit::GROUP,
            "from": "447700900001@s.whatsapp.net", "from_name": "Bilal",
            "body": text, "is_from_me": false, "timestamp": "2026-11-02T09:00:00Z"
        });
        post_message(server, payload).await
    }

    /// Posts any message `payload` (the part GOWA puts under `"payload"`),
    /// signed; returns the status.
    async fn post_message(server: &str, payload: serde_json::Value) -> reqwest::StatusCode {
        let body = serde_json::json!({ "event": "message", "payload": payload }).to_string();
        reqwest::Client::new()
            .post(format!("{server}/webhook/whatsapp"))
            .header("x-hub-signature-256", sign(b"test", body.as_bytes()))
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn a_list_that_cant_be_remembered_still_answers_ok() {
        use crate::testkit::{fake_gowa, serve_state, test_state};

        let pool = crate::db::test_pool().await;
        let gowa = fake_gowa().await;
        let server = serve_state(test_state(pool, &gowa.url)).await;
        assert_eq!(post_text(&server, "M1", "+shop milk").await, 200);
        assert_eq!(post_text(&server, "M2", "/shop").await, 200);

        // The fake GOWA gives every message the same ID, so saving this
        // second list's numbers clashes with the first one's. That happens
        // after the command is saved and the list sent, so it's only
        // logged: a 500 would make GOWA retry, and the retry is skipped.
        assert_eq!(post_text(&server, "M3", "/shop").await, 200);
    }

    /// How many rows `table` has. `query_scalar` without the `!` is checked
    /// at run time rather than compile time, so it can be used for any
    /// table here without adding to `.sqlx`.
    async fn count(pool: &sqlx::SqlitePool, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
    }

    #[tokio::test]
    async fn same_message_id_twice_adds_one_todo() {
        use crate::testkit::{fake_gowa, serve_state, test_state};

        let pool = crate::db::test_pool().await;
        let gowa = fake_gowa().await;
        let server = serve_state(test_state(pool.clone(), &gowa.url)).await;
        // GOWA retrying: the same message ID again. Both answered 200 (the
        // second is skipped as a duplicate), and the todo is added once.
        assert_eq!(post_text(&server, "T1", "+todo fix the tap").await, 200);
        assert_eq!(post_text(&server, "T1", "+todo fix the tap").await, 200);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM todos").await, 1);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM points").await, 0);
        // One reply, to the first delivery only.
        assert_eq!(gowa.sent.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn bot_own_message_is_ignored() {
        use crate::testkit::{GROUP, fake_gowa, serve_state, test_state};

        let pool = crate::db::test_pool().await;
        let gowa = fake_gowa().await;
        let server = serve_state(test_state(pool.clone(), &gowa.url)).await;
        let own = serde_json::json!({
            "id": "OWN1", "chat_id": GROUP,
            "from": "447700900009@s.whatsapp.net", "from_name": "Family HQ",
            "body": "+todo fix the tap", "is_from_me": true,
            "timestamp": "2026-11-02T09:00:00Z"
        });
        assert_eq!(post_message(&server, own).await, 200);
        // Nobody registered, nothing added, nothing sent.
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM users").await, 0);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM todos").await, 0);
        assert_eq!(
            count(&pool, "SELECT COUNT(*) FROM processed_messages").await,
            0
        );
        assert!(gowa.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn slow_lookup_does_not_block_other_writers() {
        use std::time::Duration;

        use crate::testkit::{fake_gowa, serve_state, slow_service, test_state};

        // A real file, so several connections share it as in production.
        let db = crate::db::test_file("slow-lookup").await;
        let gowa = fake_gowa().await;
        let mut state = test_state(db.pool.clone(), &gowa.url);
        // `/weather` looks the town up first; this geocoder takes 2 s.
        let slow = slow_service(Duration::from_secs(2)).await;
        Arc::get_mut(&mut state).expect("not shared yet").apis.geo =
            crate::geo::Geo::with_geocoding_url(&slow);
        let server = serve_state(state).await;

        // `tokio::spawn` runs the slow request in the background, so the
        // test carries on while it waits on the geocoder.
        let weather = tokio::spawn({
            let server = server.clone();
            async move { post_text(&server, "W1", "/weather").await }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Meanwhile Khadija, in her own DM, adds to her shopping list. That
        // write mustn't wait for Bilal's weather.
        let shop = serde_json::json!({
            "id": "S1", "chat_id": "447700900002@s.whatsapp.net",
            "from": "447700900002@s.whatsapp.net", "from_name": "Khadija",
            "body": "+shop milk", "is_from_me": false,
            "timestamp": "2026-11-02T09:00:00Z"
        });
        let started = std::time::Instant::now();
        let status =
            tokio::time::timeout(Duration::from_secs(1), post_message(&server, shop)).await;
        let waited = started.elapsed();

        // Let the weather finish before the file is deleted.
        assert_eq!(weather.await.unwrap(), 200);
        let items = count(&db.pool, "SELECT COUNT(*) FROM shopping_items").await;
        db.remove().await;
        assert_eq!(
            status.ok(),
            Some(reqwest::StatusCode::OK),
            "+shop waited {waited:?} behind /weather's lookup"
        );
        assert_eq!(items, 1);
    }

    // `multi_thread`: a runtime with several threads, so the two requests
    // really run at the same time (the default test runtime has one).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_duplicate_deliveries_add_one_todo() {
        use crate::testkit::{fake_gowa, seed_family, serve_state, test_state, utc};

        let db = crate::db::test_file("duplicates").await;
        {
            let mut conn = db.pool.acquire().await.unwrap();
            seed_family(&mut conn, utc("2026-11-01T09:00:00Z")).await;
        }
        let gowa = fake_gowa().await;
        let server = serve_state(test_state(db.pool.clone(), &gowa.url)).await;
        // GOWA delivering the same message twice at once. `join!` runs both
        // requests together and waits for both.
        let (first, second) = tokio::join!(
            post_text(&server, "D1", "+todo fix the tap"),
            post_text(&server, "D1", "+todo fix the tap"),
        );
        let todos = count(&db.pool, "SELECT COUNT(*) FROM todos").await;
        db.remove().await;
        // Both answered 200: one ran it, the other was skipped as a
        // duplicate. Neither is a 500 (which would make GOWA retry again).
        assert_eq!(
            (first, second),
            (reqwest::StatusCode::OK, reqwest::StatusCode::OK)
        );
        assert_eq!(todos, 1);
    }
}

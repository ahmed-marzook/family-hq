//! The reminder loop's engine: one way of sending, shared by every job on
//! the loop (prayer reminders, todo reminders, prayer and points reports).
//!
//! Each job is split in two:
//!
//! * **Plan**: work out what's due and what it says, as data. A job's plan
//!   reads the database (and prayer times) but never sends or claims
//!   anything, so a test can call it and look at what it returned.
//! * **Deliver**: [`deliver`] takes those plans and, for each one, claims
//!   it, sends its messages, does the bookkeeping that comes after a send
//!   (saving the message ID), and pushes. The rules every job must keep
//!   live here once instead of in each job:
//!   - *Claim before send.* The claim is a row in the database that can
//!     only be made once. If it already exists, the plan went out on an
//!     earlier tick (or before a restart) and nothing is sent. So a crash
//!     can lose a message, but never send one twice.
//!   - *One failure doesn't stop the rest.* A send or bookkeeping problem
//!     is logged, and the next message goes on.
//!   - *No pooled connection is held while waiting on WhatsApp.*
//!
//! What a job needs done in the database is *data* too (`Claim`, `OnSent`),
//! not code: a new job adds a variant, and `deliver` stays the one place
//! that does the claiming and the sending.
//!
//! Stage 2.2 (`+remind`) is meant to plug in here: its plans, with a claim
//! of their own and the message ID kept so reactions can find them.

use std::future::Future;

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{SqliteConnection, SqlitePool};
use tracing::{info, warn};

use crate::error::AppError;
use crate::prayer::{Prayer, ReminderKind};
use crate::push::{self, Notice, Ntfy};
use crate::repo::messages::{self, ListKind};
use crate::repo::prayers::{self, NewReminder};
use crate::repo::reports as stored_reports;
use crate::repo::todo_reminders;
use crate::reports::{Feature, ReportKind};

/// Something that can send a WhatsApp message: in the app, `Gowa`; in the
/// tests, a fake that records what it was asked to send.
///
/// A *trait* is a set of methods a type promises to have. Code written
/// against the trait (`deliver<M: Messenger>`) works with any type that
/// implements it. `deliver` is *generic* over `M`: the compiler makes a
/// copy of it for each type it's used with ("static dispatch"), so calling
/// through the trait costs nothing at run time.
///
/// The method returns `impl Future<Output = ...> + Send`: "some future that
/// gives a `Result` when awaited, and can move between threads". It's what
/// an `async fn` returns, written out, because a trait's method has to say
/// the future is `Send` for the loop (run by `tokio::spawn`, which can move
/// it between threads) to accept it. Spelled this way, no extra crate
/// (`async-trait`) is needed.
pub trait Messenger {
    /// Sends `text` to `chat_id`, as a reply quoting `reply_to` if given,
    /// @mentioning `mentions` (phone numbers). The new message's WhatsApp
    /// ID, or why it couldn't be sent. The same as `Gowa::send_text`.
    fn send_text(
        &self,
        chat_id: &str,
        text: &str,
        reply_to: Option<&str>,
        mentions: &[String],
    ) -> impl Future<Output = Result<String, AppError>> + Send;
}

/// The row that proves a plan is being sent, made before anything is.
/// Only the first claim of each succeeds.
#[derive(Debug, Clone, PartialEq)]
pub enum Claim {
    /// One prayer reminder for one place in a chat
    /// (`prayers::claim_reminder`). Its row ID is kept, for
    /// `OnSent::KeepReminderMessage`.
    PrayerReminder {
        chat_id: i64,
        /// The day it's for, in the place's timezone.
        date: NaiveDate,
        prayer: Prayer,
        kind: ReminderKind,
        /// Which members' times it's for (`Location::key`); '' in a DM.
        place: String,
        /// When the prayer starts.
        prayer_time: DateTime<Utc>,
    },
    /// One person's todo reminder slot (`todo_reminders::claim`).
    TodoSlot {
        chat_id: i64,
        user_id: i64,
        slot_at: DateTime<Utc>,
    },
    /// One report for one chat (`repo::reports::claim`). Claimed even when
    /// it has no messages, so the job doesn't work it out again every
    /// minute.
    Report {
        chat_id: i64,
        feature: Feature,
        kind: ReportKind,
        /// The last day it covers.
        date: NaiveDate,
        /// Which part of a group it's for (prayers: the place); '' if none.
        place: String,
    },
}

/// What a successful claim gives back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Claimed {
    /// A prayer reminder's row, to save its message ID on.
    Reminder(i64),
    /// Claimed, with nothing to remember.
    Done,
}

/// The bookkeeping after a message has gone out, which needs its WhatsApp
/// ID. Not done if the send failed.
#[derive(Debug, Clone, PartialEq)]
pub enum OnSent {
    Nothing,
    /// Saves the ID on the claimed prayer reminder, so a reaction to the
    /// message finds which prayer it was (`prayers::set_reminder_message`).
    KeepReminderMessage,
    /// Saves the numbered list it shows, as `/todo` does, so a reply
    /// `done 3` or a `+done 3` finds the right item (`messages::save_list`).
    SaveList {
        chat_id: i64,
        kind: ListKind,
        /// `item_ids[0]` is item 1, and so on.
        item_ids: Vec<i64>,
    },
}

/// One push, through ntfy, to someone who has push on. Sent once per
/// person and `key` (`push::send_once`), even if two chats ask.
#[derive(Debug, Clone, PartialEq)]
pub struct Push {
    pub user_id: i64,
    pub feature: push::Feature,
    pub key: String,
    pub notice: Notice,
}

/// One WhatsApp message to send.
#[derive(Debug, Clone, PartialEq)]
pub struct Outgoing {
    /// The WhatsApp chat ID to send it to.
    pub to: String,
    pub text: String,
    /// Phone numbers @mentioned in `text`.
    pub mentions: Vec<String>,
    /// A message to quote, as a reply. None of the loop's jobs quote yet.
    pub quote: Option<String>,
    pub on_sent: OnSent,
    /// Pushes that go with it. They're sent even if the WhatsApp message
    /// couldn't be: the push is the second way to reach the person.
    pub push: Vec<Push>,
    /// What it is, for the log: "Asr Start (Milton Keynes)".
    pub what: String,
}

/// One claim and the messages it covers: a reminder is one message, a
/// report sent privately can be several, and a claim with none just
/// records that it's been dealt with.
#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    pub claim: Claim,
    pub messages: Vec<Outgoing>,
}

/// Claims each plan and, if the claim is new, sends its messages. Never
/// fails as a whole: every problem is logged, and the next message or plan
/// goes on, as each job's loop did before.
///
/// `ntfy` is `None` when push isn't set up; pushes are then skipped.
pub async fn deliver<M: Messenger>(
    messenger: &M,
    db: &SqlitePool,
    ntfy: Option<&Ntfy>,
    planned: Vec<Planned>,
    now: DateTime<Utc>,
) {
    for plan in planned {
        // Claim first, send second. A block, so the connection goes back
        // to the pool before anything waits on WhatsApp.
        let claimed = {
            match db.acquire().await {
                Ok(mut conn) => claim(&mut conn, &plan.claim, now).await,
                Err(err) => Err(err.into()),
            }
        };
        let claimed = match claimed {
            Ok(Some(claimed)) => claimed,
            // Already claimed: sent on an earlier tick, or before a restart.
            Ok(None) => continue,
            Err(err) => {
                warn!("couldn't claim {:?}: {err}", plan.claim);
                continue;
            }
        };
        for message in plan.messages {
            let what = message.what.clone();
            if let Err(err) = send_one(messenger, db, ntfy, claimed, message, now).await {
                warn!("{what}: {err}");
            }
        }
    }
}

/// Makes the claim. `None` if it was already made.
async fn claim(
    conn: &mut SqliteConnection,
    claim: &Claim,
    now: DateTime<Utc>,
) -> Result<Option<Claimed>, AppError> {
    let claimed = match claim {
        Claim::PrayerReminder {
            chat_id,
            date,
            prayer,
            kind,
            place,
            prayer_time,
        } => {
            let reminder = NewReminder {
                chat_id: *chat_id,
                date: *date,
                prayer: *prayer,
                kind: *kind,
                place,
                prayer_time: *prayer_time,
            };
            prayers::claim_reminder(conn, &reminder, now)
                .await?
                .map(Claimed::Reminder)
        }
        Claim::TodoSlot {
            chat_id,
            user_id,
            slot_at,
        } => todo_reminders::claim(conn, *chat_id, *user_id, *slot_at)
            .await?
            .then_some(Claimed::Done),
        Claim::Report {
            chat_id,
            feature,
            kind,
            date,
            place,
        } => stored_reports::claim(conn, *chat_id, *feature, *kind, *date, place, now)
            .await?
            .then_some(Claimed::Done),
    };
    Ok(claimed)
}

/// Sends one message of a claimed plan, then its bookkeeping and pushes.
async fn send_one<M: Messenger>(
    messenger: &M,
    db: &SqlitePool,
    ntfy: Option<&Ntfy>,
    claimed: Claimed,
    message: Outgoing,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    let Outgoing {
        to,
        text,
        mentions,
        quote,
        on_sent,
        push,
        what,
    } = message;
    match messenger
        .send_text(&to, &text, quote.as_deref(), &mentions)
        .await
    {
        Ok(message_id) => {
            let mut conn = db.acquire().await?;
            match (on_sent, claimed) {
                (OnSent::Nothing, _) => {}
                (OnSent::KeepReminderMessage, Claimed::Reminder(reminder_id)) => {
                    prayers::set_reminder_message(&mut conn, reminder_id, &message_id).await?;
                }
                // A plan that asks to keep a reminder's message without
                // claiming a reminder: a mistake in that job's plan.
                (OnSent::KeepReminderMessage, Claimed::Done) => {
                    warn!("{what}: no reminder to keep the message ID on");
                }
                (
                    OnSent::SaveList {
                        chat_id,
                        kind,
                        item_ids,
                    },
                    _,
                ) => {
                    messages::save_list(&mut conn, &message_id, chat_id, kind, &item_ids, now)
                        .await?;
                }
            }
            info!("sent {what} to {to}");
        }
        // Not retried: the claim stays, so a flaky GOWA can't cause a
        // burst of repeats later.
        Err(err) => warn!("couldn't send {what} to {to}: {err}"),
    }

    if let Some(ntfy) = ntfy {
        for p in &push {
            let mut conn = db.acquire().await?;
            push::send_once(
                ntfy, &mut conn, p.user_id, p.feature, &p.key, &p.notice, now,
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;
    use crate::db::test_pool;
    use crate::repo::chats;
    use crate::testkit::{date, utc};

    /// A fake `Messenger`: records each message it's asked to send, fails
    /// the ones whose text contains `fail_on`, and, at the moment of each
    /// send, asks the database whether that message's report was already
    /// claimed (`claimed_when_sent`).
    struct Recorder {
        pool: SqlitePool,
        fail_on: Option<&'static str>,
        sent: Mutex<Vec<String>>,
        claimed_when_sent: Mutex<Vec<bool>>,
    }

    impl Recorder {
        fn new(pool: &SqlitePool, fail_on: Option<&'static str>) -> Self {
            Recorder {
                pool: pool.clone(),
                fail_on,
                sent: Mutex::new(Vec::new()),
                claimed_when_sent: Mutex::new(Vec::new()),
            }
        }

        fn sent(&self) -> Vec<String> {
            self.sent.lock().unwrap().clone()
        }
    }

    /// Implementing the trait: the same method, with a body of our own.
    /// An `async fn` gives the future the trait asks for (as in `Gowa`'s).
    impl Messenger for Recorder {
        async fn send_text(
            &self,
            _chat_id: &str,
            text: &str,
            _reply_to: Option<&str>,
            _mentions: &[String],
        ) -> Result<String, AppError> {
            // The lock is only held for the push, never across the
            // `.await` below.
            self.sent.lock().unwrap().push(text.to_string());
            // Was this report claimed before the send? Claiming it again
            // tells: `false` means it already was. (If `deliver` held the
            // pool's only connection here, this would wait forever; the
            // test's timeout catches that.)
            let mut conn = self.pool.acquire().await?;
            let again = stored_reports::claim(
                &mut conn,
                1,
                Feature::Points,
                ReportKind::Day,
                report_date(text),
                "",
                utc("2026-11-08T20:00:00Z"),
            )
            .await?;
            self.claimed_when_sent.lock().unwrap().push(!again);
            if self.fail_on.is_some_and(|bad| text.contains(bad)) {
                return Err(AppError::BadRequest("GOWA said no".to_string()));
            }
            Ok(format!("ID-{text}"))
        }
    }

    /// The tests' messages say which day's report they are: "day 3".
    fn report_date(text: &str) -> NaiveDate {
        let day: u32 = text.trim_start_matches("day ").parse().unwrap();
        NaiveDate::from_ymd_opt(2026, 11, day).unwrap()
    }

    fn message(text: &str, on_sent: OnSent) -> Outgoing {
        Outgoing {
            to: crate::testkit::GROUP.to_string(),
            text: text.to_string(),
            mentions: Vec::new(),
            quote: None,
            on_sent,
            push: Vec::new(),
            what: text.to_string(),
        }
    }

    /// The report for `day` November, in chat 1, as one message.
    fn report(day: u32, on_sent: OnSent) -> Planned {
        Planned {
            claim: Claim::Report {
                chat_id: 1,
                feature: Feature::Points,
                kind: ReportKind::Day,
                date: NaiveDate::from_ymd_opt(2026, 11, day).unwrap(),
                place: String::new(),
            },
            messages: vec![message(&format!("day {day}"), on_sent)],
        }
    }

    /// A pool with chat 1 (the group) in it, for the claims to point at.
    async fn pool_with_a_chat() -> SqlitePool {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let id = chats::find_or_create(
            &mut conn,
            crate::testkit::GROUP,
            None,
            utc("2026-11-01T12:00:00Z"),
        )
        .await
        .unwrap();
        assert_eq!(id, 1);
        pool
    }

    /// `deliver`, but failing the test rather than hanging if it waits
    /// forever (say on the pool's only connection).
    async fn deliver_soon(recorder: &Recorder, pool: &SqlitePool, planned: Vec<Planned>) {
        let now = utc("2026-11-08T20:00:00Z");
        tokio::time::timeout(
            Duration::from_secs(10),
            deliver(recorder, pool, None, planned, now),
        )
        .await
        .expect("deliver finished");
    }

    #[tokio::test]
    async fn claims_before_sending_and_sends_each_plan_once() {
        let pool = pool_with_a_chat().await;
        let recorder = Recorder::new(&pool, None);

        deliver_soon(&recorder, &pool, vec![report(3, OnSent::Nothing)]).await;
        assert_eq!(recorder.sent(), ["day 3"]);
        assert_eq!(
            *recorder.claimed_when_sent.lock().unwrap(),
            [true],
            "the claim was made before the send"
        );

        // The same plan again (the next tick): already claimed, not sent.
        deliver_soon(&recorder, &pool, vec![report(3, OnSent::Nothing)]).await;
        assert_eq!(recorder.sent(), ["day 3"]);
    }

    #[tokio::test]
    async fn one_failed_send_does_not_stop_the_others_and_is_not_retried() {
        let pool = pool_with_a_chat().await;
        let recorder = Recorder::new(&pool, Some("day 4"));
        let save = |id| OnSent::SaveList {
            chat_id: 1,
            kind: ListKind::Todo,
            item_ids: vec![id],
        };
        let planned = vec![
            report(3, save(30)),
            report(4, save(40)),
            report(5, save(50)),
        ];

        deliver_soon(&recorder, &pool, planned.clone()).await;
        assert_eq!(recorder.sent(), ["day 3", "day 4", "day 5"]);

        // Only the sent ones' lists were saved, each against its own ID.
        let mut conn = pool.acquire().await.unwrap();
        for (text, saved) in [("day 3", true), ("day 4", false), ("day 5", true)] {
            let list = messages::list_items(&mut conn, &format!("ID-{text}"), 1)
                .await
                .unwrap();
            assert_eq!(list.is_some(), saved, "{text}");
        }
        drop(conn);

        // The failed one stays claimed: the next tick doesn't send it again.
        deliver_soon(&recorder, &pool, planned).await;
        assert_eq!(recorder.sent().len(), 3);
    }

    #[tokio::test]
    async fn a_claim_with_no_messages_still_counts() {
        let pool = pool_with_a_chat().await;
        let recorder = Recorder::new(&pool, None);
        let mut nothing_to_say = report(3, OnSent::Nothing);
        nothing_to_say.messages.clear();

        deliver_soon(&recorder, &pool, vec![nothing_to_say]).await;
        // Later in the grace period there's something to say after all,
        // but the report was dealt with.
        deliver_soon(&recorder, &pool, vec![report(3, OnSent::Nothing)]).await;
        assert!(recorder.sent().is_empty());
    }

    #[tokio::test]
    async fn a_prayer_reminder_keeps_its_message_id() {
        let pool = pool_with_a_chat().await;
        let recorder = Recorder::new(&pool, None);
        let plan = Planned {
            claim: Claim::PrayerReminder {
                chat_id: 1,
                date: date("2026-11-03"),
                prayer: Prayer::Asr,
                kind: ReminderKind::Start,
                place: String::new(),
                prayer_time: utc("2026-11-03T14:00:00Z"),
            },
            messages: vec![message("day 3", OnSent::KeepReminderMessage)],
        };

        deliver_soon(&recorder, &pool, vec![plan.clone()]).await;
        deliver_soon(&recorder, &pool, vec![plan]).await;
        assert_eq!(recorder.sent(), ["day 3"], "once");

        let mut conn = pool.acquire().await.unwrap();
        let found = prayers::find_reminder(&mut conn, "ID-day 3", crate::testkit::GROUP)
            .await
            .unwrap();
        assert_eq!(found, Some((date("2026-11-03"), Prayer::Asr)));
    }
}

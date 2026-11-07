//! What the bot does for each command: database changes, then the text to
//! send back. No GOWA here; the webhook does the sending.
//!
//! A command runs in two steps: `prepare` does any lookups it needs from
//! outside services (prayer times, towns, the weather) before the webhook
//! opens its write transaction, and `execute` does the database work inside
//! it, without the network.
//!
//! Each feature's own commands live with the feature: `todo`, `shop`,
//! `prayer::commands`, `points::commands` and `settings::commands`. Replies
//! to a numbered list are `lists`, and the small commands with no folder of
//! their own (`/weather`, `/moon`, `/push`) are `misc`. This module only
//! hands each command over.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use sqlx::SqliteConnection;

use crate::Apis;
use crate::config::PushConfig;
use crate::error::AppError;
use crate::geo::{Coordinates, NearestPlace};
use crate::help;
use crate::parser::{BotCommand, Got, PushCommand};
use crate::points::{self, Finish};
use crate::prayer::commands::{self, Looked};
use crate::push;
use crate::repo::messages::ListKind;
use crate::repo::push::Subscription;
use crate::repo::settings as stored;
use crate::repo::users::User;
use crate::settings::commands::SettingsStep;
use crate::settings::{self, Settings};
use crate::shop;
use crate::todo;
use emoji::PROBLEM;

pub(crate) mod emoji;
pub mod lists;
mod misc;

/// Who is asking, and where.
pub struct Context {
    /// `chats.id` of the chat the message came from.
    pub chat_id: i64,
    /// A group (shared settings) rather than a DM (the sender's own).
    pub is_group: bool,
    pub sender: User,
    pub now: DateTime<Utc>,
}

/// What to send back.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// A normal reply.
    Reply(String),
    /// A numbered list. `item_ids[0]` is item 1, and so on; the webhook
    /// saves them, with `kind`, against the sent message's ID so replies
    /// can find them.
    List {
        text: String,
        kind: ListKind,
        item_ids: Vec<i64>,
    },
    /// A reply that @mentions people: `mentions` are their phone numbers,
    /// and `text` contains `@<number>` where each should appear.
    Mentioning { text: String, mentions: Vec<String> },
    /// Several messages, sent one after another: for something worth
    /// copying on its own, like the server and topic after `/push on`
    /// (WhatsApp copies a whole message at a time).
    Several(Vec<String>),
}

impl Outcome {
    /// Just the text, whatever kind of reply it is (handy in tests).
    // Only the tests use this.
    #[cfg(test)]
    pub fn into_text(self) -> String {
        match self {
            Outcome::Reply(text)
            | Outcome::List { text, .. }
            | Outcome::Mentioning { text, .. } => text,
            Outcome::Several(texts) => texts.join("\n\n"),
        }
    }
}

/// The parts of `Apis` that never use the network: all that `execute`
/// gets. There's no HTTP client in here at all, so code running inside a
/// write transaction *can't* call out to a slow service by mistake; the
/// compiler would refuse.
///
/// `<'a>` is a *lifetime*: `Offline` only borrows these from an `Apis`,
/// and `'a` tells the compiler it mustn't outlive the `Apis` it came from.
pub struct Offline<'a> {
    /// The server-wide settings (`.env`), for chats that haven't chosen
    /// their own.
    pub defaults: &'a Settings,
    /// How push is set up (`NTFY_URL`), if it is.
    pub push: Option<&'a PushConfig>,
}

impl<'a> Offline<'a> {
    pub fn of(apis: &'a Apis) -> Self {
        Offline {
            defaults: &apis.prayer_times.defaults,
            push: apis.push.as_ref().map(push::Ntfy::config),
        }
    }
}

/// A command with everything it needs from outside services already looked
/// up: made by `prepare`, run by `execute`.
///
/// The field is private, so only `prepare` can make one, and `execute` can
/// count on each command's lookups having been done.
pub struct Prepared(Step);

/// What `prepare` found for each kind of command.
enum Step {
    /// Needs nothing from outside services.
    Plain(BotCommand),
    /// Answered completely by `prepare`, which only read the database:
    /// `/weather` and `/moon`.
    Answered(String),
    /// `/prayers` and `+prayed`, with their prayer times looked up
    /// (`prayer::commands`). In a `Box` (on the heap) because it's much
    /// bigger than the other steps, and an enum is always the size of its
    /// biggest variant; boxed, this one is only a pointer.
    Prayer(Box<Looked>),
    /// A location shared in a DM: the nearest town and the timezone there,
    /// or (`Err`) the reply saying why not.
    SharedLocation {
        at: Coordinates,
        found: Result<(NearestPlace, Tz), String>,
    },
    /// `/settings ...`, with any new town already looked up.
    Settings(SettingsStep),
    /// `/push ...`. Needs nothing up front; `/push test`'s push is sent
    /// after the transaction, by `Ran::finish`.
    Push(PushCommand),
}

/// What `execute` did. Almost every command is finished; `/push test`
/// still has its push to send, which waits until the transaction is saved
/// (so a retried webhook that's skipped as a duplicate never sends a second
/// one).
pub enum Ran {
    Done(Outcome),
    /// Send a test push to this subscription.
    TestPush(Subscription),
}

impl Ran {
    /// Does what had to wait for the commit, and gives the reply. Call it
    /// only once the transaction is committed.
    pub async fn finish(self, apis: &Apis) -> Outcome {
        match self {
            Ran::Done(outcome) => outcome,
            Ran::TestPush(subscription) => {
                Outcome::Reply(misc::test_push(apis, &subscription).await)
            }
        }
    }
}

/// Step one of running a command: every lookup it needs from outside
/// services (AlAdhan, the geocoders, Open-Meteo), before the webhook opens
/// its write transaction. `conn` should be a plain connection, not a
/// transaction: this only reads from it.
///
/// Why: SQLite lets one connection write at a time. A transaction that
/// has written holds that lock until it ends, so a lookup inside it that
/// takes 15 seconds would make every other write (other messages, the
/// reminder loop, the dashboard) wait, and give up after 5 with "database
/// is locked". Here, nothing is locked while we wait.
///
/// Most lookups answer from a cache; at most once a day per place does
/// one reach AlAdhan. A failed lookup isn't an `Err` here: it's kept, and
/// `execute` words the reply ("the service isn't answering"). `Err` is
/// only for the database.
pub async fn prepare(
    conn: &mut SqliteConnection,
    ctx: &Context,
    apis: &Apis,
    command: BotCommand,
) -> Result<Prepared, AppError> {
    let step = match command {
        BotCommand::Prayers => {
            Step::Prayer(Box::new(commands::look_up_today(conn, ctx, apis).await?))
        }
        BotCommand::Prayed(checkin) => Step::Prayer(Box::new(
            commands::look_up_checkin(conn, ctx, apis, checkin).await?,
        )),
        BotCommand::Weather(when) => Step::Answered(misc::weather(conn, ctx, apis, when).await?),
        BotCommand::Moon => Step::Answered(misc::moon(conn, ctx, apis).await?),
        BotCommand::SharedLocation {
            latitude,
            longitude,
        } => {
            let at = Coordinates {
                latitude,
                longitude,
            };
            let found = settings::commands::find_place(conn, ctx, apis, at).await?;
            Step::SharedLocation { at, found }
        }
        BotCommand::Settings(command) => {
            Step::Settings(settings::commands::look_up(conn, ctx, apis, command).await?)
        }
        BotCommand::Push(command) => Step::Push(command),
        other => Step::Plain(other),
    };
    Ok(Prepared(step))
}

/// Step two: the command's database work and its reply, using what
/// `prepare` looked up. Runs inside the webhook's transaction, and can't
/// reach the network (see `Offline`).
pub async fn execute(
    conn: &mut SqliteConnection,
    ctx: &Context,
    offline: &Offline<'_>,
    prepared: Prepared,
) -> Result<Ran, AppError> {
    let defaults = offline.defaults;
    let text = match prepared.0 {
        Step::Plain(command) => {
            return run_plain(conn, ctx, defaults, command).await.map(Ran::Done);
        }
        Step::Answered(text) => text,
        Step::Prayer(looked) => commands::execute(conn, ctx, *looked).await?,
        Step::SharedLocation { at, found } => {
            settings::commands::save_place(conn, ctx, defaults, at, found).await?
        }
        Step::Settings(step) => settings::commands::execute(conn, ctx, defaults, step).await?,
        Step::Push(command) => return misc::push(conn, ctx, offline, command).await,
    };
    Ok(Ran::Done(Outcome::Reply(text)))
}

/// Runs a command in one go, on one connection: `prepare`, `execute`, then
/// `finish`. For tests only, where there's no transaction to keep short.
#[cfg(test)]
pub(crate) async fn run_command(
    conn: &mut SqliteConnection,
    ctx: &Context,
    apis: &Apis,
    command: BotCommand,
) -> Result<Outcome, AppError> {
    let prepared = prepare(conn, ctx, apis, command).await?;
    let ran = execute(conn, ctx, &Offline::of(apis), prepared).await?;
    Ok(ran.finish(apis).await)
}

/// The commands that need nothing from outside services.
async fn run_plain(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    command: BotCommand,
) -> Result<Outcome, AppError> {
    let text = match command {
        BotCommand::AddTodo {
            titles,
            assignee,
            due,
            priority,
        } => todo::add(conn, ctx, defaults, &titles, assignee, due, priority).await?,
        BotCommand::ListTodos(view) => return todo::list(conn, ctx, defaults, view).await,
        BotCommand::BadTodo(err) => format!("{PROBLEM} {err}"),
        BotCommand::DoneTodo(Got::Numbers(numbers)) => {
            return todo::close_by_numbers(conn, ctx, defaults, &numbers, Finish::Done).await;
        }
        BotCommand::DoneTodo(Got::Names(names)) => {
            todo::close_by_name(conn, ctx, defaults, &names, Finish::Done).await?
        }
        BotCommand::SkipTodo(Got::Numbers(numbers)) => {
            return todo::close_by_numbers(conn, ctx, defaults, &numbers, Finish::Skipped).await;
        }
        BotCommand::SkipTodo(Got::Names(names)) => {
            todo::close_by_name(conn, ctx, defaults, &names, Finish::Skipped).await?
        }
        BotCommand::ClearTodos { done_only } => todo::clear(conn, ctx, done_only).await?,
        BotCommand::TodoReminders(toggle) => todo::reminders(conn, ctx, toggle).await?,
        BotCommand::AddShop(names) => shop::add(conn, ctx, &names).await?,
        BotCommand::ListShop => return shop::list(conn, ctx).await,
        BotCommand::GotShop(Got::Numbers(numbers)) => {
            return shop::got_by_numbers(conn, ctx, defaults, numbers).await;
        }
        BotCommand::GotShop(Got::Names(names)) => {
            return shop::got_by_name(conn, ctx, &names).await;
        }
        BotCommand::ClearShop { bought_only } => shop::clear(conn, ctx, bought_only).await?,
        BotCommand::PrayerReminders(on) => commands::reminders(conn, ctx, defaults, on).await?,
        BotCommand::PrayerSummary(span) => commands::summary(conn, ctx, defaults, span).await?,
        BotCommand::PrayerReports(change) => commands::reports(conn, ctx, change).await?,
        BotCommand::Points => points::commands::show(conn, ctx, defaults).await?,
        BotCommand::PointsReports(change) => points::commands::reports(conn, ctx, change).await?,
        // `prepare` turns each of these into a step of its own, never into
        // `Step::Plain`, so they can't arrive here.
        BotCommand::Prayers
        | BotCommand::Prayed(_)
        | BotCommand::Weather(_)
        | BotCommand::Moon
        | BotCommand::SharedLocation { .. }
        | BotCommand::Settings(_)
        | BotCommand::Push(_) => unreachable!("prepare gives this command its own step"),
        BotCommand::Help(topic) => help::page(topic.as_deref()),
    };
    Ok(Outcome::Reply(text))
}

/// This chat's settings with the defaults filled in, and whether prayer
/// reminders are on.
pub(crate) async fn chat_settings(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
) -> Result<(Settings, bool), AppError> {
    let stored = stored::get(conn, ctx.chat_id).await?;
    // No person: these are the chat's own (what `/settings` shows and
    // changes), not the sender's.
    let settings = settings::layered(defaults, &stored.overrides, None);
    Ok((settings, stored.prayer_reminders))
}

/// The sender's own settings: in a DM, the DM's; in a group, their personal
/// ones (from their DM with the bot) on top of the group's. Prayer times
/// and "today" follow the person, so someone abroad sees their own.
///
/// `pub(crate)`: usable anywhere in this crate (the prayer commands in
/// `prayer::commands` need it) but, unlike `pub`, never part of the
/// library's outside face.
pub(crate) async fn my_settings(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
) -> Result<Settings, AppError> {
    let person = Some(ctx.sender.id);
    Ok(stored::effective(conn, defaults, ctx.chat_id, person).await?)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::aladhan::PrayerTimes;
    use crate::db::test_pool;
    use crate::prayer::{DayTimes, HijriDate, Prayer, Window};
    use crate::repo::chats;
    use crate::repo::messages::{ListItem, SentList};
    use crate::settings::tests::defaults;
    use crate::testkit::{register, seed_family, utc};
    use chrono::NaiveDate;

    /// 27 Oct 2026 in Milton Keynes (GMT, so London time = UTC).
    pub(crate) fn day() -> DayTimes {
        let at = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        let w = |prayer, start, end| Window {
            prayer,
            start: at(start),
            end: at(end),
        };
        DayTimes {
            date: NaiveDate::from_ymd_opt(2026, 10, 27).unwrap(),
            hijri: Some(HijriDate {
                day: 16,
                month: 5,
                year: 1448,
            }),
            windows: [
                w(Prayer::Fajr, "2026-10-27T04:55:00Z", "2026-10-27T06:49:00Z"),
                w(
                    Prayer::Dhuhr,
                    "2026-10-27T11:47:00Z",
                    "2026-10-27T14:16:00Z",
                ),
                w(Prayer::Asr, "2026-10-27T14:16:00Z", "2026-10-27T16:44:00Z"),
                w(
                    Prayer::Maghrib,
                    "2026-10-27T16:44:00Z",
                    "2026-10-27T18:31:00Z",
                ),
                w(Prayer::Isha, "2026-10-27T18:31:00Z", "2026-10-27T23:46:00Z"),
            ],
        }
    }

    /// Prayer times that come from memory, never the network.
    pub(crate) fn times() -> PrayerTimes {
        PrayerTimes::with_cached(defaults(), day())
    }

    pub(crate) fn item(position: i64, item_id: i64) -> ListItem {
        ListItem { position, item_id }
    }

    /// A list of `kind` showing `item_ids`, numbered from 1, as a reply to
    /// it would see it.
    pub(crate) fn sent(kind: ListKind, item_ids: &[i64]) -> SentList {
        let items = item_ids
            .iter()
            .zip(1..)
            .map(|(&id, pos)| item(pos, id))
            .collect();
        SentList { kind, items }
    }

    // ---- with a database ----

    /// A group with Bilal (the sender) and Khadija in it.
    pub(crate) async fn setup(conn: &mut SqliteConnection) -> Context {
        let now = Utc::now();
        let chat_id = chats::find_or_create(conn, "120363000000000001@g.us", None, now)
            .await
            .unwrap();
        let mut sender = None;
        for (name, phone) in [("Bilal", "447700900001"), ("Khadija", "447700900002")] {
            let user = register(conn, name, phone, now).await;
            chats::add_member(conn, chat_id, user.id, now)
                .await
                .unwrap();
            sender.get_or_insert(user);
        }
        Context {
            chat_id,
            is_group: true,
            sender: sender.unwrap(),
            now,
        }
    }

    /// Runs a typed message, as the webhook would (`+todo` with no
    /// `@name` is for the sender: `todo::add` treats `None` that way too).
    pub(crate) async fn typed(conn: &mut SqliteConnection, ctx: &Context, text: &str) -> Outcome {
        let command = crate::parser::parse(text).expect("should parse");
        run_command(conn, ctx, &Apis::new(times()), command)
            .await
            .unwrap()
    }

    /// The reply `command` gives, as text. Panics on anything else, so a
    /// test fails loudly if the command errors instead of answering.
    pub(crate) async fn reply_to(
        conn: &mut SqliteConnection,
        ctx: &Context,
        apis: &Apis,
        command: BotCommand,
    ) -> String {
        match run_command(conn, ctx, apis, command).await {
            Ok(Outcome::Reply(text)) => text,
            other => panic!("expected a reply, got {other:?}"),
        }
    }

    // ---- whose settings apply (`my_settings`) ----
    //
    // Pinning tests: they describe what the code does today, so a refactor
    // can't change it by accident. The places are picked so each field
    // shows where it came from: the defaults are Milton Keynes on
    // Europe/London, the group is Colombo on Asia/Colombo, and Bilal's own
    // timezone (set in his DM) is America/New_York.

    const NEW_YORK: Tz = chrono_tz::America::New_York;
    const COLOMBO: Tz = chrono_tz::Asia::Colombo;

    /// Gives a chat Colombo as its town and timezone.
    async fn set_colombo(conn: &mut SqliteConnection, chat_id: i64) {
        let at = Coordinates {
            latitude: 6.9271,
            longitude: 79.8612,
        };
        stored::set_location(conn, chat_id, "Colombo", "LK", at, "Asia/Colombo")
            .await
            .unwrap();
    }

    /// `user` sending a message in chat `chat_id`.
    fn ctx_in(chat_id: i64, is_group: bool, user: &User) -> Context {
        Context {
            chat_id,
            is_group,
            sender: user.clone(),
            now: utc("2026-11-02T12:00:00Z"),
        }
    }

    #[tokio::test]
    async fn my_settings_in_a_dm_is_the_dms_own_settings() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T12:00:00Z")).await;
        stored::set_timezone(&mut conn, family.dm, "America/New_York")
            .await
            .unwrap();
        // The group's settings don't reach the DM at all.
        set_colombo(&mut conn, family.group).await;

        let ctx = ctx_in(family.dm, false, &family.bilal);
        let settings = my_settings(&mut conn, &ctx, &defaults()).await.unwrap();
        assert_eq!(settings.timezone, NEW_YORK);
        assert_eq!(settings.city, "Milton Keynes", "the default town");
    }

    #[tokio::test]
    async fn my_settings_in_a_dm_without_own_settings_is_the_defaults() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T12:00:00Z")).await;
        set_colombo(&mut conn, family.group).await;

        let ctx = ctx_in(family.dm, false, &family.bilal);
        let settings = my_settings(&mut conn, &ctx, &defaults()).await.unwrap();
        assert_eq!(settings, defaults());
    }

    #[tokio::test]
    async fn my_settings_in_a_group_uses_personal_timezone_over_the_groups() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T12:00:00Z")).await;
        set_colombo(&mut conn, family.group).await;
        stored::set_timezone(&mut conn, family.dm, "America/New_York")
            .await
            .unwrap();

        let ctx = ctx_in(family.group, true, &family.bilal);
        let settings = my_settings(&mut conn, &ctx, &defaults()).await.unwrap();
        assert_eq!(settings.timezone, NEW_YORK, "his own timezone wins");
        // Field by field: he only set a timezone, so the town is still the
        // group's. Colombo's prayer times, shown on New York's clock.
        assert_eq!(settings.city, "Colombo");
    }

    #[tokio::test]
    async fn my_settings_in_a_group_without_personal_settings_is_the_groups() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T12:00:00Z")).await;
        set_colombo(&mut conn, family.group).await;

        // Bilal has a DM, but never changed anything in it.
        let ctx = ctx_in(family.group, true, &family.bilal);
        let settings = my_settings(&mut conn, &ctx, &defaults()).await.unwrap();
        assert_eq!(
            (settings.timezone, settings.city.as_str()),
            (COLOMBO, "Colombo")
        );

        // Khadija has no DM at all: the same.
        let ctx = ctx_in(family.group, true, &family.khadija);
        let settings = my_settings(&mut conn, &ctx, &defaults()).await.unwrap();
        assert_eq!(
            (settings.timezone, settings.city.as_str()),
            (COLOMBO, "Colombo")
        );
    }

    #[tokio::test]
    async fn my_settings_in_a_group_without_its_own_uses_personal_over_the_defaults() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T12:00:00Z")).await;
        stored::set_timezone(&mut conn, family.dm, "America/New_York")
            .await
            .unwrap();

        let ctx = ctx_in(family.group, true, &family.bilal);
        let settings = my_settings(&mut conn, &ctx, &defaults()).await.unwrap();
        assert_eq!(settings.timezone, NEW_YORK);
        assert_eq!(settings.city, "Milton Keynes");
    }

    #[tokio::test]
    async fn my_settings_in_a_group_still_uses_a_dm_marked_left() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T12:00:00Z")).await;
        set_colombo(&mut conn, family.group).await;
        stored::set_timezone(&mut conn, family.dm, "America/New_York")
            .await
            .unwrap();
        // The bot only ever marks groups as left; this is done by hand, to
        // show `stored::personal` doesn't look at `left_at`.
        let left = Some(utc("2026-11-01T13:00:00Z"));
        crate::repo::cleanup::set_left(&mut conn, family.dm, left)
            .await
            .unwrap();

        let ctx = ctx_in(family.group, true, &family.bilal);
        let settings = my_settings(&mut conn, &ctx, &defaults()).await.unwrap();
        assert_eq!(settings.timezone, NEW_YORK);
    }
}

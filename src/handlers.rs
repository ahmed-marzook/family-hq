//! What the bot does for each command: database changes, then the text to
//! send back. No GOWA here; the webhook does the sending.
//!
//! A command runs in two steps: `prepare` does any lookups it needs from
//! outside services (prayer times, towns, the weather) before the webhook
//! opens its write transaction, and `execute` does the database work inside
//! it, without the network.

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use sqlx::SqliteConnection;
use tracing::warn;

use crate::Apis;
use crate::calendar;
use crate::config::PushConfig;
use crate::error::AppError;
use crate::geo::{Coordinates, NearestPlace};
use crate::help;
use crate::moon::{self, Moon};
use crate::parser::{
    BotCommand, Checkin, Got, PushCommand, ReplyCommand, SettingsCommand, Timing, TodoFilter,
    TodoView,
};
use crate::person::Assignee;
use crate::points::{self, Finish, Source};
use crate::prayer::{self, DayTimes, Prayer};
use crate::prayer_stats::{self, Status, Summary};
use crate::progress;
use crate::push;
use crate::repo::messages::{self, ListItem, ListKind, SentList};
use crate::repo::points as stored_points;
use crate::repo::prayers;
use crate::repo::push::{self as push_repo, Subscription};
use crate::repo::reports as stored_reports;
use crate::repo::settings::{self as stored, MemberSettings};
use crate::repo::shop::{self, ShopItem};
use crate::repo::todo_reminders;
use crate::repo::todos::{self, Details, ListedTodo};
use crate::repo::users::{self, User};
use crate::reports::{Delivery, Feature, Period, Span};
use crate::settings::{self, Settings};
use crate::settings_edit::{self, Change, Checked, Refused, Saved};
use crate::todo_flags::{self, Due, Priority};
use crate::weather::{self, When};

const ADDED: &str = "📝";
const LIST: &str = "📋";
const DONE: &str = "✅";
const SKIPPED: &str = "⏭️";
const DUE: &str = "📅";
const OVERDUE: &str = "⚠️";
const REMOVED: &str = "🗑️";
const PROBLEM: &str = "❓";
const MUTED: &str = "🔕";
const SHOP: &str = "🛒";
const MOSQUE: &str = "🕌";
const NOT_YET: &str = "⬜";
const BELL: &str = "🔔";
const BELL_OFF: &str = "🔕";
const CLEARED: &str = "🧹";
const GEAR: &str = "⚙️";
const PIN: &str = "📍";
const CLOCK: &str = "🕰️";
const PEOPLE: &str = "👥";
const MOON: &str = "🌙";
const CHART: &str = "📊";

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
    /// `/prayers`: the sender's settings and today's times, or why there
    /// are none.
    Prayers {
        settings: Settings,
        today: Result<DayTimes, AppError>,
    },
    /// `+prayed`: the sender's settings, today's times and (when they can
    /// matter) yesterday's, or why there are none.
    Prayed {
        checkin: Checkin,
        settings: Settings,
        times: Result<(DayTimes, Option<DayTimes>), AppError>,
    },
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

/// What a `/settings` command does.
enum SettingsStep {
    Show,
    Everyone,
    Methods,
    /// A change, checked by `settings_edit::check`: what to save, or why
    /// it's refused.
    Change(Result<Checked, Refused>),
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
            Ran::TestPush(subscription) => Outcome::Reply(test_push(apis, &subscription).await),
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
    let defaults = &apis.prayer_times.defaults;
    let step = match command {
        BotCommand::Prayers => {
            let settings = my_settings(conn, ctx, defaults).await?;
            let today = lookup_day(apis, &settings, settings.today(ctx.now)).await;
            Step::Prayers { settings, today }
        }
        BotCommand::Prayed(checkin) => {
            // For people who didn't catch the reminder: with no timing, the
            // same as reacting to it. Today (or yesterday) is in the
            // sender's own timezone.
            let settings = my_settings(conn, ctx, defaults).await?;
            let today = settings.today(ctx.now);
            let yesterday = today.pred_opt().unwrap_or(today);
            // Every lookup that needs the network happens here, before
            // anything is saved, so if a service is down nothing is ticked
            // and the reply says so. An `async` block is like a small
            // function inline: `?` inside it ends the block, not
            // `prepare`, so all three lookups share one result.
            let lookups = async {
                let location = apis.prayer_location(&settings).await?;
                let today_times = apis.prayer_times.for_date(&location, today).await?;
                // Only looked up between midnight and Fajr, when last night's
                // Isha may still be going (see `prayer::checkin_date`), or
                // for `yesterday`.
                let yesterday_times =
                    if checkin.yesterday || prayer::before_fajr(&today_times, ctx.now) {
                        Some(apis.prayer_times.for_date(&location, yesterday).await?)
                    } else {
                        None
                    };
                // `Ok::<_, AppError>` names the error type, which the
                // compiler can't work out from the `?`s on their own.
                Ok::<_, AppError>((today_times, yesterday_times))
            };
            let times = lookups.await;
            Step::Prayed {
                checkin,
                settings,
                times,
            }
        }
        BotCommand::Weather(when) => Step::Answered(weather(conn, ctx, apis, when).await?),
        BotCommand::Moon => Step::Answered(moon_command(conn, ctx, apis).await?),
        BotCommand::SharedLocation {
            latitude,
            longitude,
        } => {
            let at = Coordinates {
                latitude,
                longitude,
            };
            let found = find_place(conn, ctx, apis, at).await?;
            Step::SharedLocation { at, found }
        }
        BotCommand::Settings(command) => {
            Step::Settings(settings_step(conn, ctx, apis, command).await?)
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
        Step::Prayers { settings, today } => match today {
            Ok(day) => format_day(conn, ctx, &settings, &day).await?,
            Err(err) => prayer_service_down(err)?,
        },
        Step::Prayed {
            checkin,
            settings,
            times,
        } => prayed(conn, ctx, checkin, &settings, times).await?,
        Step::SharedLocation { at, found } => save_place(conn, ctx, defaults, at, found).await?,
        Step::Settings(step) => settings_command(conn, ctx, defaults, step).await?,
        Step::Push(command) => return push_command(conn, ctx, offline, command).await,
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
        } => add_todo(conn, ctx, defaults, &titles, assignee, due, priority).await?,
        BotCommand::ListTodos(view) => {
            let layout = match view {
                TodoView::ByPerson => Layout::ByPerson,
                TodoView::All => Layout::All,
                TodoView::Mine => Layout::Only(ctx.sender.display_name.clone()),
                TodoView::Filtered(filter) => Layout::Filtered(filter),
                // Checked against the chat's members, so a typo gets the
                // list of names rather than "Bilql has no todos".
                TodoView::Person(name) => {
                    match users::find_member_by_name(conn, ctx.chat_id, &name).await? {
                        Some(user) => Layout::Only(user.display_name),
                        None => {
                            let known = users::member_names(conn, ctx.chat_id).await?;
                            return Ok(Outcome::Reply(unknown_name(&name, &known)));
                        }
                    }
                }
            };
            // The sender's timezone, for the "done at 14:05" times.
            let settings = my_settings(conn, ctx, defaults).await?;
            let items = todos::list_all(conn, ctx.chat_id).await?;
            return Ok(Outcome::List {
                text: format_todo_list(&items, &layout, &settings, ctx.now),
                kind: ListKind::Todo,
                // Every todo, even in one person's view, so the numbers
                // mean the same todo in every view.
                item_ids: items.iter().map(|t| t.id).collect(),
            });
        }
        BotCommand::BadTodo(err) => format!("{PROBLEM} {err}"),
        BotCommand::DoneTodo(Got::Numbers(numbers)) => {
            return close_by_numbers(conn, ctx, defaults, &numbers, Action::Tick).await;
        }
        BotCommand::DoneTodo(Got::Names(names)) => {
            close_by_name(conn, ctx, defaults, &names, Action::Tick).await?
        }
        BotCommand::SkipTodo(Got::Numbers(numbers)) => {
            return close_by_numbers(conn, ctx, defaults, &numbers, Action::Skip).await;
        }
        BotCommand::SkipTodo(Got::Names(names)) => {
            close_by_name(conn, ctx, defaults, &names, Action::Skip).await?
        }
        BotCommand::ClearTodos { done_only: false } => {
            match todos::clear_all(conn, ctx.chat_id).await? {
                0 => format!("{LIST} There are no todos to clear."),
                1 => format!("{CLEARED} Cleared the todo list (1 todo)."),
                n => format!("{CLEARED} Cleared the todo list ({n} todos)."),
            }
        }
        BotCommand::ClearTodos { done_only: true } => {
            // Counted first, to say how many of each went.
            let skipped = todos::progress(conn, ctx.chat_id).await?.skipped;
            match (todos::clear_done(conn, ctx.chat_id).await?, skipped) {
                (0, _) => format!("{LIST} No completed todos to clear."),
                (1, 0) => format!("{CLEARED} Cleared 1 completed todo."),
                (n, 0) => format!("{CLEARED} Cleared {n} completed todos."),
                (n, skipped) => format!(
                    "{CLEARED} Cleared {} completed and {skipped} skipped.",
                    n - skipped as u64
                ),
            }
        }
        BotCommand::TodoReminders(on) => {
            let (chat_id, user_id) = (ctx.chat_id, ctx.sender.id);
            match on {
                Some(true) => todo_reminders::enable(conn, chat_id, user_id, ctx.now).await?,
                Some(false) => todo_reminders::disable(conn, chat_id, user_id).await?,
                None => {}
            }
            let is_on = todo_reminders::is_on(conn, chat_id, user_id).await?;
            format_todo_reminders(is_on, ctx.is_group)
        }
        BotCommand::AddShop(names) => add_shop(conn, ctx, &names).await?,
        BotCommand::ListShop => {
            let items = shop::list_all(conn, ctx.chat_id).await?;
            return Ok(Outcome::List {
                text: format_shop_list(&items),
                kind: ListKind::Shop,
                item_ids: items.iter().map(|i| i.id).collect(),
            });
        }
        BotCommand::GotShop(Got::Numbers(numbers)) => {
            // Same as replying `1 3` to the newest `/shop` list here.
            match messages::latest_list_items(conn, ctx.chat_id, ListKind::Shop).await? {
                None => format!(
                    "{PROBLEM} No shopping list sent here yet. Send /shop, then `+got 1 3`."
                ),
                Some(list) => {
                    return run_list_reply(conn, ctx, defaults, &list, ReplyCommand::Tick(numbers))
                        .await;
                }
            }
        }
        BotCommand::GotShop(Got::Names(names)) => return got_by_name(conn, ctx, &names).await,
        BotCommand::ClearShop { bought_only: false } => {
            match shop::clear_all(conn, ctx.chat_id).await? {
                0 => format!("{SHOP} The shopping list is already empty."),
                1 => format!("{CLEARED} Cleared the shopping list (1 item)."),
                n => format!("{CLEARED} Cleared the shopping list ({n} items)."),
            }
        }
        BotCommand::ClearShop { bought_only: true } => {
            match shop::clear_bought(conn, ctx.chat_id).await? {
                0 => format!("{SHOP} Nothing bought to clear."),
                1 => format!("{CLEARED} Cleared 1 bought item."),
                n => format!("{CLEARED} Cleared {n} bought items."),
            }
        }
        BotCommand::PrayerReminders(on) => {
            stored::set_prayer_reminders(conn, ctx.chat_id, on).await?;
            let (settings, _) = chat_settings(conn, ctx, defaults).await?;
            if on {
                if ctx.is_group {
                    format!(
                        "{BELL} Prayer reminders are on in this group: when each prayer starts, \
                         and before it ends. Each member is @mentioned on *their own* times \
                         (set yours by messaging me `/settings city …` privately; otherwise \
                         this group's {} times are used). React to a reminder once you've prayed.",
                        settings.city
                    )
                } else {
                    format!(
                        "{BELL} Prayer reminders are on ({}): when each prayer starts, and {} min \
                         before it ends. React to a reminder once you've prayed.",
                        settings.city, settings.prayer_ending_minutes
                    )
                }
            } else {
                format!("{BELL_OFF} Prayer reminders are off in this chat.")
            }
        }
        BotCommand::PrayerSummary(span) => {
            let settings = my_settings(conn, ctx, defaults).await?;
            let period = Period::of(span, settings.today(ctx.now));
            let log = prayers::log_between(conn, ctx.sender.id, period.first, period.last).await?;
            let summary = Summary::new(period, &log);
            if summary.is_empty() {
                let span = match span {
                    Span::Week => "week",
                    Span::Month => "month",
                };
                format!(
                    "{MOSQUE} Nothing recorded yet this {span}. Turn on reminders with \
                     `/prayers on`, then react to each one once you've prayed."
                )
            } else {
                prayer_stats::summary_text(span, period, &[(None, summary)])
            }
        }
        BotCommand::PrayerReports(None) => {
            let delivery = stored_reports::delivery(conn, ctx.chat_id, Feature::Prayers).await?;
            format_reports(delivery, ctx.is_group)
        }
        BotCommand::PrayerReports(Some(delivery)) => {
            // In a DM, "privately" is the same as "here".
            let delivery = if ctx.is_group || delivery != Delivery::Dm {
                delivery
            } else {
                Delivery::Here
            };
            stored_reports::set_delivery(conn, ctx.chat_id, Feature::Prayers, delivery).await?;
            format_reports(delivery, ctx.is_group)
        }
        BotCommand::Points => points_command(conn, ctx, defaults).await?,
        BotCommand::PointsReports(_) if !ctx.is_group => format!(
            "{CHART} The Sunday points summary is for groups. Here, `/points` shows \
             your own week any time."
        ),
        BotCommand::PointsReports(change) => {
            let delivery = match change {
                Some(delivery) => {
                    stored_reports::set_delivery(conn, ctx.chat_id, Feature::Points, delivery)
                        .await?;
                    delivery
                }
                None => stored_reports::delivery(conn, ctx.chat_id, Feature::Points).await?,
            };
            format_points_reports(delivery)
        }
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

/// `/push ...`: the sender's own push notifications (`crate::push`).
/// Personal, so only in their DM: in a group the reply (with the topic)
/// would be seen by everyone.
async fn push_command(
    conn: &mut SqliteConnection,
    ctx: &Context,
    offline: &Offline<'_>,
    command: PushCommand,
) -> Result<Ran, AppError> {
    let reply = |text: String| Ok(Ran::Done(Outcome::Reply(text)));
    let Some(config) = offline.push else {
        return reply(not_set_up());
    };
    if ctx.is_group {
        return reply(format!(
            "{BELL} Push notifications are personal: send `/push on` in your own chat with me."
        ));
    }
    let user_id = ctx.sender.id;
    let text = match command {
        PushCommand::Status => match push_repo::subscription(conn, user_id).await? {
            Some(sub) => format!(
                "{BELL} Push notifications are on, on the topic `{}`. `/push test` sends one, \
                 `/push on` shows the setup steps again (for a new phone) and `/push off` stops them.",
                sub.topic
            ),
            None => format!(
                "{BELL} Push notifications are off. `/push on` sets them up: prayer \
                 reminders then come to the ntfy app as well, with a button to tick them off."
            ),
        },
        PushCommand::On => {
            let sub =
                push_repo::subscribe(conn, user_id, &push::new_subscription(), ctx.now).await?;
            // The steps, then the server and the topic on their own, so
            // each can be copied into the app as it is.
            return Ok(Ran::Done(Outcome::Several(push::subscribe_messages(
                config, &sub.topic,
            ))));
        }
        PushCommand::Off => {
            if push_repo::unsubscribe(conn, user_id).await? {
                format!(
                    "{MUTED} Push notifications are off. You can delete the subscription in the ntfy app."
                )
            } else {
                format!("{MUTED} Push notifications were already off.")
            }
        }
        PushCommand::Test => match push_repo::subscription(conn, user_id).await? {
            None => format!("{BELL} Push is off. Send `/push on` first."),
            // Sent by `Ran::finish`, once the transaction is saved.
            Some(sub) => return Ok(Ran::TestPush(sub)),
        },
    };
    reply(text)
}

fn not_set_up() -> String {
    format!("{BELL} Push notifications aren't set up on this server yet.")
}

/// `/push test`'s push, and the reply saying whether it went.
async fn test_push(apis: &Apis, subscription: &Subscription) -> String {
    // `push_command` only asks for this when push is set up.
    let Some(ntfy) = &apis.push else {
        return not_set_up();
    };
    match ntfy
        .publish(&ntfy.message(subscription, &push::test_notice()))
        .await
    {
        Ok(()) => format!(
            "{BELL} Sent a test push. If it hasn't arrived, check the server and topic \
             in the ntfy app (`/push on` shows them again). Tap its button to check \
             it can reach me too."
        ),
        Err(err) => {
            warn!("test push failed: {err}");
            format!(
                "{PROBLEM} Couldn't send it: the push server didn't answer. Try again in a bit."
            )
        }
    }
}

/// `+got milk, eggs`: ticks each named item off.
async fn got_by_name(
    conn: &mut SqliteConnection,
    ctx: &Context,
    names: &[String],
) -> Result<Outcome, AppError> {
    let mut bought = Vec::new();
    let mut problems = Vec::new();
    for name in names {
        let ticked = match shop::find_to_buy(conn, ctx.chat_id, name).await? {
            Some(id) => shop::mark_bought(conn, id, ctx.chat_id, ctx.sender.id, ctx.now).await?,
            None => None,
        };
        match ticked {
            // The stored name, with the capitals it was added with.
            Some(stored) => bought.push(stored),
            None => problems.push(format!("{PROBLEM} {name} isn't on the list.")),
        }
    }
    Ok(bought_reply(ctx, &bought, &problems))
}

/// "✅ @Bilal bought *milk*, *eggs*" in a group, so everyone sees who got
/// what (the @ is a real WhatsApp mention). In a DM there's no one else to
/// tell: "✅ Bought *milk*, *eggs*". Any `problems` follow on their own
/// lines. With nothing bought it's just the problems.
fn bought_reply(ctx: &Context, bought: &[String], problems: &[String]) -> Outcome {
    let mut lines = Vec::new();
    let mut mentions = Vec::new();
    if !bought.is_empty() {
        let items: Vec<String> = bought.iter().map(|name| format!("*{name}*")).collect();
        let items = items.join(", ");
        if ctx.is_group {
            // "447700900001@s.whatsapp.net" → "447700900001".
            let phone = ctx
                .sender
                .wa_jid
                .split('@')
                .next()
                .unwrap_or_default()
                .to_string();
            lines.push(format!("{DONE} @{phone} bought {items}"));
            mentions.push(phone);
        } else {
            lines.push(format!("{DONE} Bought {items}"));
        }
    }
    lines.extend(problems.iter().cloned());
    let text = lines.join("\n");
    if mentions.is_empty() {
        Outcome::Reply(text)
    } else {
        Outcome::Mentioning { text, mentions }
    }
}

/// Adds each item that isn't already on the list (ignoring case), and says
/// which were added and which were already there.
async fn add_shop(
    conn: &mut SqliteConnection,
    ctx: &Context,
    names: &[String],
) -> Result<String, AppError> {
    let mut added: Vec<&str> = Vec::new();
    let mut already: Vec<&str> = Vec::new();
    for name in names {
        // Also catches the same item twice in one message ("milk, Milk"):
        // the first one is on the list by the time the second is checked.
        if shop::is_on_list(conn, ctx.chat_id, name).await? {
            already.push(name);
        } else {
            shop::add(conn, ctx.chat_id, name, ctx.sender.id, ctx.now).await?;
            added.push(name);
        }
    }
    Ok(format_shop_added(&added, &already))
}

/// This chat's settings with the defaults filled in, and whether prayer
/// reminders are on.
async fn chat_settings(
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

/// Why a `/settings` change wasn't made, as the bot says it.
fn format_refused(refused: Refused) -> String {
    match refused {
        Refused::UnknownTown { city, country } => format!(
            "{PROBLEM} I couldn't find {city}, {country}. \
             Try e.g. `/settings city Leicester, GB`."
        ),
        Refused::UnknownTimezone(name) => format!(
            "{PROBLEM} I don't know the timezone {name}. Use a name like \
             Europe/London or Asia/Karachi."
        ),
        Refused::UnknownMethod(method) => {
            format!("{PROBLEM} There's no method {method}. /settings methods lists them.")
        }
        // The parser only ever gives Shafi'i or Hanafi, but just in case.
        Refused::UnknownSchool(_) => format!("{PROBLEM} Use `/settings asr hanafi` or `shafi`."),
        Refused::EndingOutOfRange(_) => format!(
            "{PROBLEM} Pick between {} and {} minutes.",
            settings_edit::ENDING_MINUTES.start(),
            settings_edit::ENDING_MINUTES.end()
        ),
    }
}

/// A location shared in a DM, step one (in `prepare`): the nearest town
/// (OpenStreetMap), and the timezone AlAdhan gives for that point. `Err`
/// is the reply when the town can't be worked out.
async fn find_place(
    conn: &mut SqliteConnection,
    ctx: &Context,
    apis: &Apis,
    at: Coordinates,
) -> Result<Result<(NearestPlace, Tz), String>, AppError> {
    let (current, _) = chat_settings(conn, ctx, &apis.prayer_times.defaults).await?;
    // `Geo` means OpenStreetMap answered but found no town there; any
    // other error means it didn't answer properly, which is worth a log
    // line and a different reply ("try again", not "try something else").
    let near = match apis.geo.nearest_place(at).await {
        Ok(near) => near,
        Err(AppError::Geo(_)) => {
            return Ok(Err(format!(
                "{PROBLEM} I couldn't work out which town that is. \
                 Try `/settings city Leicester, GB` instead."
            )));
        }
        Err(err) => {
            warn!("nearest town lookup failed: {err}");
            return Ok(Err(format!(
                "{PROBLEM} The map service isn't answering right now, so I couldn't \
                 work out which town that is. Try again in a bit."
            )));
        }
    };
    let timezone = apis
        .prayer_times
        .timezone_at(at, current.today(ctx.now))
        .await
        .unwrap_or(current.timezone);
    Ok(Ok((near, timezone)))
}

/// Step two (in `execute`): saves the exact point, named after the nearest
/// town, with that timezone. Prayer times are then fetched for the point
/// itself, so no name ever has to be looked up.
async fn save_place(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    at: Coordinates,
    found: Result<(NearestPlace, Tz), String>,
) -> Result<String, AppError> {
    let (near, timezone) = match found {
        Ok(found) => found,
        Err(reply) => return Ok(reply),
    };
    // `names[0]` is the most specific: the town or village itself.
    let name = &near.names[0];
    stored::set_location(conn, ctx.chat_id, name, &near.country, at, timezone.name()).await?;

    let (settings, reminders) = chat_settings(conn, ctx, defaults).await?;
    Ok(format!(
        "{PIN} Got it: you're near *{name}, {}* ({timezone}). Prayer times, \
         reminders and weather now use your exact location.\n\n{}",
        near.country,
        format_settings(&settings, reminders, ctx.is_group)
    ))
}

/// `/weather`: the forecast for the sender's own city (their DM settings,
/// or this chat's), in their timezone.
async fn weather(
    conn: &mut SqliteConnection,
    ctx: &Context,
    apis: &Apis,
    when: When,
) -> Result<String, AppError> {
    let settings = my_settings(conn, ctx, &apis.prayer_times.defaults).await?;
    // As in `shared_location`: `Geo` is "no such town", anything else is
    // the town lookup not answering.
    let at = match apis.coordinates(&settings).await {
        Ok(at) => at,
        Err(AppError::Geo(_)) => {
            return Ok(format!(
                "{PROBLEM} I couldn't find {}, {} for the weather. Check `/settings`.",
                settings.city, settings.country
            ));
        }
        Err(err) => {
            warn!("town lookup for the weather failed: {err}");
            return Ok(format!(
                "{PROBLEM} The town lookup isn't answering right now. Try again in a bit."
            ));
        }
    };
    match apis.weather.forecast(at, settings.timezone).await {
        Ok(forecast) => Ok(weather::format(&forecast, when, &settings.city)),
        Err(err) => {
            // Not "weather: {err}": `AppError::Weather`'s text already
            // starts with "weather:".
            warn!("forecast failed: {err}");
            Ok(format!(
                "{PROBLEM} The weather service isn't answering. Try again in a bit."
            ))
        }
    }
}

/// `/moon`: the phase now, today's moonrise and moonset in the sender's
/// town, and the next new and full moons. The phase is worked out here;
/// only the rise and set times need the network, and if that fails (or
/// the town can't be found) the rest still shows.
async fn moon_command(
    conn: &mut SqliteConnection,
    ctx: &Context,
    apis: &Apis,
) -> Result<String, AppError> {
    let settings = my_settings(conn, ctx, &apis.prayer_times.defaults).await?;
    let today = settings.today(ctx.now);

    // `.ok()` turns each `Result` into an `Option`: a failed lookup is
    // "not shown", not an error for the whole reply.
    // `inspect_err` looks at the error (to log it) without using it up,
    // so `.ok()` after it still works as before.
    let at = apis
        .coordinates(&settings)
        .await
        .inspect_err(|err| warn!("town lookup for the moon failed: {err}"))
        .ok();
    let rise_set = match at {
        Some(at) => apis
            .weather
            .moon_times(at, settings.timezone, today)
            .await
            .inspect_err(|err| warn!("moon times: {err}"))
            .ok(),
        None => None,
    };
    // The Hijri date comes with the prayer times (usually already cached).
    let hijri = match apis.prayer_location(&settings).await {
        Ok(location) => apis
            .prayer_times
            .for_date(&location, today)
            .await
            .inspect_err(|err| warn!("Hijri date for the moon failed: {err}"))
            .ok()
            .and_then(|day| day.hijri),
        Err(err) => {
            warn!("prayer location for the moon failed: {err}");
            None
        }
    };
    Ok(moon::format(ctx.now, &settings, rise_set, hijri))
}

/// The sender's own settings: in a DM, the DM's; in a group, their personal
/// ones (from their DM with the bot) on top of the group's. Prayer times
/// and "today" follow the person, so someone abroad sees their own.
async fn my_settings(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
) -> Result<Settings, AppError> {
    let person = Some(ctx.sender.id);
    Ok(stored::effective(conn, defaults, ctx.chat_id, person).await?)
}

/// The prayer times for these settings on `date`: from the cache, or
/// AlAdhan (and, for a town without coordinates, the geocoder). Network,
/// so only ever called from `prepare`.
async fn lookup_day(
    apis: &Apis,
    settings: &Settings,
    date: NaiveDate,
) -> Result<DayTimes, AppError> {
    let location = apis.prayer_location(settings).await?;
    apis.prayer_times.for_date(&location, date).await
}

/// A day's prayer times, with the sender's ticks.
async fn format_day(
    conn: &mut SqliteConnection,
    ctx: &Context,
    settings: &Settings,
    day: &DayTimes,
) -> Result<String, AppError> {
    let date = day.date;
    let log: Vec<(Prayer, Status)> = prayers::log_between(conn, ctx.sender.id, date, date)
        .await?
        .into_iter()
        .map(|e| (e.prayer, e.status))
        .collect();
    Ok(format_prayer_day(day, settings, &log, Moon::at(ctx.now)))
}

/// `+prayed`, with the times `prepare` looked up: ticks each prayer, then
/// shows each day that changed.
async fn prayed(
    conn: &mut SqliteConnection,
    ctx: &Context,
    checkin: Checkin,
    settings: &Settings,
    times: Result<(DayTimes, Option<DayTimes>), AppError>,
) -> Result<String, AppError> {
    let (today_times, yesterday_times) = match times {
        Ok(times) => times,
        Err(err) => return prayer_service_down(err),
    };
    let today = settings.today(ctx.now);
    let yesterday = today.pred_opt().unwrap_or(today);
    let mut dates = Vec::new();
    for prayer in checkin.prayers {
        let date = if checkin.yesterday {
            yesterday
        } else {
            prayer::checkin_date(prayer, &today_times, yesterday_times.as_ref(), ctx.now)
        };
        if !dates.contains(&date) {
            dates.push(date);
        }
        let user = ctx.sender.id;
        match checkin.timing {
            Timing::Auto => {
                prayers::mark_prayed(conn, user, date, prayer, ctx.now).await?;
            }
            Timing::OnTime => {
                prayers::set_prayed(conn, user, date, prayer, true, ctx.now).await?;
            }
            Timing::Late => {
                prayers::set_prayed(conn, user, date, prayer, false, ctx.now).await?;
            }
        }
    }
    // Each day that changed, in order: usually one, but `+prayed
    // isha fajr` just after midnight ticks last night's Isha and
    // today's Fajr.
    // Each date is today's or yesterday's, and both days' times came
    // from `prepare` (`checkin_date` only picks yesterday when they're
    // there), so nothing is looked up here.
    dates.sort();
    let mut days = Vec::new();
    for date in dates {
        let day = [Some(&today_times), yesterday_times.as_ref()]
            .into_iter()
            .flatten()
            .find(|day| day.date == date)
            .ok_or_else(|| AppError::PrayerTimes(format!("no times looked up for {date}")))?;
        days.push(format_day(conn, ctx, settings, day).await?);
    }
    Ok(days.join("\n\n"))
}

/// What to say when a prayer-times lookup fails because a service is down
/// or answered with something unusable: AlAdhan (`Http`, `PrayerTimes`)
/// or the town lookup (`Geo`). The real error goes to the log.
///
/// Any other error, in practice the database, is given back as it is:
/// the webhook then answers 500 and GOWA retries, which can help there.
///
/// `err @ (A | B)` matches either variant and still names the whole value
/// `err`, so it can be logged.
fn prayer_service_down(err: AppError) -> Result<String, AppError> {
    match err {
        err @ (AppError::Http(_) | AppError::PrayerTimes(_) | AppError::Geo(_)) => {
            warn!("prayer times lookup failed: {err}");
            Ok(format!(
                "{PROBLEM} The prayer-times service isn't answering right now. Try again in a bit."
            ))
        }
        other => Err(other),
    }
}

/// `/settings ...`, step one (in `prepare`): which kind it is, and for a
/// change, its checks, including looking a new town up.
async fn settings_step(
    conn: &mut SqliteConnection,
    ctx: &Context,
    apis: &Apis,
    command: SettingsCommand,
) -> Result<SettingsStep, AppError> {
    // The changes are checked and saved by `settings_edit`, under the same
    // rules the admin dashboard uses; only the wording here is the bot's.
    let change = match command {
        SettingsCommand::Show => return Ok(SettingsStep::Show),
        SettingsCommand::Everyone => return Ok(SettingsStep::Everyone),
        SettingsCommand::Methods => return Ok(SettingsStep::Methods),
        SettingsCommand::Reset => Change::Reset,
        SettingsCommand::City { city, country } => {
            // No country typed: keep the one already in use.
            let country = match country {
                Some(country) => country,
                None => {
                    let defaults = &apis.prayer_times.defaults;
                    chat_settings(conn, ctx, defaults).await?.0.country
                }
            };
            Change::City { city, country }
        }
        SettingsCommand::Timezone(name) => Change::Timezone(name),
        SettingsCommand::PrayerMethod(method) => Change::PrayerMethod(method),
        SettingsCommand::PrayerSchool(school) => Change::PrayerSchool(school),
        SettingsCommand::PrayerEnding(minutes) => Change::PrayerEnding(minutes),
    };
    Ok(SettingsStep::Change(
        settings_edit::check(&apis.geo, &change).await,
    ))
}

/// Step two (in `execute`): saves a checked change, then the reply shows
/// the chat's full settings so people see the result.
async fn settings_command(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    step: SettingsStep,
) -> Result<String, AppError> {
    let id = ctx.chat_id;
    // `note` is an extra first line for the reply; most changes have none.
    let note = match step {
        SettingsStep::Show => None,
        SettingsStep::Everyone => {
            if !ctx.is_group {
                return Ok(format!("{PEOPLE} In a DM there's only you: see /settings."));
            }
            let members = stored::members(conn, id).await?;
            return Ok(format_everyone(&members, defaults));
        }
        SettingsStep::Methods => return Ok(format_methods()),
        // Refused: the explanation is the whole reply.
        SettingsStep::Change(Err(refused)) => return Ok(format_refused(refused)),
        SettingsStep::Change(Ok(checked)) => {
            let reset = checked == Checked::Reset;
            let (current, _) = chat_settings(conn, ctx, defaults).await?;
            match settings_edit::save(conn, id, &current, checked).await? {
                Saved::Town { name, timezone } => Some(format!(
                    "Found {name}. Timezone set to {timezone} to match."
                )),
                Saved::Done if reset => Some("Back to the defaults.".to_string()),
                Saved::Done => None,
            }
        }
    };

    // Read back what's now stored, so the reply shows exactly what's saved.
    let (settings, reminders) = chat_settings(conn, ctx, defaults).await?;
    let mut text = note.map(|n| format!("{n}\n\n")).unwrap_or_default();
    text.push_str(&format_settings(&settings, reminders, ctx.is_group));
    Ok(text)
}

async fn add_todo(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    titles: &[String],
    assignee: Option<Assignee>,
    due: Option<Due>,
    priority: Option<Priority>,
) -> Result<String, AppError> {
    let assignee = match assignee {
        // No `@name` (the webhook fills in the sender) → the sender.
        None | Some(Assignee::Sender(_)) => ctx.sender.clone(),
        Some(Assignee::Mentioned(name)) => {
            match users::find_member_by_name(conn, ctx.chat_id, &name).await? {
                Some(user) => user,
                None => {
                    let known = users::member_names(conn, ctx.chat_id).await?;
                    return Ok(unknown_name(&name, &known));
                }
            }
        }
    };

    // "fri" → a date, counting from the sender's own today (their
    // timezone, even in a group set to another one).
    let today = my_settings(conn, ctx, defaults).await?.today(ctx.now);
    // `transpose` turns `Option<Result<..>>` into `Result<Option<..>>`, so
    // one `match` covers "no date", "a date" and "a bad date".
    let due = match due.map(|due| due.resolve(today)).transpose() {
        Ok(due) => due,
        Err(err) => return Ok(format!("{PROBLEM} {err}")),
    };
    let details = Details { due, priority };

    // All in the webhook's one transaction: if one fails, none are added.
    for title in titles {
        todos::add(
            conn,
            ctx.chat_id,
            title,
            ctx.sender.id,
            Some(assignee.id),
            details,
            ctx.now,
        )
        .await?;
    }
    Ok(format_todos_added(
        titles,
        &assignee.display_name,
        details,
        today,
    ))
}

/// "📝 Added *fix the tap* for Bilal", or for several:
/// "📝 Added 2 todos for Bilal:" and one line each. Then the due date and
/// priority, if given, on a line of their own (they're for all of them).
fn format_todos_added(titles: &[String], name: &str, details: Details, today: NaiveDate) -> String {
    let mut text = match titles {
        [title] => format!("{ADDED} Added *{title}* for {name}"),
        _ => {
            let lines: Vec<String> = titles.iter().map(|t| format!("• {t}")).collect();
            format!(
                "{ADDED} Added {} todos for {name}:\n{}",
                titles.len(),
                lines.join("\n")
            )
        }
    };
    if let Some(line) = details_line(details, today) {
        text.push('\n');
        text.push_str(&line);
    }
    text
}

/// "📅 Due Fri 13 Nov · 🔴 High priority", either half, or `None`.
fn details_line(details: Details, today: NaiveDate) -> Option<String> {
    let due = details
        .due
        .map(|date| format!("{DUE} Due {}", todo_flags::show_date(date, today)));
    let priority = details.priority.map(|p| {
        let name = match p {
            Priority::High => "High",
            Priority::Medium => "Medium",
            Priority::Low => "Low",
        };
        format!("{} {name} priority", p.emoji())
    });
    // `[a, b].into_iter().flatten()` keeps only the `Some`s.
    let parts: Vec<String> = [due, priority].into_iter().flatten().collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Runs a reply to one of the bot's numbered lists. `list` is what that
/// list was and showed. `defaults` is for the sender's settings (their
/// "today", for `due 2 fri`).
pub async fn run_list_reply(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    list: &SentList,
    reply: ReplyCommand,
) -> Result<Outcome, AppError> {
    // `done 2` and `1 3` both tick off; `remove` deletes; `skip` closes a
    // todo as "won't be done". The rest change a todo and return here.
    let (numbers, action) = match reply {
        ReplyCommand::Done(n) => (vec![n], Action::Tick),
        ReplyCommand::Tick(numbers) => (numbers, Action::Tick),
        ReplyCommand::Remove(n) => (vec![n], Action::Remove),
        ReplyCommand::Skip(n) => (vec![n], Action::Skip),
        ReplyCommand::SetDue(n, due) => {
            return change_todo(conn, ctx, defaults, list, n, TodoChange::Due(due)).await;
        }
        ReplyCommand::SetPriority(n, priority) => {
            return change_todo(conn, ctx, defaults, list, n, TodoChange::Priority(priority)).await;
        }
        ReplyCommand::BadChange(err) => return Ok(Outcome::Reply(format!("{PROBLEM} {err}"))),
    };
    act_on_list(conn, ctx, defaults, list, &numbers, action).await
}

/// A change to one todo, from a reply (`due 2 fri`, `priority 2 none`).
#[derive(Debug, Clone, Copy)]
enum TodoChange {
    Due(Option<Due>),
    Priority(Option<Priority>),
}

/// `due 2 fri` / `priority 2 high` (or `none`): changes todo 2 in the list
/// that was replied to.
async fn change_todo(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    list: &SentList,
    n: usize,
    change: TodoChange,
) -> Result<Outcome, AppError> {
    let reply = |text: String| Ok(Outcome::Reply(text));
    if list.kind == ListKind::Shop {
        return reply(format!(
            "{PROBLEM} Due dates and priorities are for todos, not the shopping list."
        ));
    }
    let id = match item_at(&list.items, n) {
        Ok(id) => id,
        Err(problem) => return reply(problem),
    };

    let text = match change {
        TodoChange::Due(due) => {
            // As with `+todo -d`: counted from the sender's own today.
            let today = my_settings(conn, ctx, defaults).await?.today(ctx.now);
            let date = match due.map(|due| due.resolve(today)).transpose() {
                Ok(date) => date,
                Err(err) => return reply(format!("{PROBLEM} {err}")),
            };
            let Some(title) = todos::set_due(conn, id, ctx.chat_id, date).await? else {
                return reply(not_open(n));
            };
            match date {
                Some(date) => format!(
                    "{DUE} *{title}* is now due {}.",
                    todo_flags::show_date(date, today)
                ),
                None => format!("{DUE} *{title}* has no due date now."),
            }
        }
        TodoChange::Priority(priority) => {
            let Some(title) = todos::set_priority(conn, id, ctx.chat_id, priority).await? else {
                return reply(not_open(n));
            };
            match priority {
                Some(p) => format!("{} *{title}* is now {} priority.", p.emoji(), p.as_str()),
                None => format!("*{title}* has no priority now."),
            }
        }
    };
    reply(text)
}

/// Why todo `n` can't be changed: it's closed or gone.
fn not_open(n: usize) -> String {
    format!("{PROBLEM} Todo {n} is already done, skipped or removed, so there's nothing to change.")
}

/// What a reply (or `+done` / `+skip`) does to the items it numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Tick off: a todo done, a shopping item bought.
    Tick,
    Remove,
    /// Todos only: closed as "won't be done".
    Skip,
}

impl Action {
    /// How a todo ends up, for its points: `None` for `Remove`, which
    /// neither earns nor costs anything.
    fn finish(self) -> Option<Finish> {
        match self {
            Action::Tick => Some(Finish::Done),
            Action::Skip => Some(Finish::Skipped),
            Action::Remove => None,
        }
    }

    /// The `+` command that does this without replying.
    fn command(self) -> &'static str {
        match self {
            Action::Skip => "+skip",
            Action::Tick | Action::Remove => "+done",
        }
    }
}

/// Does `action` to the items at `numbers` in `list`.
async fn act_on_list(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    list: &SentList,
    numbers: &[usize],
    action: Action,
) -> Result<Outcome, AppError> {
    let is_shop = list.kind == ListKind::Shop;
    if is_shop && action == Action::Skip {
        return Ok(Outcome::Reply(format!(
            "{PROBLEM} `skip` is for todos. To take something off the shopping \
             list, reply `remove 2`."
        )));
    }

    // The closer's own today, for "done by its due date?". Only todos earn
    // points, so a shopping list doesn't need to look it up.
    let today = if is_shop {
        ctx.now.date_naive()
    } else {
        my_settings(conn, ctx, defaults).await?.today(ctx.now)
    };

    let mut lines = Vec::new();
    // Shopping items ticked off: reported together, mentioning the buyer.
    let mut bought = Vec::new();
    // Any todo done or skipped: the reply ends with the updated progress bar.
    let mut closed_any = false;
    for &n in numbers {
        let id = match item_at(&list.items, n) {
            Ok(id) => id,
            Err(problem) => {
                lines.push(problem);
                continue;
            }
        };
        // The repo call that matches the list type and the action. Each
        // returns the item's name, or `None` if it's already gone.
        let (chat, by, now) = (ctx.chat_id, ctx.sender.id, ctx.now);
        // The points a closed todo earned, for its line.
        let mut earned = None;
        let name = match (is_shop, action.finish()) {
            (false, Some(finish)) => {
                // `map` with a closure that also sets `earned`: the title
                // goes on to the `match` below, the points to the line.
                close_todo(conn, ctx, id, finish, today)
                    .await?
                    .map(|(title, points)| {
                        earned = Some(points);
                        title
                    })
            }
            (false, None) => todos::remove(conn, id, chat).await?,
            (true, Some(Finish::Done)) => shop::mark_bought(conn, id, chat, by, now).await?,
            (true, None) => shop::remove(conn, id, chat).await?,
            // Can't happen: answered before the loop.
            (true, Some(Finish::Skipped)) => None,
        };
        match (name, is_shop, action) {
            (None, _, _) => lines.push(gone(n)),
            (Some(name), _, Action::Remove) => lines.push(format!("{REMOVED} Removed *{name}*")),
            (Some(name), false, Action::Tick) => {
                lines.push(format!("{DONE} Done: *{name}*{}", earned_text(earned)));
                closed_any = true;
            }
            (Some(name), false, Action::Skip) => {
                lines.push(format!(
                    "{SKIPPED} Skipped: *{name}*{}",
                    earned_text(earned)
                ));
                closed_any = true;
            }
            (Some(name), true, _) => bought.push(name),
        }
    }
    if is_shop && action == Action::Tick {
        return Ok(bought_reply(ctx, &bought, &lines));
    }
    if closed_any {
        let p = todos::progress(conn, ctx.chat_id).await?;
        lines.push(progress_bar(p.done, p.skipped, p.total));
    }
    Ok(Outcome::Reply(lines.join("\n")))
}

/// `+done 2` / `+skip 1 3`: the same as replying to the newest `/todo`
/// list sent in this chat.
async fn close_by_numbers(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    numbers: &[usize],
    action: Action,
) -> Result<Outcome, AppError> {
    let Some(list) = messages::latest_list_items(conn, ctx.chat_id, ListKind::Todo).await? else {
        let command = action.command();
        return Ok(Outcome::Reply(format!(
            "{PROBLEM} No todo list sent here yet. Send /todo, then `{command} 2`."
        )));
    };
    act_on_list(conn, ctx, defaults, &list, numbers, action).await
}

/// Closes todo `id` in this chat as `finish` (done or skipped) and records
/// the points that earns the sender (`points::for_todo`), counted on their
/// own `today`. Returns its title and the points, or `None` if it was
/// already closed or gone (and so earns nothing).
///
/// No transaction of its own: the webhook runs each command in one, so the
/// todo and its points are saved together or not at all.
async fn close_todo(
    conn: &mut SqliteConnection,
    ctx: &Context,
    id: i64,
    finish: Finish,
    today: NaiveDate,
) -> Result<Option<(String, i64)>, AppError> {
    let (chat, by, now) = (ctx.chat_id, ctx.sender.id, ctx.now);
    let closed = match finish {
        Finish::Done => todos::complete(conn, id, chat, by, now).await?,
        Finish::Skipped => todos::skip(conn, id, chat, by, now).await?,
    };
    // `let ... else`: carry on with the todo if there is one, else return.
    let Some(todo) = closed else {
        return Ok(None);
    };
    let award = points::for_todo(finish, todo.priority, todo.due, today);
    stored_points::award(conn, by, chat, Source::Todo, id, award, today, now).await?;
    Ok(Some((todo.title, award.points)))
}

/// `/points`. In a group: this week's leaderboard for the group, Monday to
/// Sunday on the reader's clock. In a DM: the sender's own points from
/// every chat they're in.
async fn points_command(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
) -> Result<String, AppError> {
    let today = my_settings(conn, ctx, defaults).await?.today(ctx.now);
    let week = Period::of(Span::Week, today);
    let just_today = Period {
        first: today,
        last: today,
    };
    if ctx.is_group {
        let totals = stored_points::totals(conn, ctx.chat_id, week).await?;
        let today_totals = stored_points::totals(conn, ctx.chat_id, just_today).await?;
        return Ok(points::group_text(week, &totals, &today_totals));
    }

    let user = ctx.sender.id;
    let last_7 = Period {
        first: today - chrono::Duration::days(6),
        last: today,
    };
    // The previous Monday to Sunday: the week that the day before this
    // Monday is in.
    let last_week = Period::of(Span::Week, week.first - chrono::Duration::days(1));
    let sum = |days: &[(NaiveDate, i64)]| days.iter().map(|(_, p)| p).sum::<i64>();

    let earned = stored_points::by_day(conn, user, last_7).await?;
    // Every one of the 7 days, with 0 for a day that earned nothing.
    let last_7_days = last_7
        .days()
        .map(|day| {
            let points = earned
                .iter()
                .find(|(d, _)| *d == day)
                .map_or(0, |(_, p)| *p);
            (day, points)
        })
        .collect();
    let mine = points::Mine {
        last_7_days,
        this_week: sum(&stored_points::by_day(conn, user, week).await?),
        last_week: sum(&stored_points::by_day(conn, user, last_week).await?),
        by_chat: stored_points::by_chat(conn, user, week).await?,
    };
    Ok(points::mine_text(&mine))
}

/// " · +3 ⭐" after a closed todo's name, or nothing if it earned nothing.
fn earned_text(points: Option<i64>) -> String {
    match points {
        Some(p) if p != 0 => format!(" · {} {}", points::signed(p), points::STAR),
        _ => String::new(),
    }
}

/// `+done gym` / `+skip gym`: closes the open todo whose title matches each
/// name. Several matches is a question back rather than a guess.
async fn close_by_name(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    names: &[String],
    action: Action,
) -> Result<String, AppError> {
    // `+done` / `+skip` only ever tick or skip, never remove.
    let finish = action.finish().unwrap_or(Finish::Done);
    // The closer's own today, for "done by its due date?".
    let today = my_settings(conn, ctx, defaults).await?.today(ctx.now);
    let mut lines = Vec::new();
    let mut closed_any = false;
    for name in names {
        let matches = todos::find_open(conn, ctx.chat_id, name).await?;
        // A slice pattern: exactly one match, bound as `(id, title)`.
        match matches.as_slice() {
            [] => lines.push(format!("{PROBLEM} No open todo matches \"{name}\".")),
            [(id, title)] => {
                if let Some((_, points)) = close_todo(conn, ctx, *id, finish, today).await? {
                    let mark = match finish {
                        Finish::Done => format!("{DONE} Done"),
                        Finish::Skipped => format!("{SKIPPED} Skipped"),
                    };
                    let earned = earned_text(Some(points));
                    lines.push(format!("{mark}: *{title}*{earned}"));
                    closed_any = true;
                }
            }
            several => {
                let titles: Vec<&str> = several.iter().map(|(_, t)| t.as_str()).collect();
                lines.push(format!(
                    "{PROBLEM} \"{name}\" matches {}: {}. Say more of the title, or use \
                     the number from /todo (`{} 2`).",
                    titles.len(),
                    titles.join(", "),
                    action.command()
                ));
            }
        }
    }
    if closed_any {
        let p = todos::progress(conn, ctx.chat_id).await?;
        lines.push(progress_bar(p.done, p.skipped, p.total));
    }
    Ok(lines.join("\n"))
}

/// "■■■■□□□□□□ 4/10 done" (the bar itself is `progress::bar`), and
/// "· 1 skipped" after it if any were. `total` counts the skipped ones,
/// but the bar leaves them out: a todo that won't be done isn't one still
/// to do.
fn progress_bar(done: i64, skipped: i64, total: i64) -> String {
    let counted = total - skipped;
    let skipped_note = (skipped > 0).then(|| format!("{skipped} skipped"));
    if counted == 0 {
        // Nothing left to count: all skipped, or no todos at all.
        return skipped_note.unwrap_or_default();
    }
    let bar = format!("{} {done}/{counted} done", progress::bar(done, counted));
    match skipped_note {
        Some(note) => format!("{bar} · {note}"),
        None => bar,
    }
}

/// The row id at list number `n`, or the message to send if there's no
/// such number.
fn item_at(items: &[ListItem], n: usize) -> Result<i64, String> {
    // Lists are numbered from 1, the Vec from 0. `checked_sub` gives
    // `None` for 0 instead of wrapping round to a huge number.
    n.checked_sub(1)
        .and_then(|index| items.get(index))
        .map(|item| item.item_id)
        .ok_or_else(|| {
            format!(
                "{PROBLEM} That list only has {} item{}.",
                items.len(),
                if items.len() == 1 { "" } else { "s" }
            )
        })
}

fn gone(n: usize) -> String {
    format!("{PROBLEM} Item {n} is already ticked off or was removed.")
}

/// The `+shop` reply.
fn format_shop_added(added: &[&str], already: &[&str]) -> String {
    let mut lines = Vec::new();
    if !added.is_empty() {
        lines.push(format!("{SHOP} Added {}", added.join(", ")));
    }
    if !already.is_empty() {
        lines.push(format!("Already on the list: {}", already.join(", ")));
    }
    lines.join("\n")
}

/// The `/shop` reply: everything on the list, ⬜ still to buy, ✅ bought
/// (struck through). Numbers start at 1 and match `item_ids` order.
fn format_shop_list(items: &[ShopItem]) -> String {
    if items.is_empty() {
        return format!("{SHOP} The shopping list is empty. Add with `+shop milk, eggs`.");
    }

    let mut text = format!("{SHOP} *Shopping list*\n");
    for (i, item) in items.iter().enumerate() {
        // `~text~` is strikethrough in WhatsApp.
        let line = if item.bought {
            format!("{DONE} ~{}~", item.name)
        } else {
            format!("{NOT_YET} {}", item.name)
        };
        text.push_str(&format!("{}. {line}\n", i + 1));
    }

    // `.any(...)`: true if at least one item matches.
    if items.iter().any(|i| !i.bought) {
        text.push_str(
            "\nTick off what you bought: reply to this message with the numbers \
             (e.g. `1 3`), or send `+got 1 3` or `+got milk` at any time.",
        );
    } else {
        text.push_str("\nEverything's bought.");
    }
    if items.iter().any(|i| i.bought) {
        text.push_str(" `/shop clear bought` removes the ✅ ones; `/shop clear` empties the list.");
    }
    text
}

fn unknown_name(name: &str, known: &[String]) -> String {
    if known.is_empty() {
        format!("{PROBLEM} I don't know anyone called {name} here yet.")
    } else {
        format!(
            "{PROBLEM} I don't know anyone called {name} here. People I know in this chat: {}.\n\
             (Someone appears here once they've sent any message in this chat.)",
            known.join(", ")
        )
    }
}

/// The `/prayers` reply: each prayer's time on the chat's own clock, marked
/// with how it went for the sender: ✅ prayed, 🕐 made up, ❌ missed.
fn format_prayer_day(
    day: &DayTimes,
    settings: &Settings,
    log: &[(Prayer, Status)],
    moon: Moon,
) -> String {
    let mut text = format!(
        "{MOSQUE} *Prayer times* ({}, {})\n",
        settings.city,
        day.date.format("%a %-d %b")
    );
    // "🌙 16 Jumada al-Ula 1448 AH · 🌖 Waning gibbous", or just the phase
    // if AlAdhan didn't send the date.
    let phase = moon.phase();
    let phase = format!("{} {phase}", phase.emoji());
    match day.hijri {
        Some(hijri) => text.push_str(&format!("{MOON} {hijri} · {phase}\n")),
        None => text.push_str(&format!("{phase}\n")),
    }
    // Numbered 1-5 in the day's order, which is what `+p 3` refers to.
    for (number, window) in (1..).zip(&day.windows) {
        let mark = log
            .iter()
            .find(|(prayer, _)| *prayer == window.prayer)
            .map_or(NOT_YET, |(_, status)| status.mark());
        text.push_str(&format!(
            "{mark} {number}. {} {}–{}\n",
            window.prayer,
            settings.clock(window.start),
            settings.clock(window.end)
        ));
    }
    text.push_str("\nTick one off with `+prayed asr` (or `+p 3`), or react to its reminder.");
    // Only worth saying once something is ❌ or 🕐.
    if log.iter().any(|(_, status)| *status != Status::Prayed) {
        text.push_str(
            "\nForgot to tick one in time? `+prayed asr on time` (or `late` if it was made up).",
        );
    }
    text
}

/// The `/prayers reports` reply: where they go now, and how to change it.
fn format_reports(delivery: Delivery, is_group: bool) -> String {
    let what = "each day's 🎉 for praying all 5, and on Sundays and at each month's end a \
                summary with the streak, misses and a heatmap";
    let now = match (delivery, is_group) {
        (Delivery::Off, _) => format!("{BELL_OFF} Prayer reports are off in this chat."),
        (Delivery::Here, true) => {
            format!("{CHART} Prayer reports go to this group: {what}, for everyone.")
        }
        (Delivery::Dm, true) => format!(
            "{CHART} Prayer reports go to each member privately: {what}. \
             (Only to people who have messaged me privately.)"
        ),
        (_, false) => format!("{CHART} Prayer reports come here: {what}."),
    };
    let change = if is_group {
        "Change with `/prayers reports here`, `dm` or `off`."
    } else {
        "Change with `/prayers reports here` or `off`."
    };
    format!("{now}\nThey need prayer reminders on (`/prayers on`). {change}")
}

/// The `/points reports` reply, in a group: where the summary goes now.
fn format_points_reports(delivery: Delivery) -> String {
    let what = "every Sunday at 8pm, the week's points: the leaderboard, how \
                each person did against last week, and the best day";
    let now = match delivery {
        Delivery::Off => format!("{BELL_OFF} The Sunday points summary is off in this group."),
        Delivery::Here => format!("{CHART} The points summary comes to this group: {what}."),
        Delivery::Dm => format!(
            "{CHART} The points summary goes to each member privately, just their own \
             week. (Only to people who have messaged me privately.)"
        ),
    };
    format!("{now}\nChange with `/points reports here`, `dm` or `off`.")
}

/// The `/settings` reply.
fn format_settings(settings: &Settings, reminders: bool, is_group: bool) -> String {
    let whose = if is_group { "this group" } else { "your own" };
    let method = prayer::method_name(settings.prayer_method).unwrap_or("unknown");
    let reminders = if reminders {
        format!(
            "on (start, and {} min before the end)",
            settings.prayer_ending_minutes
        )
    } else {
        "off (`/prayers on`)".to_string()
    };
    let mut text = format!(
        "{GEAR} *Settings* ({whose})\n\
         {PIN} {}, {}\n\
         {CLOCK} {}\n\
         {MOSQUE} Method {}: {method}; Asr {}\n\
         {BELL} Prayer reminders: {reminders}\n\n\
         Change with `/settings city …`, `timezone …`, `method …`, \
         `school hanafi|shafi`, `ending 20` or `reset`.",
        settings.city,
        settings.country,
        settings.timezone,
        settings.prayer_method,
        prayer::school_name(settings.prayer_school),
    );
    if is_group {
        text.push_str(
            "\nYour own settings: message me privately. `/settings everyone` shows each member's.",
        );
    }
    text
}

/// The `/settings everyone` reply: each member's personal settings.
fn format_everyone(members: &[MemberSettings], defaults: &Settings) -> String {
    let mut text = format!("{PEOPLE} *Members' own settings* (from their chats with me)\n");
    for member in members {
        let line = match &member.settings {
            None => "defaults".to_string(),
            Some(own) => {
                let s = own.overrides.apply(defaults);
                let reminders = if own.prayer_reminders {
                    "reminders on"
                } else {
                    "reminders off"
                };
                format!(
                    "{}, {} · {} · method {} · {} · {reminders}",
                    s.city,
                    s.country,
                    s.timezone,
                    s.prayer_method,
                    prayer::school_name(s.prayer_school)
                )
            }
        };
        text.push_str(&format!("• {}: {line}\n", member.display_name));
    }
    text
}

/// The `/settings methods` reply.
fn format_methods() -> String {
    let mut text = format!("{MOSQUE} *Prayer calculation methods*\n");
    for (id, name) in prayer::METHODS {
        text.push_str(&format!("{id}. {name}\n"));
    }
    text.push_str("\nChoose with `/settings method 3`.");
    text
}

/// The reply to `/todo reminders`.
fn format_todo_reminders(on: bool, is_group: bool) -> String {
    let place = if is_group { "in this group" } else { "here" };
    if on {
        format!(
            "{BELL} Todo reminders are on for you {place}: 08:00, 13:00, 18:00 and 22:00 on your \
             own clock, only when you have todos. `/todo reminders off` stops them."
        )
    } else {
        format!(
            "{BELL_OFF} Todo reminders are off for you {place}. `/todo reminders on` starts them."
        )
    }
}

/// How `/todo` lays the list out (see `TodoView`).
#[derive(Debug, Clone, PartialEq)]
enum Layout {
    ByPerson,
    All,
    /// Only this person's (their display name, as stored).
    Only(String),
    /// Only the open todos that match, everyone's (`format_filtered_list`).
    Filtered(TodoFilter),
}

/// The heading for todos nobody is assigned to (made before todos had to
/// have someone).
const ANYONE: &str = "Anyone";

/// The `/todo` reply: a progress bar, then the todos, ✅ ones struck through
/// with who did them and when. Numbers are each todo's place in the whole
/// list, so they match `item_ids` whichever layout is used.
fn format_todo_list(
    items: &[ListedTodo],
    layout: &Layout,
    settings: &Settings,
    now: DateTime<Utc>,
) -> String {
    // A filtered view is laid out differently enough to be its own function.
    if let Layout::Filtered(filter) = layout {
        return format_filtered_list(items, *filter, settings, now);
    }
    // One person's view says whose list is empty (below), even if all are.
    if items.is_empty() && !matches!(layout, Layout::Only(_)) {
        return format!("{LIST} No todos. Add one with `+todo fix the tap`.");
    }

    // `(1..).zip(items)` pairs each todo with its number: (1, first), ...
    let numbered: Vec<Numbered> = (1..).zip(items).collect();
    let (title, shown) = match layout {
        Layout::Only(name) => {
            let theirs: Vec<_> = numbered
                .into_iter()
                .filter(|(_, todo)| is_for(todo, name))
                .collect();
            if theirs.is_empty() {
                return format!("{LIST} {name} has no todos. Add one with `+todo @{name} ...`.");
            }
            (format!("*{name}'s todos*"), theirs)
        }
        // (`Filtered` returned at the top.)
        Layout::ByPerson | Layout::All | Layout::Filtered(_) => ("*Todos*".to_string(), numbered),
    };

    let total = shown.len() as i64;
    let done = shown.iter().filter(|(_, t)| t.is_done()).count() as i64;
    let skipped = shown.iter().filter(|(_, t)| t.is_skipped()).count() as i64;
    let mut text = format!("{LIST} {title}\n{}\n", progress_bar(done, skipped, total));
    let today = settings.today(now);
    let overdue = shown.iter().filter(|(_, t)| t.is_overdue(today)).count();
    if overdue > 0 {
        text.push_str(&format!("{OVERDUE} {overdue} overdue\n"));
    }
    // How much of the sender's day is left to do them in.
    if let Some(day) = calendar::day_line(now, settings.timezone) {
        text.push_str(&day);
        text.push('\n');
    }

    let groups = match layout {
        Layout::ByPerson => group_by_person(&shown),
        // One group, with no heading.
        Layout::All | Layout::Only(_) | Layout::Filtered(_) => vec![(None, shown.clone())],
    };
    // A heading over each person's todos, unless they're all one person's
    // (in a DM, say), where it would only repeat the obvious.
    let headings = groups.len() > 1;
    for (person, todos) in &groups {
        text.push('\n');
        if headings {
            text.push_str(&format!("*{}*\n", person.unwrap_or(ANYONE)));
        }
        for (number, todo) in todos {
            // The "(Bilal)" after a title is only needed in the flat list.
            let show_assignee = *layout == Layout::All;
            text.push_str(&todo_line(*number, todo, show_assignee, settings, now));
            text.push('\n');
        }
    }

    if done + skipped < total {
        text.push_str(
            "\nFinish one: reply to this message with `done 2`, or send `+done 2` or \
             `+done gym` any time.",
        );
    } else {
        text.push_str("\nAll done! 🎉");
    }
    match (done > 0, skipped > 0) {
        (false, false) => {}
        (true, false) => text
            .push_str(" `/todo clear done` removes the ✅ ones; `/todo clear` empties the list."),
        (_, true) => text.push_str(
            " `/todo clear done` removes the ✅ and ⏭️ ones; `/todo clear` empties the list.",
        ),
    }
    text
}

/// `/todo overdue`, `/todo due`, `/todo high`: only the open todos that
/// match, with who each is for and their numbers in the whole list (so
/// `done 3` here means the same todo as in `/todo`). No progress bar: it's
/// a slice of the list, not the list.
fn format_filtered_list(
    items: &[ListedTodo],
    filter: TodoFilter,
    settings: &Settings,
    now: DateTime<Utc>,
) -> String {
    let today = settings.today(now);
    let shown: Vec<Numbered> = (1..)
        .zip(items)
        .filter(|(_, todo)| match filter {
            TodoFilter::Overdue => todo.is_overdue(today),
            TodoFilter::Due => todo.closed.is_none() && todo.due.is_some(),
            TodoFilter::High => todo.closed.is_none() && todo.priority == Some(Priority::High),
        })
        .collect();

    let heading = match (filter, shown.is_empty()) {
        (TodoFilter::Overdue, true) => return format!("{LIST} Nothing is overdue. 🎉"),
        (TodoFilter::Due, true) => {
            return format!(
                "{LIST} No open todos have a due date. Add one with `+todo bins -d fri`."
            );
        }
        (TodoFilter::High, true) => {
            return format!(
                "{LIST} No open todos are high priority. Add one with `+todo bins -p high`."
            );
        }
        (TodoFilter::Overdue, false) => "Overdue",
        (TodoFilter::Due, false) => "Todos with a due date",
        (TodoFilter::High, false) => "High priority",
    };

    let mut text = format!("{LIST} *{heading}* ({})\n\n", shown.len());
    for (number, todo) in &shown {
        text.push_str(&todo_line(*number, todo, true, settings, now));
        text.push('\n');
    }
    text.push_str(
        "\nReply `done 2`, `skip 2` or `due 2 fri` to this message. `/todo` shows the whole list.",
    );
    text
}

/// A todo with its number in the whole list.
type Numbered<'a> = (usize, &'a ListedTodo);

/// Whether `todo` is assigned to `name` (ignoring case, like `@names`).
fn is_for(todo: &ListedTodo, name: &str) -> bool {
    todo.assignee
        .as_deref()
        .is_some_and(|assignee| assignee.eq_ignore_ascii_case(name))
}

/// One numbered todo per person, people in the order their first todo
/// appears (so whoever has the earliest open todo comes first), and the
/// todos keeping their order within each.
///
/// The lifetime `'a` says the names and todos in the result are borrowed
/// from `shown`: the groups can't outlive the list they point into.
fn group_by_person<'a>(shown: &[Numbered<'a>]) -> Vec<(Option<&'a str>, Vec<Numbered<'a>>)> {
    let mut groups: Vec<(Option<&str>, Vec<Numbered>)> = Vec::new();
    for &(number, todo) in shown {
        let person = todo.assignee.as_deref();
        // A `Vec` searched each time rather than a `HashMap`, because a
        // `HashMap` forgets the order, and a family has a handful of people.
        match groups.iter_mut().find(|(p, _)| *p == person) {
            Some((_, todos)) => todos.push((number, todo)),
            None => groups.push((person, vec![(number, todo)])),
        }
    }
    groups
}

/// The end of an open todo's line: " · 📅 Fri 13 Nov · 🔴", or
/// " · ⚠️ overdue (Wed 4 Nov)" once it's late, " · 📅 today" / "tomorrow"
/// when it's that close. Empty with no date or priority. `today` is the
/// reader's own.
fn due_note(todo: &ListedTodo, today: NaiveDate) -> String {
    let mut note = String::new();
    if let Some(due) = todo.due {
        let day = todo_flags::show_date(due, today);
        // `succ_opt`: the next day (`None` only at the end of time).
        let tomorrow = today.succ_opt();
        let text = if due < today {
            format!("{OVERDUE} overdue ({day})")
        } else if due == today {
            format!("{DUE} today")
        } else if Some(due) == tomorrow {
            format!("{DUE} tomorrow")
        } else {
            format!("{DUE} {day}")
        };
        note.push_str(" · ");
        note.push_str(&text);
    }
    if let Some(priority) = todo.priority {
        note.push_str(" · ");
        note.push_str(priority.emoji());
    }
    note
}

/// "⬜ 4. fix the tap", "✅ 2. ~bins~ · Bilal 14:05" when done, or
/// "⏭️ 3. ~hoover~ · skipped by Bilal 14:05" when skipped. The "Bilal" (who
/// closed it) is left out when it was the assignee themselves, since
/// they're already named. `show_assignee` adds " (Khadija)". Open todos
/// end with their due date and priority, if they have them (`due_note`).
fn todo_line(
    number: usize,
    todo: &ListedTodo,
    show_assignee: bool,
    settings: &Settings,
    now: DateTime<Utc>,
) -> String {
    let assignee = match &todo.assignee {
        Some(name) if show_assignee => format!(" ({name})"),
        _ => String::new(),
    };
    let Some(closed) = &todo.closed else {
        let note = due_note(todo, settings.today(now));
        return format!("{NOT_YET} {number}. {}{assignee}{note}", todo.title);
    };
    // "14:05" if it was today (their today), else "Tue 14:05".
    let when = if settings.today(closed.at) == settings.today(now) {
        settings.clock(closed.at)
    } else {
        let local = closed.at.with_timezone(&settings.timezone);
        local.format("%a %H:%M").to_string()
    };
    let by_themselves = !show_assignee && is_for(todo, &closed.by);
    if closed.skipped {
        let by = if by_themselves {
            String::new()
        } else {
            format!("by {} ", closed.by)
        };
        return format!(
            "{SKIPPED} {number}. ~{}~{assignee} · skipped {by}{when}",
            todo.title
        );
    }
    let by = if by_themselves {
        String::new()
    } else {
        format!("{} ", closed.by)
    };
    format!("{DONE} {number}. ~{}~{assignee} · {by}{when}", todo.title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aladhan::PrayerTimes;
    use crate::db::test_pool;
    use crate::person::Person;
    use crate::prayer::{HijriDate, Window};
    use crate::repo::chats;
    use crate::settings::tests::defaults;
    use crate::testkit::{register, seed_family, utc};
    use chrono::NaiveDate;

    /// 27 Oct 2026 in Milton Keynes (GMT, so London time = UTC).
    fn day() -> DayTimes {
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
    fn times() -> PrayerTimes {
        PrayerTimes::with_cached(defaults(), day())
    }

    fn item(position: i64, item_id: i64) -> ListItem {
        ListItem { position, item_id }
    }

    /// A list of `kind` showing `item_ids`, numbered from 1, as a reply to
    /// it would see it.
    fn sent(kind: ListKind, item_ids: &[i64]) -> SentList {
        let items = item_ids
            .iter()
            .zip(1..)
            .map(|(&id, pos)| item(pos, id))
            .collect();
        SentList { kind, items }
    }

    // ---- pure text ----

    #[test]
    fn empty_list_says_so() {
        let now = Utc::now();
        assert!(format_todo_list(&[], &Layout::ByPerson, &defaults(), now).contains("No todos"));
    }

    #[test]
    fn todo_list_shows_progress_open_and_done_with_times() {
        let at = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        let now = at("2026-10-28T18:00:00Z");
        let items = [
            ListedTodo {
                id: 7,
                title: "go gym".to_string(),
                assignee: Some("Bilal".to_string()),
                closed: None,
                due: None,
                priority: None,
            },
            ListedTodo {
                id: 3,
                title: "fix the tap".to_string(),
                assignee: None,
                closed: Some(todos::Closed {
                    by: "Khadija".to_string(),
                    at: at("2026-10-28T14:05:00Z"),
                    skipped: false,
                }),
                due: None,
                priority: None,
            },
            ListedTodo {
                id: 4,
                title: "bins".to_string(),
                assignee: None,
                closed: Some(todos::Closed {
                    by: "Bilal".to_string(),
                    at: at("2026-10-27T08:30:00Z"),
                    skipped: false,
                }),
                due: None,
                priority: None,
            },
        ];
        let text = format_todo_list(&items, &Layout::All, &defaults(), now);
        assert!(text.contains("■■■■■■■□□□ 2/3 done"), "{text}");
        assert!(text.contains("⬜ 1. go gym (Bilal)\n"), "{text}");
        assert!(
            text.contains("✅ 2. ~fix the tap~ · Khadija 14:05\n"),
            "{text}"
        );
        assert!(text.contains("✅ 3. ~bins~ · Bilal Tue 08:30\n"), "{text}");
        assert!(text.contains("`+done 2`"), "{text}");
    }

    /// Bilal's, Khadija's, Bilal's, nobody's, and one of Khadija's that
    /// Bilal did for her: numbered 1-5.
    fn family_todos() -> Vec<ListedTodo> {
        let at = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        let todo = |id, title: &str, assignee: Option<&str>, done_by: Option<&str>| ListedTodo {
            id,
            title: title.to_string(),
            assignee: assignee.map(str::to_string),
            closed: done_by.map(|by| todos::Closed {
                by: by.to_string(),
                at: at("2026-10-28T14:05:00Z"),
                skipped: false,
            }),
            due: None,
            priority: None,
        };
        vec![
            todo(11, "hoover", Some("Bilal"), None),
            todo(12, "call the school", Some("Khadija"), None),
            todo(13, "bins", Some("Bilal"), Some("Bilal")),
            todo(14, "fix the tap", None, None),
            todo(15, "book dentist", Some("Khadija"), Some("Bilal")),
        ]
    }

    #[test]
    fn todo_list_by_person_keeps_the_numbers() {
        let now = "2026-10-28T18:00:00Z".parse().unwrap();
        let text = format_todo_list(&family_todos(), &Layout::ByPerson, &defaults(), now);
        let expected = "📋 *Todos*\n■■■■□□□□□□ 2/5 done\n\
             ⏳ ■■■■■■■■□□ 75% through the day · 6h 00m left\n\n\
             *Bilal*\n⬜ 1. hoover\n✅ 3. ~bins~ · 14:05\n\n\
             *Khadija*\n⬜ 2. call the school\n✅ 5. ~book dentist~ · Bilal 14:05\n\n\
             *Anyone*\n⬜ 4. fix the tap\n";
        assert!(text.starts_with(expected), "{text}");
    }

    #[test]
    fn todo_list_of_one_person_has_their_own_progress() {
        let now = "2026-10-28T18:00:00Z".parse().unwrap();
        let only = Layout::Only("Khadija".to_string());
        let text = format_todo_list(&family_todos(), &only, &defaults(), now);
        let expected = "📋 *Khadija's todos*\n■■■■■□□□□□ 1/2 done\n\
             ⏳ ■■■■■■■■□□ 75% through the day · 6h 00m left\n\n\
             ⬜ 2. call the school\n✅ 5. ~book dentist~ · Bilal 14:05\n";
        assert!(text.starts_with(expected), "{text}");

        let only = Layout::Only("Yusuf".to_string());
        let text = format_todo_list(&family_todos(), &only, &defaults(), now);
        assert!(text.starts_with("📋 Yusuf has no todos."), "{text}");
    }

    #[test]
    fn todo_list_of_one_persons_todos_has_no_heading() {
        let now = "2026-10-28T18:00:00Z".parse().unwrap();
        let mine: Vec<_> = family_todos()
            .into_iter()
            .filter(|t| t.assignee.as_deref() == Some("Bilal"))
            .collect();
        let text = format_todo_list(&mine, &Layout::ByPerson, &defaults(), now);
        assert!(text.contains("left\n\n⬜ 1. hoover\n"), "{text}");
        assert!(!text.contains("*Bilal*"), "{text}");
    }

    #[tokio::test]
    async fn todo_for_a_person_checks_the_name() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        let apis = Apis::new(times());
        let list = |name: &str| BotCommand::ListTodos(TodoView::Person(name.to_string()));

        let reply = run_command(&mut conn, &ctx, &apis, list("nobody"))
            .await
            .unwrap()
            .into_text();
        assert!(
            reply.contains("I don't know anyone called nobody"),
            "{reply}"
        );

        // Any case finds them; the heading uses their name as stored.
        let me = ctx.sender.display_name.to_lowercase();
        let reply = run_command(&mut conn, &ctx, &apis, list(&me))
            .await
            .unwrap()
            .into_text();
        let expected = format!("{} has no todos", ctx.sender.display_name);
        assert!(reply.contains(&expected), "{reply}");
    }

    #[tokio::test]
    async fn todo_reminders_are_each_persons_own_choice() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        let apis = Apis::new(times());
        let mut run = async |on| {
            run_command(&mut conn, &ctx, &apis, BotCommand::TodoReminders(on))
                .await
                .unwrap()
                .into_text()
        };

        assert!(
            run(None).await.contains("are off for you"),
            "off by default"
        );
        assert!(
            run(Some(true))
                .await
                .contains("are on for you in this group")
        );
        assert!(run(None).await.contains("are on for you"));
        assert!(run(Some(false)).await.contains("are off for you"));
    }

    #[test]
    fn progress_bar_rounds_and_handles_edges() {
        assert_eq!(progress_bar(0, 0, 4), "□□□□□□□□□□ 0/4 done");
        assert_eq!(progress_bar(1, 0, 3), "■■■□□□□□□□ 1/3 done");
        assert_eq!(progress_bar(4, 0, 4), "■■■■■■■■■■ 4/4 done");
        assert_eq!(progress_bar(0, 0, 0), "");
    }

    #[test]
    fn progress_bar_leaves_skipped_todos_out_but_says_so() {
        // 4 todos, 1 skipped: 1 of the other 3 done.
        assert_eq!(progress_bar(1, 1, 4), "■■■□□□□□□□ 1/3 done · 1 skipped");
        assert_eq!(progress_bar(3, 1, 4), "■■■■■■■■■■ 3/3 done · 1 skipped");
        // Nothing left that counts.
        assert_eq!(progress_bar(0, 2, 2), "2 skipped");
    }

    #[test]
    fn item_at_maps_numbers_to_ids() {
        let items = [item(1, 7), item(2, 3)];
        assert_eq!(item_at(&items, 1), Ok(7));
        assert_eq!(item_at(&items, 2), Ok(3));
        assert!(item_at(&items, 3).unwrap_err().contains("only has 2 items"));
        assert!(item_at(&items, 0).is_err());
        assert!(
            item_at(&items[..1], 5)
                .unwrap_err()
                .contains("only has 1 item.")
        );
    }

    // ---- with a database ----

    /// A group with Bilal (the sender) and Khadija in it.
    async fn setup(conn: &mut SqliteConnection) -> Context {
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

    fn add(title: &str, assignee: Option<Assignee>) -> BotCommand {
        BotCommand::AddTodo {
            titles: vec![title.to_string()],
            assignee,
            due: None,
            priority: None,
        }
    }

    #[tokio::test]
    async fn several_todos_are_added_together() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        let command = BotCommand::AddTodo {
            titles: vec!["hoover".to_string(), "bins".to_string()],
            assignee: Some(Assignee::Sender(Person {
                name: ctx.sender.display_name.clone(),
                jid: ctx.sender.wa_jid.clone(),
                lid: None,
            })),
            due: None,
            priority: None,
        };
        let reply = run_command(&mut conn, &ctx, &Apis::new(times()), command)
            .await
            .unwrap()
            .into_text();
        let name = &ctx.sender.display_name;
        assert_eq!(
            reply,
            format!("📝 Added 2 todos for {name}:\n• hoover\n• bins")
        );

        let listed = todos::list_all(&mut conn, ctx.chat_id).await.unwrap();
        let titles: Vec<&str> = listed.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["hoover", "bins"], "in the order typed");
    }

    #[test]
    fn one_todo_added_reads_as_before() {
        let one = format_todos_added(
            &["go gym".to_string()],
            "Bilal",
            Details::default(),
            friday(),
        );
        assert_eq!(one, "📝 Added *go gym* for Bilal");
    }

    /// Friday 6 November 2026.
    fn friday() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 6).unwrap()
    }

    #[test]
    fn added_todos_show_their_due_date_and_priority() {
        let details = Details {
            due: NaiveDate::from_ymd_opt(2026, 11, 13),
            priority: Some(Priority::High),
        };
        let one = format_todos_added(&["bins".to_string()], "Bilal", details, friday());
        assert_eq!(
            one,
            "📝 Added *bins* for Bilal\n📅 Due Fri 13 Nov · 🔴 High priority"
        );

        // For several, one line at the end: it's the same for all of them.
        let titles = ["hoover".to_string(), "bins".to_string()];
        let low = Details {
            due: None,
            priority: Some(Priority::Low),
        };
        assert_eq!(
            format_todos_added(&titles, "Bilal", low, friday()),
            "📝 Added 2 todos for Bilal:\n• hoover\n• bins\n🟢 Low priority"
        );
    }

    /// Runs a typed message, as the webhook would (`+todo` with no
    /// `@name` is for the sender: `add_todo` treats `None` that way too).
    async fn typed(conn: &mut SqliteConnection, ctx: &Context, text: &str) -> Outcome {
        let command = crate::parser::parse(text).expect("should parse");
        run_command(conn, ctx, &Apis::new(times()), command)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn several_todos_share_the_flags() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        // Friday lunchtime in London.
        ctx.now = "2026-11-06T12:00:00Z".parse().unwrap();

        let reply = typed(&mut conn, &ctx, "+todo hoover, bins -d tmr -p high").await;
        assert_eq!(
            reply.into_text(),
            "📝 Added 2 todos for Bilal:\n• hoover\n• bins\n📅 Due Sat 7 Nov · 🔴 High priority"
        );
        let rows = sqlx::query!("SELECT title, due_date, priority FROM todos ORDER BY id")
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        let saved: Vec<(&str, Option<&str>, Option<&str>)> = rows
            .iter()
            .map(|r| {
                (
                    r.title.as_str(),
                    r.due_date.as_deref(),
                    r.priority.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            saved,
            [
                ("hoover", Some("2026-11-07"), Some("high")),
                ("bins", Some("2026-11-07"), Some("high"))
            ]
        );
    }

    #[tokio::test]
    async fn due_dates_count_from_the_senders_own_today() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        // 23:30 UTC on Friday 3 July is already 00:30 on Saturday in London
        // (summer time, UTC+1), so "today" there is Saturday.
        ctx.now = "2026-07-03T23:30:00Z".parse().unwrap();

        let reply = typed(&mut conn, &ctx, "+todo bins -d today").await;
        assert!(
            reply.into_text().ends_with("📅 Due Sat 4 Jul"),
            "Saturday on the sender's clock"
        );
    }

    #[tokio::test]
    async fn bad_and_past_dates_add_nothing() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        ctx.now = "2026-11-06T12:00:00Z".parse().unwrap();

        let past = typed(&mut conn, &ctx, "+todo bins -d 01-01-2026").await;
        assert_eq!(
            past.into_text(),
            "❓ That date has already passed (Thu 1 Jan 2026)."
        );
        let bad = typed(&mut conn, &ctx, "+todo bins -p urgent").await;
        assert_eq!(
            bad.into_text(),
            "❓ Priority can be `high`, `medium` or `low`, not \"urgent\"."
        );
        assert!(
            todos::list_all(&mut conn, ctx.chat_id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn closing_todos_earns_points_that_outlive_the_list() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        // Friday 6 November, lunchtime in London.
        ctx.now = "2026-11-06T12:00:00Z".parse().unwrap();
        typed(&mut conn, &ctx, "+todo report -d today -p high").await;
        typed(&mut conn, &ctx, "+todo bins, gym -d today").await;
        typed(&mut conn, &ctx, "+todo fix the tap -p low").await;

        // High and on time: 3 + 1.
        let reply = typed(&mut conn, &ctx, "+done report").await.into_text();
        assert!(reply.starts_with("✅ Done: *report* · +4 ⭐"), "{reply}");

        // Saturday: bins is a day late (2 − 1), and a skip costs 1.
        ctx.now = "2026-11-07T10:00:00Z".parse().unwrap();
        let reply = typed(&mut conn, &ctx, "+done bins").await.into_text();
        assert!(reply.starts_with("✅ Done: *bins* · +1 ⭐"), "{reply}");
        let reply = typed(&mut conn, &ctx, "+skip gym").await.into_text();
        assert!(reply.starts_with("⏭️ Skipped: *gym* · −1 ⭐"), "{reply}");

        // Khadija ticks off the low one: whoever closes it earns it.
        let khadija = users::find_member_by_name(&mut conn, ctx.chat_id, "Khadija")
            .await
            .unwrap()
            .unwrap();
        let hers = Context {
            sender: khadija,
            chat_id: ctx.chat_id,
            is_group: true,
            now: ctx.now,
        };
        let reply = typed(&mut conn, &hers, "+done tap").await.into_text();
        assert!(
            reply.starts_with("✅ Done: *fix the tap* · +1 ⭐"),
            "{reply}"
        );
        // Already closed: nothing more to earn.
        typed(&mut conn, &hers, "+done tap").await;

        // Clearing the list keeps everyone's points.
        typed(&mut conn, &ctx, "/todo clear done").await;
        assert_eq!(
            typed(&mut conn, &ctx, "/points").await.into_text(),
            "🏆 *Points this week* (Mon 2 Nov – Sun 8 Nov)\n\
             🥇 Bilal 4\n\
             🥈 Khadija 1\n\
             \n\
             Today: Khadija +1"
        );

        // In his DM, Bilal sees his own points, from the group too.
        let dm = chats::find_or_create(
            &mut conn,
            "447700900001@s.whatsapp.net",
            Some(ctx.sender.id),
            ctx.now,
        )
        .await
        .unwrap();
        let in_dm = Context {
            chat_id: dm,
            is_group: false,
            sender: ctx.sender.clone(),
            now: ctx.now,
        };
        let mine = typed(&mut conn, &in_dm, "/points").await.into_text();
        assert!(
            mine.starts_with("⭐ *Your points*\nToday: 0 · This week: 4 · Last week: 0"),
            "{mine}"
        );
        assert!(mine.contains("Fri 6 ■■■■■■■■■■ 4"), "{mine}");
    }

    #[tokio::test]
    async fn points_reports_are_set_per_group() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;

        let reply = typed(&mut conn, &ctx, "/points reports").await.into_text();
        assert!(
            reply.contains("comes to this group"),
            "here by default: {reply}"
        );
        let reply = typed(&mut conn, &ctx, "/points reports dm")
            .await
            .into_text();
        assert!(reply.contains("each member privately"), "{reply}");
        assert_eq!(
            stored_reports::delivery(&mut conn, ctx.chat_id, Feature::Points)
                .await
                .unwrap(),
            Delivery::Dm
        );
        // The prayer reports are a separate setting.
        assert_eq!(
            stored_reports::delivery(&mut conn, ctx.chat_id, Feature::Prayers)
                .await
                .unwrap(),
            Delivery::Here
        );

        let dm = Context {
            chat_id: ctx.chat_id,
            is_group: false,
            sender: ctx.sender.clone(),
            now: ctx.now,
        };
        let reply = typed(&mut conn, &dm, "/points reports off")
            .await
            .into_text();
        assert!(reply.contains("for groups"), "{reply}");
    }

    #[tokio::test]
    async fn skipping_by_reply_by_number_and_by_name() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        typed(&mut conn, &ctx, "+todo go gym, fix the tap, bins, hoover").await;
        let Outcome::List { item_ids, .. } = typed(&mut conn, &ctx, "/todo all").await else {
            panic!("expected a list");
        };
        let items = sent(ListKind::Todo, &item_ids);

        // Reply `skip 1`.
        let reply = run_list_reply(&mut conn, &ctx, &defaults(), &items, ReplyCommand::Skip(1))
            .await
            .unwrap()
            .into_text();
        assert_eq!(
            reply,
            "⏭️ Skipped: *go gym* · −1 ⭐\n□□□□□□□□□□ 0/3 done · 1 skipped"
        );
        // Already closed.
        let again = run_list_reply(&mut conn, &ctx, &defaults(), &items, ReplyCommand::Skip(1))
            .await
            .unwrap()
            .into_text();
        assert!(again.contains("already"), "{again}");

        // `+skip 2` uses the latest list, which the webhook saved; here
        // there's none saved, so it says how to get one.
        let reply = typed(&mut conn, &ctx, "+skip 2").await.into_text();
        assert!(reply.contains("Send /todo, then `+skip 2`"), "{reply}");

        // `+skip tap`, then `+done bins`.
        let reply = typed(&mut conn, &ctx, "+skip tap").await.into_text();
        assert_eq!(
            reply,
            "⏭️ Skipped: *fix the tap* · −1 ⭐\n□□□□□□□□□□ 0/2 done · 2 skipped"
        );
        typed(&mut conn, &ctx, "+done bins").await;

        // `/todo` shows them: open, then closed in the order closed.
        let list = typed(&mut conn, &ctx, "/todo all").await.into_text();
        assert!(list.contains("■■■■■□□□□□ 1/2 done · 2 skipped"), "{list}");
        assert!(list.contains("⬜ 1. hoover (Bilal)\n"), "{list}");
        assert!(
            list.contains("⏭️ 2. ~go gym~ (Bilal) · skipped by Bilal "),
            "{list}"
        );
        assert!(list.contains("✅ 4. ~bins~ (Bilal) · Bilal "), "{list}");
        assert!(list.contains("removes the ✅ and ⏭️ ones"), "{list}");
        // In the person view, "by Bilal" is left out: it's his own.
        let mine = typed(&mut conn, &ctx, "/todo mine").await.into_text();
        assert!(mine.contains("⏭️ 2. ~go gym~ · skipped "), "{mine}");

        let reply = typed(&mut conn, &ctx, "/todo clear done").await.into_text();
        assert_eq!(reply, "🧹 Cleared 1 completed and 2 skipped.");
    }

    #[tokio::test]
    async fn skip_on_a_shopping_list_says_what_to_do() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        let shop_list = sent(ListKind::Shop, &[1]);
        let reply = run_list_reply(
            &mut conn,
            &ctx,
            &defaults(),
            &shop_list,
            ReplyCommand::Skip(1),
        )
        .await
        .unwrap()
        .into_text();
        assert!(reply.contains("`skip` is for todos"), "{reply}");
    }

    #[test]
    fn due_notes_say_how_soon() {
        let on = |d: u32, priority| ListedTodo {
            id: 1,
            title: "bins".to_string(),
            assignee: None,
            closed: None,
            due: NaiveDate::from_ymd_opt(2026, 11, d),
            priority,
        };
        let today = friday(); // the 6th
        assert_eq!(due_note(&on(4, None), today), " · ⚠️ overdue (Wed 4 Nov)");
        assert_eq!(due_note(&on(6, None), today), " · 📅 today");
        assert_eq!(due_note(&on(7, None), today), " · 📅 tomorrow");
        assert_eq!(
            due_note(&on(13, Some(Priority::Medium)), today),
            " · 📅 Fri 13 Nov · 🟠"
        );
        let mut undated = on(1, Some(Priority::High));
        undated.due = None;
        assert_eq!(due_note(&undated, today), " · 🔴");
        undated.priority = None;
        assert_eq!(due_note(&undated, today), "");
    }

    #[tokio::test]
    async fn todo_list_shows_and_sorts_by_due_date() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        // Friday 6 November, lunchtime in London.
        ctx.now = "2026-11-06T12:00:00Z".parse().unwrap();

        typed(&mut conn, &ctx, "+todo hoover").await;
        typed(&mut conn, &ctx, "+todo passport -d 20-11 -p high").await;
        typed(&mut conn, &ctx, "+todo dentist -d tmr -p low").await;
        typed(&mut conn, &ctx, "+todo bins -d today").await;
        // A command can't add a past date, but a todo becomes overdue as the
        // days go by: this one was due on Wednesday.
        let late = Details {
            due: NaiveDate::from_ymd_opt(2026, 11, 4),
            priority: Some(Priority::High),
        };
        let me = Some(ctx.sender.id);
        let tax = todos::add(
            &mut conn,
            ctx.chat_id,
            "tax",
            ctx.sender.id,
            me,
            late,
            ctx.now,
        )
        .await
        .unwrap();

        let Outcome::List { text, item_ids, .. } = typed(&mut conn, &ctx, "/todo all").await else {
            panic!("expected a list");
        };
        assert!(text.contains("⚠️ 1 overdue\n"), "{text}");
        let lines = "⬜ 1. tax (Bilal) · ⚠️ overdue (Wed 4 Nov) · 🔴\n\
                     ⬜ 2. bins (Bilal) · 📅 today\n\
                     ⬜ 3. dentist (Bilal) · 📅 tomorrow · 🟢\n\
                     ⬜ 4. passport (Bilal) · 📅 Fri 20 Nov · 🔴\n\
                     ⬜ 5. hoover (Bilal)\n";
        assert!(text.contains(lines), "{text}");
        // The numbers saved for replies follow the same order, so `done 1`
        // is the overdue one.
        assert_eq!(item_ids[0], tax);
        let items = sent(ListKind::Todo, &item_ids);
        let reply = run_list_reply(&mut conn, &ctx, &defaults(), &items, ReplyCommand::Done(1))
            .await
            .unwrap()
            .into_text();
        assert!(reply.starts_with("✅ Done: *tax*"), "{reply}");
    }

    /// Bilal's todos on Friday 6 November: "tax" was due Wednesday (high
    /// priority), "bins" is due today, "hoover" has no date, "passport" is
    /// high priority with no date. Returns the context and the list items
    /// from `/todo`, as a reply would see them.
    async fn dated_todos(conn: &mut SqliteConnection) -> (Context, SentList) {
        let mut ctx = setup(conn).await;
        ctx.now = "2026-11-06T12:00:00Z".parse().unwrap();
        let late = Details {
            due: NaiveDate::from_ymd_opt(2026, 11, 4),
            priority: Some(Priority::High),
        };
        let me = Some(ctx.sender.id);
        todos::add(conn, ctx.chat_id, "tax", ctx.sender.id, me, late, ctx.now)
            .await
            .unwrap();
        typed(conn, &ctx, "+todo bins -d today").await;
        typed(conn, &ctx, "+todo hoover").await;
        typed(conn, &ctx, "+todo passport -p high").await;
        let Outcome::List { item_ids, .. } = typed(conn, &ctx, "/todo").await else {
            panic!("expected a list");
        };
        (ctx, sent(ListKind::Todo, &item_ids))
    }

    #[tokio::test]
    async fn filtered_lists_keep_the_whole_lists_numbers() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let (ctx, _) = dated_todos(&mut conn).await;
        // Whole list: 1 tax, 2 bins, 3 passport (high, undated), 4 hoover.

        let overdue = typed(&mut conn, &ctx, "/todo overdue").await.into_text();
        assert!(
            overdue.starts_with(
                "📋 *Overdue* (1)\n\n⬜ 1. tax (Bilal) · ⚠️ overdue (Wed 4 Nov) · 🔴\n"
            ),
            "{overdue}"
        );
        let due = typed(&mut conn, &ctx, "/todo due").await.into_text();
        assert!(due.contains("⬜ 1. tax"), "{due}");
        assert!(due.contains("⬜ 2. bins (Bilal) · 📅 today\n"), "{due}");
        assert!(!due.contains("hoover"), "{due}");
        let high = typed(&mut conn, &ctx, "/todo high").await.into_text();
        assert!(high.contains("*High priority* (2)"), "{high}");
        assert!(high.contains("⬜ 3. passport (Bilal) · 🔴\n"), "{high}");

        // A filtered list is still a list: replies to it work, and every
        // todo's id is saved so the numbers match.
        let Outcome::List { item_ids, .. } = typed(&mut conn, &ctx, "/todo high").await else {
            panic!("expected a list");
        };
        assert_eq!(item_ids.len(), 4);

        // Once it's done, nothing is overdue.
        typed(&mut conn, &ctx, "+done tax").await;
        let overdue = typed(&mut conn, &ctx, "/todo overdue").await.into_text();
        assert_eq!(overdue, "📋 Nothing is overdue. 🎉");
    }

    #[tokio::test]
    async fn replies_change_the_due_date_and_priority() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let (ctx, items) = dated_todos(&mut conn).await;
        let apis = Apis::new(times());
        let mut change = async |text: &str| {
            let reply = crate::parser::parse_reply(text).expect("should parse");
            run_list_reply(&mut conn, &ctx, &apis.prayer_times.defaults, &items, reply)
                .await
                .unwrap()
                .into_text()
        };

        // 4 is hoover (no date, no priority).
        assert_eq!(
            change("due 4 mon").await,
            "📅 *hoover* is now due Mon 9 Nov."
        );
        assert_eq!(
            change("priority 4 low").await,
            "🟢 *hoover* is now low priority."
        );
        assert_eq!(change("due 1 none").await, "📅 *tax* has no due date now.");
        assert_eq!(
            change("priority 1 none").await,
            "*tax* has no priority now."
        );
        assert_eq!(
            change("due 4 01-01-2026").await,
            "❓ That date has already passed (Thu 1 Jan 2026)."
        );
        assert_eq!(
            change("priority 4 urgent").await,
            "❓ Priority can be `high`, `medium` or `low`, not \"urgent\"."
        );
        assert!(change("due 9 fri").await.contains("only has 4 items"));

        // A closed todo can't be changed.
        change("done 2").await;
        assert!(
            change("due 2 fri")
                .await
                .contains("already done, skipped or removed")
        );

        let saved = todos::list_all(&mut conn, ctx.chat_id).await.unwrap();
        let hoover = saved.iter().find(|t| t.title == "hoover").unwrap();
        assert_eq!(hoover.due, NaiveDate::from_ymd_opt(2026, 11, 9));
        assert_eq!(hoover.priority, Some(Priority::Low));
        let tax = saved.iter().find(|t| t.title == "tax").unwrap();
        assert_eq!((tax.due, tax.priority), (None, None));
    }

    #[tokio::test]
    async fn changes_are_for_todos_only() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        let shop_list = sent(ListKind::Shop, &[1]);
        let reply = run_list_reply(
            &mut conn,
            &ctx,
            &defaults(),
            &shop_list,
            ReplyCommand::SetDue(1, None),
        )
        .await
        .unwrap()
        .into_text();
        assert!(reply.contains("not the shopping list"), "{reply}");
    }

    #[tokio::test]
    async fn add_assigns_to_sender_or_mentioned_member() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;

        let mine = run_command(&mut conn, &ctx, &Apis::new(times()), add("go gym", None))
            .await
            .unwrap();
        assert_eq!(
            mine,
            Outcome::Reply("📝 Added *go gym* for Bilal".to_string())
        );

        let mention = Some(Assignee::Mentioned("khadija".to_string()));
        let theirs = run_command(
            &mut conn,
            &ctx,
            &Apis::new(times()),
            add("book dentist", mention),
        )
        .await
        .unwrap();
        assert_eq!(
            theirs,
            Outcome::Reply("📝 Added *book dentist* for Khadija".to_string())
        );
    }

    #[tokio::test]
    async fn unknown_mention_adds_nothing_and_lists_names() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;

        let mention = Some(Assignee::Mentioned("Yusuf".to_string()));
        let Outcome::Reply(text) =
            run_command(&mut conn, &ctx, &Apis::new(times()), add("x", mention))
                .await
                .unwrap()
        else {
            panic!("expected a reply");
        };
        assert!(text.contains("Bilal, Khadija"), "{text}");
        assert!(
            todos::list_open(&mut conn, ctx.chat_id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn done_by_name_and_clearing_todos() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        let apis = Apis::new(times());
        for title in ["go gym", "gym bag wash", "fix the tap"] {
            run_command(&mut conn, &ctx, &apis, add(title, None))
                .await
                .unwrap();
        }
        let done = |name: &str| BotCommand::DoneTodo(Got::Names(vec![name.to_string()]));

        // Exact title wins over a partial match.
        let reply = run_command(&mut conn, &ctx, &apis, done("go gym"))
            .await
            .unwrap()
            .into_text();
        // No due date, no priority: 2 points.
        assert_eq!(reply, "✅ Done: *go gym* · +2 ⭐\n■■■□□□□□□□ 1/3 done");

        // "tap" is in one open title: done.
        let reply = run_command(&mut conn, &ctx, &apis, done("tap"))
            .await
            .unwrap()
            .into_text();
        assert!(reply.starts_with("✅ Done: *fix the tap*"), "{reply}");

        // Nothing left that matches "gym" twice now, but "xyz" matches none.
        let reply = run_command(&mut conn, &ctx, &apis, done("xyz"))
            .await
            .unwrap()
            .into_text();
        assert!(reply.contains("No open todo matches"), "{reply}");

        let clear_done = BotCommand::ClearTodos { done_only: true };
        let reply = run_command(&mut conn, &ctx, &apis, clear_done)
            .await
            .unwrap()
            .into_text();
        assert_eq!(reply, "🧹 Cleared 2 completed todos.");
        let clear_all = BotCommand::ClearTodos { done_only: false };
        let reply = run_command(&mut conn, &ctx, &apis, clear_all)
            .await
            .unwrap()
            .into_text();
        assert_eq!(reply, "🧹 Cleared the todo list (1 todo).");
    }

    #[tokio::test]
    async fn done_by_name_asks_when_several_match() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        let apis = Apis::new(times());
        for title in ["gym", "gym bag", "gym shoes"] {
            run_command(&mut conn, &ctx, &apis, add(title, None))
                .await
                .unwrap();
        }
        // "bag" is only in one; "shoe" only in one; but "gy" is in all three
        // and none is exactly "gy".
        let ask = BotCommand::DoneTodo(Got::Names(vec!["gy".to_string()]));
        let reply = run_command(&mut conn, &ctx, &apis, ask)
            .await
            .unwrap()
            .into_text();
        assert!(
            reply.contains("matches 3: gym, gym bag, gym shoes"),
            "{reply}"
        );
    }

    #[tokio::test]
    async fn list_then_done_and_remove_by_number() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        for title in ["go gym", "fix the tap", "book dentist"] {
            run_command(&mut conn, &ctx, &Apis::new(times()), add(title, None))
                .await
                .unwrap();
        }

        let Outcome::List { item_ids, .. } = run_command(
            &mut conn,
            &ctx,
            &Apis::new(times()),
            BotCommand::ListTodos(TodoView::All),
        )
        .await
        .unwrap() else {
            panic!("expected a list");
        };
        let items = sent(ListKind::Todo, &item_ids);

        let reply = run_list_reply(
            &mut conn,
            &ctx,
            &defaults(),
            &items,
            ReplyCommand::Tick(vec![1, 3]),
        )
        .await
        .unwrap()
        .into_text();
        assert_eq!(
            reply,
            "✅ Done: *go gym* · +2 ⭐\n✅ Done: *book dentist* · +2 ⭐\n■■■■■■■□□□ 2/3 done"
        );

        // The numbers still refer to the old list: 2 is "fix the tap".
        let reply = run_list_reply(
            &mut conn,
            &ctx,
            &defaults(),
            &items,
            ReplyCommand::Remove(2),
        )
        .await
        .unwrap()
        .into_text();
        assert_eq!(reply, "🗑️ Removed *fix the tap*");

        let reply = run_list_reply(&mut conn, &ctx, &defaults(), &items, ReplyCommand::Done(1))
            .await
            .unwrap()
            .into_text();
        assert!(reply.contains("already ticked off"), "{reply}");

        let reply = run_list_reply(&mut conn, &ctx, &defaults(), &items, ReplyCommand::Done(5))
            .await
            .unwrap()
            .into_text();
        assert!(reply.contains("only has 3 items"), "{reply}");
    }

    #[test]
    fn prayer_day_ticks_what_was_prayed() {
        let text = format_prayer_day(
            &day(),
            &defaults(),
            &[
                (Prayer::Fajr, Status::Prayed),
                (Prayer::Asr, Status::Late),
                (Prayer::Dhuhr, Status::Missed),
            ],
            Moon { elongation: 205.0 },
        );
        assert!(
            text.starts_with(
                "🕌 *Prayer times* (Milton Keynes, Tue 27 Oct)\n\
                 🌙 16 Jumada al-Ula 1448 AH · 🌖 Waning gibbous\n"
            ),
            "{text}"
        );
        assert!(text.contains("✅ 1. Fajr 04:55–06:49\n"), "{text}");
        assert!(text.contains("❌ 2. Dhuhr 11:47–14:16\n"), "{text}");
        assert!(text.contains("🕐 3. Asr 14:16–16:44\n"), "{text}");
        assert!(text.contains("⬜ 5. Isha 18:31–23:46\n"), "{text}");
        assert!(text.contains("`+prayed asr on time`"), "{text}");
    }

    #[test]
    fn prayer_day_without_a_hijri_date_still_shows_the_phase() {
        let day = DayTimes {
            hijri: None,
            ..day()
        };
        let text = format_prayer_day(&day, &defaults(), &[], Moon { elongation: 0.0 });
        assert!(!text.contains("AH"), "{text}");
        assert!(text.contains(")\n🌑 New moon\n⬜ 1. Fajr"), "{text}");
    }

    #[tokio::test]
    async fn prayers_command_shows_the_senders_own_ticks() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        ctx.now = "2026-10-27T15:00:00Z".parse().unwrap();
        prayers::mark_prayed(&mut conn, ctx.sender.id, day().date, Prayer::Dhuhr, ctx.now)
            .await
            .unwrap();

        let Outcome::Reply(text) =
            run_command(&mut conn, &ctx, &Apis::new(times()), BotCommand::Prayers)
                .await
                .unwrap()
        else {
            panic!("expected a reply");
        };
        assert!(text.contains("✅ 2. Dhuhr 11:47"), "{text}");
        assert!(text.contains("⬜ 3. Asr 14:16"), "{text}");
    }

    #[tokio::test]
    async fn prayer_reminders_on_and_off() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;

        let on = run_command(
            &mut conn,
            &ctx,
            &Apis::new(times()),
            BotCommand::PrayerReminders(true),
        )
        .await
        .unwrap();
        assert!(matches!(on, Outcome::Reply(t) if t.contains("on")));
        assert_eq!(prayers::reminder_chats(&mut conn).await.unwrap().len(), 1);

        run_command(
            &mut conn,
            &ctx,
            &Apis::new(times()),
            BotCommand::PrayerReminders(false),
        )
        .await
        .unwrap();
        assert!(prayers::reminder_chats(&mut conn).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn prayed_command_marks_today_and_shows_the_day() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        ctx.now = "2026-10-27T19:00:00Z".parse().unwrap();

        let command = BotCommand::Prayed(Checkin::today(vec![Prayer::Fajr, Prayer::Isha]));
        let Outcome::Reply(text) = run_command(&mut conn, &ctx, &Apis::new(times()), command)
            .await
            .unwrap()
        else {
            panic!("expected a reply");
        };
        assert!(text.contains("✅ 1. Fajr 04:55"), "{text}");
        assert!(text.contains("⬜ 3. Asr 14:16"), "{text}");
        assert!(text.contains("✅ 5. Isha 18:31"), "{text}");

        let mut prayed = prayers::prayed_on(&mut conn, ctx.sender.id, day().date)
            .await
            .unwrap();
        prayed.sort_by_key(|p| p.as_db_str());
        assert_eq!(prayed, [Prayer::Fajr, Prayer::Isha]);
    }

    #[tokio::test]
    async fn isha_ticked_after_midnight_counts_for_last_night() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        // 27 Oct's Isha runs to 00:30 on the 28th; it's 00:10.
        let mut last_night = day();
        last_night.windows[4].end = "2026-10-28T00:30:00Z".parse().unwrap();
        let mut today = day();
        today.date = NaiveDate::from_ymd_opt(2026, 10, 28).unwrap();
        today.windows[0].start = "2026-10-28T04:56:00Z".parse().unwrap();
        let apis = Apis::new(PrayerTimes::with_cached_days(
            defaults(),
            vec![last_night.clone(), today.clone()],
        ));
        ctx.now = "2026-10-28T00:10:00Z".parse().unwrap();

        let command = BotCommand::Prayed(Checkin::today(vec![Prayer::Isha]));
        let text = run_command(&mut conn, &ctx, &apis, command)
            .await
            .unwrap()
            .into_text();
        assert!(
            text.contains("Tue 27 Oct"),
            "shows last night's day: {text}"
        );
        assert!(text.contains("✅ 5. Isha"), "{text}");
        let user = ctx.sender.id;
        let last = prayers::prayed_on(&mut conn, user, last_night.date).await;
        assert_eq!(last.unwrap(), [Prayer::Isha]);
        let now = prayers::prayed_on(&mut conn, user, today.date).await;
        assert!(now.unwrap().is_empty());

        // Fajr named at the same time is today's: both days are shown.
        let command = BotCommand::Prayed(Checkin::today(vec![Prayer::Fajr]));
        let text = run_command(&mut conn, &ctx, &apis, command)
            .await
            .unwrap()
            .into_text();
        assert!(text.contains("Wed 28 Oct"), "{text}");
        assert_eq!(
            prayers::prayed_on(&mut conn, ctx.sender.id, today.date)
                .await
                .unwrap(),
            [Prayer::Fajr]
        );
    }

    // ---- shopping ----

    fn shop_cmd(items: &[&str]) -> BotCommand {
        BotCommand::AddShop(items.iter().map(|i| i.to_string()).collect())
    }

    /// Runs `/shop` and returns (text, the list a reply would see).
    async fn shop_list(conn: &mut SqliteConnection, ctx: &Context) -> (String, SentList) {
        let Outcome::List {
            text,
            kind,
            item_ids,
        } = run_command(conn, ctx, &Apis::new(times()), BotCommand::ListShop)
            .await
            .unwrap()
        else {
            panic!("expected a list");
        };
        assert_eq!(kind, ListKind::Shop);
        (text, sent(kind, &item_ids))
    }

    #[test]
    fn shop_list_explains_both_ways_to_tick_off() {
        let items = [ShopItem {
            id: 1,
            name: "milk".to_string(),
            bought: false,
        }];
        let text = format_shop_list(&items);
        assert!(text.contains("reply to this message"), "{text}");
        assert!(text.contains("`+got 1 3`"), "{text}");
        assert!(text.contains("`+got milk`"), "{text}");
    }

    #[test]
    fn shop_list_marks_bought_and_unbought() {
        let item = |id, name: &str, bought| ShopItem {
            id,
            name: name.to_string(),
            bought,
        };
        let text = format_shop_list(&[item(1, "milk", false), item(2, "bread", true)]);
        assert!(text.contains("1. ⬜ milk\n2. ✅ ~bread~\n"), "{text}");
        assert!(text.contains("`/shop clear bought`"), "{text}");

        let all_bought = format_shop_list(&[item(2, "bread", true)]);
        assert!(all_bought.contains("Everything's bought."), "{all_bought}");
        assert!(!all_bought.contains("Tick off"), "{all_bought}");
    }

    #[test]
    fn shop_added_reply_mentions_duplicates() {
        assert_eq!(
            format_shop_added(&["milk", "eggs"], &[]),
            "🛒 Added milk, eggs"
        );
        assert_eq!(
            format_shop_added(&["eggs"], &["Milk"]),
            "🛒 Added eggs
Already on the list: Milk"
        );
        assert_eq!(
            format_shop_added(&[], &["milk"]),
            "Already on the list: milk"
        );
    }

    #[test]
    fn empty_shop_list_says_so() {
        assert!(format_shop_list(&[]).starts_with("🛒 The shopping list is empty."));
    }

    #[tokio::test]
    async fn add_skips_items_already_on_the_list() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;

        run_command(&mut conn, &ctx, &Apis::new(times()), shop_cmd(&["Milk"]))
            .await
            .unwrap();
        let reply = run_command(
            &mut conn,
            &ctx,
            &Apis::new(times()),
            shop_cmd(&["milk", "eggs", "Eggs"]),
        )
        .await
        .unwrap();
        assert_eq!(
            reply,
            Outcome::Reply(
                "🛒 Added eggs
Already on the list: milk, Eggs"
                    .to_string()
            )
        );

        let (text, items) = shop_list(&mut conn, &ctx).await;
        assert_eq!(items.items.len(), 2);
        assert!(text.contains("1. ⬜ Milk\n2. ⬜ eggs\n"), "{text}");
    }

    #[tokio::test]
    async fn tick_remove_and_clear_by_number() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        run_command(
            &mut conn,
            &ctx,
            &Apis::new(times()),
            shop_cmd(&["milk", "eggs", "bread", "rice"]),
        )
        .await
        .unwrap();
        let (_, items) = shop_list(&mut conn, &ctx).await;

        let reply = run_list_reply(
            &mut conn,
            &ctx,
            &defaults(),
            &items,
            ReplyCommand::Tick(vec![1, 3]),
        )
        .await
        .unwrap()
        .into_text();
        assert_eq!(reply, "✅ @447700900001 bought *milk*, *bread*");

        let reply = run_list_reply(
            &mut conn,
            &ctx,
            &defaults(),
            &items,
            ReplyCommand::Remove(4),
        )
        .await
        .unwrap()
        .into_text();
        assert_eq!(reply, "🗑️ Removed *rice*");

        let reply = run_list_reply(&mut conn, &ctx, &defaults(), &items, ReplyCommand::Done(1))
            .await
            .unwrap()
            .into_text();
        assert!(reply.contains("already ticked off"), "{reply}");

        // Eggs still to buy first, then milk and bread, bought.
        let (text, items) = shop_list(&mut conn, &ctx).await;
        assert_eq!(items.items.len(), 3);
        assert!(
            text.contains("1. ⬜ eggs\n2. ✅ ~milk~\n3. ✅ ~bread~\n"),
            "{text}"
        );

        let clear_bought = || BotCommand::ClearShop { bought_only: true };
        let cleared = run_command(&mut conn, &ctx, &Apis::new(times()), clear_bought())
            .await
            .unwrap();
        assert_eq!(
            cleared,
            Outcome::Reply("🧹 Cleared 2 bought items.".to_string())
        );
        let again = run_command(&mut conn, &ctx, &Apis::new(times()), clear_bought())
            .await
            .unwrap();
        assert_eq!(
            again,
            Outcome::Reply("🛒 Nothing bought to clear.".to_string())
        );
    }

    #[tokio::test]
    async fn shop_clear_empties_the_whole_list() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        let clear = || BotCommand::ClearShop { bought_only: false };

        // Nothing ticked off: the old behaviour would have kept all three.
        run_command(
            &mut conn,
            &ctx,
            &Apis::new(times()),
            shop_cmd(&["milk", "eggs", "bread"]),
        )
        .await
        .unwrap();
        let cleared = run_command(&mut conn, &ctx, &Apis::new(times()), clear())
            .await
            .unwrap();
        assert_eq!(
            cleared,
            Outcome::Reply("🧹 Cleared the shopping list (3 items).".to_string())
        );
        let (_, left) = shop_list(&mut conn, &ctx).await;
        assert!(left.items.is_empty());

        let again = run_command(&mut conn, &ctx, &Apis::new(times()), clear())
            .await
            .unwrap();
        assert_eq!(
            again,
            Outcome::Reply("🛒 The shopping list is already empty.".to_string())
        );
    }

    #[tokio::test]
    async fn bought_in_a_dm_has_no_mention() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        ctx.is_group = false;
        run_command(&mut conn, &ctx, &Apis::new(times()), shop_cmd(&["milk"]))
            .await
            .unwrap();

        let got = BotCommand::GotShop(Got::Names(vec!["milk".to_string()]));
        let reply = run_command(&mut conn, &ctx, &Apis::new(times()), got)
            .await
            .unwrap();
        assert_eq!(reply, Outcome::Reply("✅ Bought *milk*".to_string()));
    }

    #[tokio::test]
    async fn two_groups_have_separate_shopping_lists() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        let other_chat = chats::find_or_create(&mut conn, "120363000000000009@g.us", None, ctx.now)
            .await
            .unwrap();
        let other = Context {
            chat_id: other_chat,
            is_group: true,
            sender: ctx.sender.clone(),
            now: ctx.now,
        };

        run_command(&mut conn, &ctx, &Apis::new(times()), shop_cmd(&["milk"]))
            .await
            .unwrap();
        let (_, theirs) = shop_list(&mut conn, &other).await;
        assert!(theirs.items.is_empty());

        // "milk" isn't a duplicate in the other group.
        let reply = run_command(&mut conn, &other, &Apis::new(times()), shop_cmd(&["milk"]))
            .await
            .unwrap();
        assert_eq!(reply, Outcome::Reply("🛒 Added milk".to_string()));
    }

    #[tokio::test]
    async fn got_by_name_ticks_items_ignoring_case() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        run_command(
            &mut conn,
            &ctx,
            &Apis::new(times()),
            shop_cmd(&["Milk", "eggs"]),
        )
        .await
        .unwrap();

        let got = BotCommand::GotShop(Got::Names(vec!["milk".to_string(), "bread".to_string()]));
        let reply = run_command(&mut conn, &ctx, &Apis::new(times()), got)
            .await
            .unwrap();
        assert_eq!(
            reply,
            Outcome::Mentioning {
                text: "✅ @447700900001 bought *Milk*
❓ bread isn't on the list."
                    .to_string(),
                mentions: vec!["447700900001".to_string()],
            }
        );

        let (text, _) = shop_list(&mut conn, &ctx).await;
        assert!(text.contains("1. ⬜ eggs\n2. ✅ ~Milk~\n"), "{text}");
    }

    #[tokio::test]
    async fn got_by_number_uses_the_latest_shop_list() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        let got = || BotCommand::GotShop(Got::Numbers(vec![2]));

        // No list sent yet.
        let reply = run_command(&mut conn, &ctx, &Apis::new(times()), got())
            .await
            .unwrap();
        assert!(
            matches!(&reply, Outcome::Reply(t) if t.contains("Send /shop")),
            "{reply:?}"
        );

        run_command(
            &mut conn,
            &ctx,
            &Apis::new(times()),
            shop_cmd(&["milk", "eggs"]),
        )
        .await
        .unwrap();
        // What the webhook does after sending a /shop list.
        let (_, list) = shop_list(&mut conn, &ctx).await;
        let ids: Vec<i64> = list.items.iter().map(|i| i.item_id).collect();
        messages::save_list(
            &mut conn,
            "3EB0LIST",
            ctx.chat_id,
            ListKind::Shop,
            &ids,
            ctx.now,
        )
        .await
        .unwrap();

        let reply = run_command(&mut conn, &ctx, &Apis::new(times()), got())
            .await
            .unwrap();
        assert_eq!(
            reply,
            Outcome::Mentioning {
                text: "✅ @447700900001 bought *eggs*".to_string(),
                mentions: vec!["447700900001".to_string()],
            }
        );
    }

    /// Sends `/todo all` and `/shop`, and saves both lists the way the
    /// webhook does after sending them: the todo list first, so the shop
    /// list is the newest list in the chat.
    async fn save_both_lists(conn: &mut SqliteConnection, ctx: &Context) {
        let Outcome::List { item_ids, .. } = typed(conn, ctx, "/todo all").await else {
            panic!("expected a list");
        };
        messages::save_list(
            conn,
            "TODOLIST",
            ctx.chat_id,
            ListKind::Todo,
            &item_ids,
            ctx.now,
        )
        .await
        .unwrap();
        let (_, list) = shop_list(conn, ctx).await;
        let ids: Vec<i64> = list.items.iter().map(|i| i.item_id).collect();
        messages::save_list(conn, "SHOPLIST", ctx.chat_id, ListKind::Shop, &ids, ctx.now)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn done_and_skip_by_number_use_the_latest_todo_list() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        typed(&mut conn, &ctx, "+todo go gym, bins").await;
        run_command(&mut conn, &ctx, &Apis::new(times()), shop_cmd(&["milk"]))
            .await
            .unwrap();
        save_both_lists(&mut conn, &ctx).await;

        // The shop list is newer, but `+done` / `+skip` only look at todo
        // lists.
        let reply = typed(&mut conn, &ctx, "+done 1").await.into_text();
        assert!(reply.starts_with("✅ Done: *go gym*"), "{reply}");
        let reply = typed(&mut conn, &ctx, "+skip 2").await.into_text();
        assert!(reply.starts_with("⏭️ Skipped: *bins*"), "{reply}");
    }

    #[tokio::test]
    async fn a_reply_acts_on_the_kind_of_list_it_quotes() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        typed(&mut conn, &ctx, "+todo go gym").await;
        run_command(&mut conn, &ctx, &Apis::new(times()), shop_cmd(&["milk"]))
            .await
            .unwrap();
        save_both_lists(&mut conn, &ctx).await;

        // What the webhook does with a reply: read back the quoted list.
        let todo = messages::list_items(&mut conn, "TODOLIST", ctx.chat_id)
            .await
            .unwrap()
            .expect("a saved list");
        let shop = messages::list_items(&mut conn, "SHOPLIST", ctx.chat_id)
            .await
            .unwrap()
            .expect("a saved list");

        let reply = run_list_reply(&mut conn, &ctx, &defaults(), &todo, ReplyCommand::Done(1))
            .await
            .unwrap()
            .into_text();
        assert!(reply.starts_with("✅ Done: *go gym*"), "{reply}");

        let reply = run_list_reply(&mut conn, &ctx, &defaults(), &shop, ReplyCommand::Skip(1))
            .await
            .unwrap()
            .into_text();
        assert!(reply.contains("`skip` is for todos"), "{reply}");
        let reply = run_list_reply(&mut conn, &ctx, &defaults(), &shop, ReplyCommand::Done(1))
            .await
            .unwrap()
            .into_text();
        assert_eq!(reply, "✅ @447700900001 bought *milk*");
    }

    // ---- settings ----

    async fn settings_reply(
        conn: &mut SqliteConnection,
        ctx: &Context,
        command: SettingsCommand,
    ) -> String {
        match run_command(
            conn,
            ctx,
            &Apis::new(times()),
            BotCommand::Settings(command),
        )
        .await
        .unwrap()
        {
            Outcome::Reply(text) => text,
            other => panic!("expected a reply, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn settings_show_the_defaults_then_changes() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;

        let text = settings_reply(&mut conn, &ctx, SettingsCommand::Show).await;
        assert!(text.contains("*Settings* (this group)"), "{text}");
        assert!(text.contains("📍 Milton Keynes, GB"), "{text}");
        assert!(text.contains("Europe/London"), "{text}");
        assert!(
            text.contains("Method 3: Muslim World League; Asr Shafi'i"),
            "{text}"
        );
        assert!(text.contains("reminders: off"), "{text}");

        settings_reply(
            &mut conn,
            &ctx,
            SettingsCommand::PrayerSchool(prayer::HANAFI),
        )
        .await;
        settings_reply(&mut conn, &ctx, SettingsCommand::PrayerMethod(2)).await;
        let text = settings_reply(
            &mut conn,
            &ctx,
            SettingsCommand::Timezone("Asia/Karachi".into()),
        )
        .await;
        assert!(text.contains("Asia/Karachi"), "{text}");
        assert!(
            text.contains("Method 2: Islamic Society of North America (ISNA); Asr Hanafi"),
            "{text}"
        );

        let text = settings_reply(&mut conn, &ctx, SettingsCommand::Reset).await;
        assert!(text.starts_with("Back to the defaults."), "{text}");
        assert!(text.contains("Method 3"), "{text}");
    }

    #[tokio::test]
    async fn bad_settings_are_explained_and_not_saved() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;

        let text = settings_reply(&mut conn, &ctx, SettingsCommand::PrayerMethod(6)).await;
        assert!(text.contains("no method 6"), "{text}");
        let text = settings_reply(
            &mut conn,
            &ctx,
            SettingsCommand::Timezone("Mars/Olympus".into()),
        )
        .await;
        assert!(text.contains("don't know the timezone"), "{text}");
        let text = settings_reply(&mut conn, &ctx, SettingsCommand::PrayerEnding(0)).await;
        assert!(text.contains("between 1 and 120"), "{text}");

        let stored = stored::get(&mut conn, ctx.chat_id).await.unwrap();
        assert_eq!(stored.overrides, crate::settings::Overrides::default());
    }

    #[tokio::test]
    async fn prayer_times_follow_the_chats_timezone() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        ctx.now = "2026-10-27T15:00:00Z".parse().unwrap();
        // Same city's times, read on a Karachi clock (UTC+5).
        stored::set_timezone(&mut conn, ctx.chat_id, "Asia/Karachi")
            .await
            .unwrap();
        let karachi = crate::settings::Settings {
            timezone: chrono_tz::Asia::Karachi,
            ..defaults()
        };
        let source = PrayerTimes::with_cached(karachi, day());

        let Outcome::Reply(text) =
            run_command(&mut conn, &ctx, &Apis::new(source), BotCommand::Prayers)
                .await
                .unwrap()
        else {
            panic!("expected a reply");
        };
        assert!(text.contains("3. Asr 19:16–21:44"), "{text}");
    }

    #[tokio::test]
    async fn everyone_lists_members_and_is_group_only() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;

        let text = settings_reply(&mut conn, &ctx, SettingsCommand::Everyone).await;
        assert!(text.contains("• Bilal: defaults"), "{text}");
        assert!(text.contains("• Khadija: defaults"), "{text}");

        ctx.is_group = false;
        let text = settings_reply(&mut conn, &ctx, SettingsCommand::Everyone).await;
        assert!(text.contains("only you"), "{text}");
    }

    #[tokio::test]
    async fn methods_lists_every_method() {
        let text = format_methods();
        assert!(text.contains("3. Muslim World League\n"));
        assert!(text.contains("23. Ministry of Awqaf, Jordan\n"));
        assert!(!text.contains("\n6. "));
    }

    #[tokio::test]
    async fn prayers_in_a_group_use_the_senders_own_settings() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        ctx.now = "2026-10-27T15:00:00Z".parse().unwrap();

        // The sender's DM says Karachi time; the group has no settings.
        let dm = chats::find_or_create(
            &mut conn,
            "447700900001@s.whatsapp.net",
            Some(ctx.sender.id),
            ctx.now,
        )
        .await
        .unwrap();
        stored::set_timezone(&mut conn, dm, "Asia/Karachi")
            .await
            .unwrap();

        // Times for "Milton Keynes in Karachi time" come from the cache.
        let karachi = Settings {
            timezone: chrono_tz::Asia::Karachi,
            ..defaults()
        };
        let source = PrayerTimes::with_cached(karachi, day());
        let Outcome::Reply(text) =
            run_command(&mut conn, &ctx, &Apis::new(source), BotCommand::Prayers)
                .await
                .unwrap()
        else {
            panic!("expected a reply");
        };
        assert!(text.contains("3. Asr 19:16–21:44"), "{text}");
    }

    // ---- outside services that are down ----

    /// Settings with no saved coordinates, so `/weather` has to look the
    /// town up by name.
    fn by_name() -> Settings {
        Settings {
            coordinates: None,
            ..defaults()
        }
    }

    /// The reply `command` gives, as text. Panics on anything else, so a
    /// test fails loudly if the command errors instead of answering.
    async fn reply_to(
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

    #[tokio::test]
    async fn prayers_reply_when_the_prayer_times_service_is_down() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        // Nothing cached, and AlAdhan answers 500 to everything.
        let down = crate::testkit::failing_service().await;
        let apis = Apis::new(PrayerTimes::new(defaults()).with_base_url(&down));

        let text = reply_to(&mut conn, &ctx, &apis, BotCommand::Prayers).await;
        assert!(
            text.contains("prayer-times service isn't answering"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn prayed_ticks_nothing_when_the_prayer_times_service_is_down() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        let down = crate::testkit::failing_service().await;
        let apis = Apis::new(PrayerTimes::new(defaults()).with_base_url(&down));

        let command = BotCommand::Prayed(Checkin::today(vec![Prayer::Fajr]));
        let text = reply_to(&mut conn, &ctx, &apis, command).await;
        assert!(
            text.contains("prayer-times service isn't answering"),
            "{text}"
        );
        let today = defaults().today(ctx.now);
        let prayed = prayers::prayed_on(&mut conn, ctx.sender.id, today)
            .await
            .unwrap();
        assert!(prayed.is_empty(), "{prayed:?}");
    }

    #[tokio::test]
    async fn weather_says_service_down_when_the_geocoder_fails() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        let mut apis = Apis::new(PrayerTimes::new(by_name()));
        apis.geo = crate::geo::Geo::with_geocoding_url(&crate::testkit::failing_service().await);

        let text = reply_to(&mut conn, &ctx, &apis, BotCommand::Weather(When::Now)).await;
        assert!(text.contains("isn't answering"), "{text}");
        assert!(!text.contains("couldn't find"), "{text}");
    }

    #[tokio::test]
    async fn weather_says_not_found_for_an_unknown_town() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        // The fake geocoder only knows Leicester, not Milton Keynes.
        let mut apis = Apis::new(PrayerTimes::new(by_name()));
        apis.geo = crate::geo::Geo::with_geocoding_url(&crate::testkit::fake_geocoder().await);

        let text = reply_to(&mut conn, &ctx, &apis, BotCommand::Weather(When::Now)).await;
        assert!(text.contains("couldn't find Milton Keynes, GB"), "{text}");
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

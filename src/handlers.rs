//! What the bot does for each command: database changes, then the text to
//! send back. No GOWA here; the webhook does the sending.

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;

use crate::aladhan::PrayerTimes;
use crate::error::AppError;
use crate::help;
use crate::parser::{BotCommand, Got, ReplyCommand, SettingsCommand};
use crate::person::Assignee;
use crate::prayer::{self, DayTimes, Prayer};
use crate::repo::messages::{self, ListItem};
use crate::repo::prayers;
use crate::repo::settings::{self as stored, MemberSettings};
use crate::repo::shop::{self, ShopItem};
use crate::repo::todos::{self, OpenTodo};
use crate::repo::users::{self, User};
use crate::settings::Settings;

const ADDED: &str = "📝";
const LIST: &str = "📋";
const DONE: &str = "✅";
const REMOVED: &str = "🗑️";
const PROBLEM: &str = "❓";
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

/// `item_type` values in `wa_message_context` (the schema's CHECK allows
/// exactly these two).
pub const TODO_ITEM: &str = "todo";
pub const SHOP_ITEM: &str = "shop";

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
    /// saves them, with `item_type`, against the sent message's ID so
    /// replies can find them.
    List {
        text: String,
        /// `TODO_ITEM` or `SHOP_ITEM`.
        item_type: &'static str,
        item_ids: Vec<i64>,
    },
}

/// Runs a command typed as a normal message.
///
/// `prayer_times` is used by the prayer and settings commands. It answers
/// from its cache almost always; at most once a day per location it calls
/// AlAdhan first (and `/settings city` checks the city with AlAdhan).
pub async fn run_command(
    conn: &mut SqliteConnection,
    ctx: &Context,
    prayer_times: &PrayerTimes,
    command: BotCommand,
) -> Result<Outcome, AppError> {
    let text = match command {
        BotCommand::AddTodo { title, assignee } => add_todo(conn, ctx, &title, assignee).await?,
        BotCommand::ListTodos => {
            let open = todos::list_open(conn, ctx.chat_id).await?;
            return Ok(Outcome::List {
                text: format_todo_list(&open),
                item_type: TODO_ITEM,
                item_ids: open.iter().map(|t| t.id).collect(),
            });
        }
        BotCommand::AddShop(names) => add_shop(conn, ctx, &names).await?,
        BotCommand::ListShop => {
            let to_buy = shop::list_to_buy(conn, ctx.chat_id).await?;
            let bought = shop::count_bought(conn, ctx.chat_id).await?;
            return Ok(Outcome::List {
                text: format_shop_list(&to_buy, bought),
                item_type: SHOP_ITEM,
                item_ids: to_buy.iter().map(|i| i.id).collect(),
            });
        }
        BotCommand::GotShop(Got::Numbers(numbers)) => {
            // Same as replying `1 3` to the newest `/shop` list here.
            let items = messages::latest_list_items(conn, ctx.chat_id, SHOP_ITEM).await?;
            if items.is_empty() {
                format!("{PROBLEM} No shopping list sent here yet. Send /shop, then `+got 1 3`.")
            } else {
                run_list_reply(conn, ctx, &items, ReplyCommand::Tick(numbers)).await?
            }
        }
        BotCommand::GotShop(Got::Names(names)) => got_by_name(conn, ctx, &names).await?,
        BotCommand::ClearShop => match shop::clear_bought(conn, ctx.chat_id).await? {
            0 => format!("{SHOP} Nothing bought to clear."),
            1 => format!("{CLEARED} Cleared 1 bought item."),
            n => format!("{CLEARED} Cleared {n} bought items."),
        },
        BotCommand::Prayers => {
            let (settings, _) = chat_settings(conn, ctx, prayer_times).await?;
            prayer_day(conn, ctx, prayer_times, &settings).await?
        }
        BotCommand::Prayed(names) => {
            // For people who didn't catch the reminder: same effect as
            // reacting to it, for today (in this chat's timezone).
            let (settings, _) = chat_settings(conn, ctx, prayer_times).await?;
            let today = settings.today(ctx.now);
            for prayer in names {
                prayers::mark_prayed(conn, ctx.sender.id, today, prayer, ctx.now).await?;
            }
            prayer_day(conn, ctx, prayer_times, &settings).await?
        }
        BotCommand::PrayerReminders(on) => {
            stored::set_prayer_reminders(conn, ctx.chat_id, on).await?;
            let (settings, _) = chat_settings(conn, ctx, prayer_times).await?;
            if on {
                format!(
                    "{BELL} Prayer reminders are on in this chat ({}): when each prayer \
                     starts, and {} min before it ends. React to a reminder once you've prayed.",
                    settings.city, settings.prayer_ending_minutes
                )
            } else {
                format!("{BELL_OFF} Prayer reminders are off in this chat.")
            }
        }
        BotCommand::Settings(command) => settings_command(conn, ctx, prayer_times, command).await?,
        BotCommand::Help(topic) => help::page(topic.as_deref()),
    };
    Ok(Outcome::Reply(text))
}

/// `+got milk, eggs`: ticks each named item off, one line per name.
async fn got_by_name(
    conn: &mut SqliteConnection,
    ctx: &Context,
    names: &[String],
) -> Result<String, AppError> {
    let mut lines = Vec::new();
    for name in names {
        let bought = match shop::find_to_buy(conn, ctx.chat_id, name).await? {
            Some(id) => shop::mark_bought(conn, id, ctx.chat_id, ctx.sender.id, ctx.now).await?,
            None => None,
        };
        lines.push(match bought {
            // The stored name, with the capitals it was added with.
            Some(stored) => format!("{DONE} Bought: *{stored}*"),
            None => format!("{PROBLEM} {name} isn't on the list."),
        });
    }
    Ok(lines.join("\n"))
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
    prayer_times: &PrayerTimes,
) -> Result<(Settings, bool), AppError> {
    let stored = stored::get(conn, ctx.chat_id).await?;
    Ok((
        stored.overrides.apply(&prayer_times.defaults),
        stored.prayer_reminders,
    ))
}

/// Today's prayer times for these settings, with the sender's ticks.
async fn prayer_day(
    conn: &mut SqliteConnection,
    ctx: &Context,
    prayer_times: &PrayerTimes,
    settings: &Settings,
) -> Result<String, AppError> {
    let today = settings.today(ctx.now);
    let day = prayer_times
        .for_date(&settings.prayer_location(), today)
        .await?;
    let prayed = prayers::prayed_on(conn, ctx.sender.id, today).await?;
    Ok(format_prayer_day(&day, settings, &prayed))
}

/// `/settings ...`. Each change is checked first, then saved, then the
/// reply shows the chat's full settings so people see the result.
async fn settings_command(
    conn: &mut SqliteConnection,
    ctx: &Context,
    prayer_times: &PrayerTimes,
    command: SettingsCommand,
) -> Result<String, AppError> {
    let (current, _) = chat_settings(conn, ctx, prayer_times).await?;
    let id = ctx.chat_id;

    // `note` is an extra first line for the reply; most changes have none.
    let note = match command {
        SettingsCommand::Show => None,
        SettingsCommand::Everyone => {
            if !ctx.is_group {
                return Ok(format!("{PEOPLE} In a DM there's only you: see /settings."));
            }
            let members = stored::members(conn, id).await?;
            return Ok(format_everyone(&members, &prayer_times.defaults));
        }
        SettingsCommand::Methods => return Ok(format_methods()),
        SettingsCommand::Reset => {
            stored::reset(conn, id).await?;
            Some("Back to the defaults.".to_string())
        }
        SettingsCommand::City { city, country } => {
            // No country typed: keep the one already in use.
            let country = country.unwrap_or_else(|| current.country.clone());
            // Asking AlAdhan both checks the city exists and gives its
            // timezone, so nobody has to look that up separately.
            match prayer_times
                .timezone_of(&city, &country, current.today(ctx.now))
                .await
            {
                Ok(timezone) => {
                    stored::set_location(conn, id, &city, &country, timezone.name()).await?;
                    Some(format!("Timezone set to {timezone} to match."))
                }
                Err(_) => {
                    return Ok(format!(
                        "{PROBLEM} I couldn't find {city}, {country}. \
                         Try e.g. `/settings city Leicester, GB`."
                    ));
                }
            }
        }
        SettingsCommand::Timezone(name) => match name.parse::<chrono_tz::Tz>() {
            Ok(timezone) => {
                stored::set_timezone(conn, id, timezone.name()).await?;
                None
            }
            Err(_) => {
                return Ok(format!(
                    "{PROBLEM} I don't know the timezone {name}. Use a name like \
                     Europe/London or Asia/Karachi."
                ));
            }
        },
        SettingsCommand::PrayerMethod(method) => {
            if prayer::method_name(method).is_none() {
                return Ok(format!(
                    "{PROBLEM} There's no method {method}. /settings methods lists them."
                ));
            }
            stored::set_prayer_method(conn, id, method).await?;
            None
        }
        SettingsCommand::PrayerSchool(school) => {
            stored::set_prayer_school(conn, id, school).await?;
            None
        }
        SettingsCommand::PrayerEnding(minutes) => {
            if !(1..=120).contains(&minutes) {
                return Ok(format!("{PROBLEM} Pick between 1 and 120 minutes."));
            }
            stored::set_prayer_ending_minutes(conn, id, minutes).await?;
            None
        }
    };

    // Read back what's now stored, so the reply shows exactly what's saved.
    let (settings, reminders) = chat_settings(conn, ctx, prayer_times).await?;
    let mut text = note.map(|n| format!("{n}\n\n")).unwrap_or_default();
    text.push_str(&format_settings(&settings, reminders, ctx.is_group));
    Ok(text)
}

async fn add_todo(
    conn: &mut SqliteConnection,
    ctx: &Context,
    title: &str,
    assignee: Option<Assignee>,
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

    todos::add(
        conn,
        ctx.chat_id,
        title,
        ctx.sender.id,
        Some(assignee.id),
        ctx.now,
    )
    .await?;
    Ok(format!(
        "{ADDED} Added *{title}* for {}",
        assignee.display_name
    ))
}

/// Runs a reply to one of the bot's numbered lists. `items` is what that
/// list showed (never empty: the webhook only calls this for real lists).
pub async fn run_list_reply(
    conn: &mut SqliteConnection,
    ctx: &Context,
    items: &[ListItem],
    reply: ReplyCommand,
) -> Result<String, AppError> {
    // Every row of one list has the same type; the first one says which.
    let is_shop = items
        .first()
        .is_some_and(|item| item.item_type == SHOP_ITEM);

    // `done 2` and `1 3` both tick off; `remove` deletes.
    let (numbers, removing) = match reply {
        ReplyCommand::Done(n) => (vec![n], false),
        ReplyCommand::Tick(numbers) => (numbers, false),
        ReplyCommand::Remove(n) => (vec![n], true),
    };

    let mut lines = Vec::new();
    for n in numbers {
        let id = match item_at(items, n) {
            Ok(id) => id,
            Err(problem) => {
                lines.push(problem);
                continue;
            }
        };
        // The repo call that matches the list type and the action. Each
        // returns the item's name, or `None` if it's already gone.
        let name = match (is_shop, removing) {
            (false, false) => {
                todos::complete(conn, id, ctx.chat_id, ctx.sender.id, ctx.now).await?
            }
            (false, true) => todos::remove(conn, id, ctx.chat_id).await?,
            (true, false) => {
                shop::mark_bought(conn, id, ctx.chat_id, ctx.sender.id, ctx.now).await?
            }
            (true, true) => shop::remove(conn, id, ctx.chat_id).await?,
        };
        lines.push(match (name, is_shop, removing) {
            (None, _, _) => gone(n),
            (Some(name), _, true) => format!("{REMOVED} Removed *{name}*"),
            (Some(name), false, false) => format!("{DONE} Done: *{name}*"),
            (Some(name), true, false) => format!("{DONE} Bought: *{name}*"),
        });
    }
    Ok(lines.join("\n"))
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

/// The `/shop` reply. Numbers start at 1 and match `item_ids` order.
fn format_shop_list(to_buy: &[ShopItem], bought: i64) -> String {
    let cleanup = match bought {
        0 => String::new(),
        1 => "\n✔️ 1 bought item; /shop clear removes it.".to_string(),
        n => format!("\n✔️ {n} bought items; /shop clear removes them."),
    };
    if to_buy.is_empty() {
        return format!("{SHOP} Nothing to buy. Add with `+shop milk, eggs`.{cleanup}");
    }

    let mut text = format!("{SHOP} *Shopping list*\n");
    for (i, item) in to_buy.iter().enumerate() {
        text.push_str(&format!("{}. {}\n", i + 1, item.name));
    }
    text.push_str(
        "\nTick off what you bought: reply to this message with the numbers \
         (e.g. `1 3`), or send `+got 1 3` or `+got milk` at any time.",
    );
    text.push_str(&cleanup);
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

/// The `/prayers` reply: each prayer's time on the chat's own clock,
/// ticked if the sender has prayed it.
fn format_prayer_day(day: &DayTimes, settings: &Settings, prayed: &[Prayer]) -> String {
    let mut text = format!(
        "{MOSQUE} *Prayer times* ({}, {})\n",
        settings.city,
        day.date.format("%a %-d %b")
    );
    for window in &day.windows {
        let mark = if prayed.contains(&window.prayer) {
            DONE
        } else {
            NOT_YET
        };
        text.push_str(&format!(
            "{mark} {} {}–{}\n",
            window.prayer,
            settings.clock(window.start),
            settings.clock(window.end)
        ));
    }
    text.push_str("\nTick one off with `+prayed asr`, or react to its reminder.");
    text
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

/// The `/todo` reply. Numbers start at 1 and match `item_ids` order.
fn format_todo_list(open: &[OpenTodo]) -> String {
    if open.is_empty() {
        return format!("{LIST} No open todos. Add one with `+todo fix the tap`.");
    }

    let mut text = format!("{LIST} *Todos*\n");
    // `enumerate` counts from 0, hence `i + 1`.
    for (i, todo) in open.iter().enumerate() {
        text.push_str(&format!("{}. {}", i + 1, todo.title));
        if let Some(name) = &todo.assignee {
            text.push_str(&format!(" ({name})"));
        }
        text.push('\n');
    }
    text.push_str("\nReply to this message with `done 2` or `remove 2`.");
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::person::Person;
    use crate::prayer::Window;
    use crate::repo::chats;
    use crate::settings::tests::defaults;
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
        ListItem {
            position,
            item_type: TODO_ITEM.to_string(),
            item_id,
        }
    }

    // ---- pure text ----

    #[test]
    fn empty_list_says_so() {
        assert!(format_todo_list(&[]).contains("No open todos"));
    }

    #[test]
    fn list_is_numbered_from_one_with_assignees() {
        let open = [
            OpenTodo {
                id: 7,
                title: "go gym".to_string(),
                assignee: Some("Bilal".to_string()),
            },
            OpenTodo {
                id: 3,
                title: "fix the tap".to_string(),
                assignee: None,
            },
        ];
        let text = format_todo_list(&open);
        assert!(text.contains("1. go gym (Bilal)\n"));
        assert!(text.contains("2. fix the tap\n"));
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
        for (name, jid) in [
            ("Bilal", "447700900001@s.whatsapp.net"),
            ("Khadija", "447700900002@s.whatsapp.net"),
        ] {
            let person = Person {
                name: name.to_string(),
                jid: jid.to_string(),
                lid: None,
            };
            let user = users::find_or_register(conn, &person, now)
                .await
                .unwrap()
                .unwrap();
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
            title: title.to_string(),
            assignee,
        }
    }

    #[tokio::test]
    async fn add_assigns_to_sender_or_mentioned_member() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;

        let mine = run_command(&mut conn, &ctx, &times(), add("go gym", None))
            .await
            .unwrap();
        assert_eq!(
            mine,
            Outcome::Reply("📝 Added *go gym* for Bilal".to_string())
        );

        let mention = Some(Assignee::Mentioned("khadija".to_string()));
        let theirs = run_command(&mut conn, &ctx, &times(), add("book dentist", mention))
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
        let Outcome::Reply(text) = run_command(&mut conn, &ctx, &times(), add("x", mention))
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
    async fn list_then_done_and_remove_by_number() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        for title in ["go gym", "fix the tap", "book dentist"] {
            run_command(&mut conn, &ctx, &times(), add(title, None))
                .await
                .unwrap();
        }

        let Outcome::List { item_ids, .. } =
            run_command(&mut conn, &ctx, &times(), BotCommand::ListTodos)
                .await
                .unwrap()
        else {
            panic!("expected a list");
        };
        let items: Vec<ListItem> = item_ids
            .iter()
            .zip(1..)
            .map(|(&id, pos)| item(pos, id))
            .collect();

        let reply = run_list_reply(&mut conn, &ctx, &items, ReplyCommand::Tick(vec![1, 3]))
            .await
            .unwrap();
        assert_eq!(reply, "✅ Done: *go gym*\n✅ Done: *book dentist*");

        // The numbers still refer to the old list: 2 is "fix the tap".
        let reply = run_list_reply(&mut conn, &ctx, &items, ReplyCommand::Remove(2))
            .await
            .unwrap();
        assert_eq!(reply, "🗑️ Removed *fix the tap*");

        let reply = run_list_reply(&mut conn, &ctx, &items, ReplyCommand::Done(1))
            .await
            .unwrap();
        assert!(reply.contains("already ticked off"), "{reply}");

        let reply = run_list_reply(&mut conn, &ctx, &items, ReplyCommand::Done(5))
            .await
            .unwrap();
        assert!(reply.contains("only has 3 items"), "{reply}");
    }

    #[test]
    fn prayer_day_ticks_what_was_prayed() {
        let text = format_prayer_day(&day(), &defaults(), &[Prayer::Fajr, Prayer::Asr]);
        assert!(
            text.starts_with("🕌 *Prayer times* (Milton Keynes, Tue 27 Oct)\n"),
            "{text}"
        );
        assert!(text.contains("✅ Fajr 04:55–06:49\n"), "{text}");
        assert!(text.contains("⬜ Dhuhr 11:47–14:16\n"));
        assert!(text.contains("✅ Asr 14:16–16:44\n"));
        assert!(text.contains("⬜ Isha 18:31–23:46\n"));
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

        let Outcome::Reply(text) = run_command(&mut conn, &ctx, &times(), BotCommand::Prayers)
            .await
            .unwrap()
        else {
            panic!("expected a reply");
        };
        assert!(text.contains("✅ Dhuhr 11:47"), "{text}");
        assert!(text.contains("⬜ Asr 14:16"), "{text}");
    }

    #[tokio::test]
    async fn prayer_reminders_on_and_off() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;

        let on = run_command(&mut conn, &ctx, &times(), BotCommand::PrayerReminders(true))
            .await
            .unwrap();
        assert!(matches!(on, Outcome::Reply(t) if t.contains("on")));
        assert_eq!(prayers::reminder_chats(&mut conn).await.unwrap().len(), 1);

        run_command(
            &mut conn,
            &ctx,
            &times(),
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

        let command = BotCommand::Prayed(vec![Prayer::Fajr, Prayer::Isha]);
        let Outcome::Reply(text) = run_command(&mut conn, &ctx, &times(), command)
            .await
            .unwrap()
        else {
            panic!("expected a reply");
        };
        assert!(text.contains("✅ Fajr 04:55"), "{text}");
        assert!(text.contains("⬜ Asr 14:16"), "{text}");
        assert!(text.contains("✅ Isha 18:31"), "{text}");

        let mut prayed = prayers::prayed_on(&mut conn, ctx.sender.id, day().date)
            .await
            .unwrap();
        prayed.sort_by_key(|p| p.as_db_str());
        assert_eq!(prayed, [Prayer::Fajr, Prayer::Isha]);
    }

    // ---- shopping ----

    fn shop_cmd(items: &[&str]) -> BotCommand {
        BotCommand::AddShop(items.iter().map(|i| i.to_string()).collect())
    }

    /// Runs `/shop` and returns (text, the ListItems a reply would see).
    async fn shop_list(conn: &mut SqliteConnection, ctx: &Context) -> (String, Vec<ListItem>) {
        let Outcome::List {
            text,
            item_type,
            item_ids,
        } = run_command(conn, ctx, &times(), BotCommand::ListShop)
            .await
            .unwrap()
        else {
            panic!("expected a list");
        };
        assert_eq!(item_type, SHOP_ITEM);
        let items = item_ids
            .iter()
            .zip(1..)
            .map(|(&item_id, position)| ListItem {
                position,
                item_type: SHOP_ITEM.to_string(),
                item_id,
            })
            .collect();
        (text, items)
    }

    #[test]
    fn shop_list_explains_both_ways_to_tick_off() {
        let items = [ShopItem {
            id: 1,
            name: "milk".to_string(),
        }];
        let text = format_shop_list(&items, 0);
        assert!(text.contains("reply to this message"), "{text}");
        assert!(text.contains("`+got 1 3`"), "{text}");
        assert!(text.contains("`+got milk`"), "{text}");
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
    fn empty_shop_list_mentions_bought_items_to_clear() {
        assert!(format_shop_list(&[], 0).starts_with("🛒 Nothing to buy"));
        assert!(format_shop_list(&[], 2).contains("2 bought items; /shop clear"));
    }

    #[tokio::test]
    async fn add_skips_items_already_on_the_list() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;

        run_command(&mut conn, &ctx, &times(), shop_cmd(&["Milk"]))
            .await
            .unwrap();
        let reply = run_command(
            &mut conn,
            &ctx,
            &times(),
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
        assert_eq!(items.len(), 2);
        assert!(
            text.contains(
                "1. Milk
2. eggs
"
            ),
            "{text}"
        );
    }

    #[tokio::test]
    async fn tick_remove_and_clear_by_number() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        run_command(
            &mut conn,
            &ctx,
            &times(),
            shop_cmd(&["milk", "eggs", "bread", "rice"]),
        )
        .await
        .unwrap();
        let (_, items) = shop_list(&mut conn, &ctx).await;

        let reply = run_list_reply(&mut conn, &ctx, &items, ReplyCommand::Tick(vec![1, 3]))
            .await
            .unwrap();
        assert_eq!(
            reply,
            "✅ Bought: *milk*
✅ Bought: *bread*"
        );

        let reply = run_list_reply(&mut conn, &ctx, &items, ReplyCommand::Remove(4))
            .await
            .unwrap();
        assert_eq!(reply, "🗑️ Removed *rice*");

        let reply = run_list_reply(&mut conn, &ctx, &items, ReplyCommand::Done(1))
            .await
            .unwrap();
        assert!(reply.contains("already ticked off"), "{reply}");

        // Only eggs left to buy; milk and bread wait to be cleared.
        let (text, items) = shop_list(&mut conn, &ctx).await;
        assert_eq!(items.len(), 1);
        assert!(
            text.contains(
                "1. eggs
"
            ),
            "{text}"
        );
        assert!(text.contains("2 bought items"), "{text}");

        let cleared = run_command(&mut conn, &ctx, &times(), BotCommand::ClearShop)
            .await
            .unwrap();
        assert_eq!(
            cleared,
            Outcome::Reply("🧹 Cleared 2 bought items.".to_string())
        );
        let again = run_command(&mut conn, &ctx, &times(), BotCommand::ClearShop)
            .await
            .unwrap();
        assert_eq!(
            again,
            Outcome::Reply("🛒 Nothing bought to clear.".to_string())
        );
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

        run_command(&mut conn, &ctx, &times(), shop_cmd(&["milk"]))
            .await
            .unwrap();
        let (_, theirs) = shop_list(&mut conn, &other).await;
        assert!(theirs.is_empty());

        // "milk" isn't a duplicate in the other group.
        let reply = run_command(&mut conn, &other, &times(), shop_cmd(&["milk"]))
            .await
            .unwrap();
        assert_eq!(reply, Outcome::Reply("🛒 Added milk".to_string()));
    }

    #[tokio::test]
    async fn got_by_name_ticks_items_ignoring_case() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        run_command(&mut conn, &ctx, &times(), shop_cmd(&["Milk", "eggs"]))
            .await
            .unwrap();

        let got = BotCommand::GotShop(Got::Names(vec!["milk".to_string(), "bread".to_string()]));
        let reply = run_command(&mut conn, &ctx, &times(), got).await.unwrap();
        assert_eq!(
            reply,
            Outcome::Reply(
                "✅ Bought: *Milk*
❓ bread isn't on the list."
                    .to_string()
            )
        );

        let (_, left) = shop_list(&mut conn, &ctx).await;
        assert_eq!(left.len(), 1, "only eggs left");
    }

    #[tokio::test]
    async fn got_by_number_uses_the_latest_shop_list() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        let got = || BotCommand::GotShop(Got::Numbers(vec![2]));

        // No list sent yet.
        let reply = run_command(&mut conn, &ctx, &times(), got()).await.unwrap();
        assert!(
            matches!(&reply, Outcome::Reply(t) if t.contains("Send /shop")),
            "{reply:?}"
        );

        run_command(&mut conn, &ctx, &times(), shop_cmd(&["milk", "eggs"]))
            .await
            .unwrap();
        // What the webhook does after sending a /shop list.
        let (_, items) = shop_list(&mut conn, &ctx).await;
        let ids: Vec<i64> = items.iter().map(|i| i.item_id).collect();
        messages::save_list(&mut conn, "3EB0LIST", ctx.chat_id, SHOP_ITEM, &ids, ctx.now)
            .await
            .unwrap();

        let reply = run_command(&mut conn, &ctx, &times(), got()).await.unwrap();
        assert_eq!(reply, Outcome::Reply("✅ Bought: *eggs*".to_string()));
    }

    // ---- settings ----

    async fn settings_reply(
        conn: &mut SqliteConnection,
        ctx: &Context,
        command: SettingsCommand,
    ) -> String {
        match run_command(conn, ctx, &times(), BotCommand::Settings(command))
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

        let Outcome::Reply(text) = run_command(&mut conn, &ctx, &source, BotCommand::Prayers)
            .await
            .unwrap()
        else {
            panic!("expected a reply");
        };
        assert!(text.contains("Asr 19:16–21:44"), "{text}");
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
}

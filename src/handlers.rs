//! What the bot does for each command: database changes, then the text to
//! send back. No GOWA here; the webhook does the sending.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqliteConnection;

use crate::Apis;
use crate::aladhan::PrayerTimes;
use crate::calendar;
use crate::error::AppError;
use crate::geo::Coordinates;
use crate::help;
use crate::moon::{self, Moon};
use crate::parser::{BotCommand, Got, ReplyCommand, SettingsCommand, Timing, TodoView};
use crate::person::Assignee;
use crate::prayer::{self, DayTimes, Prayer};
use crate::prayer_stats::{self, Status, Summary};
use crate::progress;
use crate::repo::messages::{self, ListItem};
use crate::repo::prayers;
use crate::repo::reports as stored_reports;
use crate::repo::settings::{self as stored, MemberSettings};
use crate::repo::shop::{self, ShopItem};
use crate::repo::todo_reminders;
use crate::repo::todos::{self, ListedTodo};
use crate::repo::users::{self, User};
use crate::reports::{Delivery, Feature, Period, Span};
use crate::settings::Settings;
use crate::settings_edit::{self, Change, Refused, Saved};
use crate::weather::{self, When};

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
const MOON: &str = "🌙";
const CHART: &str = "📊";

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
    /// A reply that @mentions people: `mentions` are their phone numbers,
    /// and `text` contains `@<number>` where each should appear.
    Mentioning { text: String, mentions: Vec<String> },
}

impl Outcome {
    /// Just the text, whatever kind of reply it is (handy in tests).
    pub fn into_text(self) -> String {
        match self {
            Outcome::Reply(text)
            | Outcome::List { text, .. }
            | Outcome::Mentioning { text, .. } => text,
        }
    }
}

/// Runs a command typed as a normal message.
///
/// `prayer_times` is used by the prayer and settings commands. It answers
/// from its cache almost always; at most once a day per location it calls
/// AlAdhan first (and `/settings city` checks the city with AlAdhan).
pub async fn run_command(
    conn: &mut SqliteConnection,
    ctx: &Context,
    apis: &Apis,
    command: BotCommand,
) -> Result<Outcome, AppError> {
    let prayer_times = &apis.prayer_times;
    let text = match command {
        BotCommand::AddTodo { titles, assignee } => add_todo(conn, ctx, &titles, assignee).await?,
        BotCommand::ListTodos(view) => {
            let layout = match view {
                TodoView::ByPerson => Layout::ByPerson,
                TodoView::All => Layout::All,
                TodoView::Mine => Layout::Only(ctx.sender.display_name.clone()),
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
            let settings = my_settings(conn, ctx, prayer_times).await?;
            let items = todos::list_all(conn, ctx.chat_id).await?;
            return Ok(Outcome::List {
                text: format_todo_list(&items, &layout, &settings, ctx.now),
                item_type: TODO_ITEM,
                // Every todo, even in one person's view, so the numbers
                // mean the same todo in every view.
                item_ids: items.iter().map(|t| t.id).collect(),
            });
        }
        BotCommand::DoneTodo(Got::Numbers(numbers)) => {
            // Same as replying `1 3` to the newest `/todo` list here.
            let items = messages::latest_list_items(conn, ctx.chat_id, TODO_ITEM).await?;
            if items.is_empty() {
                format!("{PROBLEM} No todo list sent here yet. Send /todo, then `+done 2`.")
            } else {
                return run_list_reply(conn, ctx, &items, ReplyCommand::Tick(numbers)).await;
            }
        }
        BotCommand::DoneTodo(Got::Names(names)) => done_by_name(conn, ctx, &names).await?,
        BotCommand::ClearTodos { done_only: false } => {
            match todos::clear_all(conn, ctx.chat_id).await? {
                0 => format!("{LIST} There are no todos to clear."),
                1 => format!("{CLEARED} Cleared the todo list (1 todo)."),
                n => format!("{CLEARED} Cleared the todo list ({n} todos)."),
            }
        }
        BotCommand::ClearTodos { done_only: true } => {
            match todos::clear_done(conn, ctx.chat_id).await? {
                0 => format!("{LIST} No completed todos to clear."),
                1 => format!("{CLEARED} Cleared 1 completed todo."),
                n => format!("{CLEARED} Cleared {n} completed todos."),
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
                item_type: SHOP_ITEM,
                item_ids: items.iter().map(|i| i.id).collect(),
            });
        }
        BotCommand::GotShop(Got::Numbers(numbers)) => {
            // Same as replying `1 3` to the newest `/shop` list here.
            let items = messages::latest_list_items(conn, ctx.chat_id, SHOP_ITEM).await?;
            if items.is_empty() {
                format!("{PROBLEM} No shopping list sent here yet. Send /shop, then `+got 1 3`.")
            } else {
                return run_list_reply(conn, ctx, &items, ReplyCommand::Tick(numbers)).await;
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
        BotCommand::Prayers => {
            let settings = my_settings(conn, ctx, prayer_times).await?;
            let today = settings.today(ctx.now);
            prayer_day(conn, ctx, apis, &settings, today).await?
        }
        BotCommand::Prayed(checkin) => {
            // For people who didn't catch the reminder: with no timing, the
            // same as reacting to it. Today (or yesterday) is in the
            // sender's own timezone.
            let settings = my_settings(conn, ctx, prayer_times).await?;
            let today = settings.today(ctx.now);
            let yesterday = today.pred_opt().unwrap_or(today);
            let location = apis.prayer_location(&settings).await?;
            let today_times = prayer_times.for_date(&location, today).await?;
            // Only looked up between midnight and Fajr, when last night's
            // Isha may still be going (see `prayer::checkin_date`).
            let yesterday_times = if prayer::before_fajr(&today_times, ctx.now) {
                Some(prayer_times.for_date(&location, yesterday).await?)
            } else {
                None
            };
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
            dates.sort();
            let mut days = Vec::new();
            for date in dates {
                days.push(prayer_day(conn, ctx, apis, &settings, date).await?);
            }
            days.join("\n\n")
        }
        BotCommand::PrayerReminders(on) => {
            stored::set_prayer_reminders(conn, ctx.chat_id, on).await?;
            let (settings, _) = chat_settings(conn, ctx, prayer_times).await?;
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
            let settings = my_settings(conn, ctx, prayer_times).await?;
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
        BotCommand::Settings(command) => settings_command(conn, ctx, apis, command).await?,
        BotCommand::Weather(when) => weather(conn, ctx, apis, when).await?,
        BotCommand::Moon => moon_command(conn, ctx, apis).await?,
        BotCommand::SharedLocation {
            latitude,
            longitude,
        } => {
            let at = Coordinates {
                latitude,
                longitude,
            };
            shared_location(conn, ctx, apis, at).await?
        }
        BotCommand::Help(topic) => help::page(topic.as_deref()),
    };
    Ok(Outcome::Reply(text))
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
    prayer_times: &PrayerTimes,
) -> Result<(Settings, bool), AppError> {
    let stored = stored::get(conn, ctx.chat_id).await?;
    Ok((
        stored.overrides.apply(&prayer_times.defaults),
        stored.prayer_reminders,
    ))
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

/// A location shared in a DM: saves the exact point, named after the
/// nearest town (OpenStreetMap), with the timezone AlAdhan gives for that
/// point. Prayer times are then fetched for the point itself, so no name
/// ever has to be looked up.
async fn shared_location(
    conn: &mut SqliteConnection,
    ctx: &Context,
    apis: &Apis,
    at: Coordinates,
) -> Result<String, AppError> {
    let (current, reminders) = chat_settings(conn, ctx, &apis.prayer_times).await?;
    let Ok(near) = apis.geo.nearest_place(at).await else {
        return Ok(format!(
            "{PROBLEM} I couldn't work out which town that is. \
             Try `/settings city Leicester, GB` instead."
        ));
    };
    // `names[0]` is the most specific: the town or village itself.
    let name = &near.names[0];
    let timezone = apis
        .prayer_times
        .timezone_at(at, current.today(ctx.now))
        .await
        .unwrap_or(current.timezone);
    stored::set_location(conn, ctx.chat_id, name, &near.country, at, timezone.name()).await?;

    let (settings, _) = chat_settings(conn, ctx, &apis.prayer_times).await?;
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
    let settings = my_settings(conn, ctx, &apis.prayer_times).await?;
    let Ok(at) = apis.coordinates(&settings).await else {
        return Ok(format!(
            "{PROBLEM} I couldn't find {}, {} for the weather. Check `/settings`.",
            settings.city, settings.country
        ));
    };
    match apis.weather.forecast(at, settings.timezone).await {
        Ok(forecast) => Ok(weather::format(&forecast, when, &settings.city)),
        Err(err) => {
            println!("weather: {err}");
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
    let settings = my_settings(conn, ctx, &apis.prayer_times).await?;
    let today = settings.today(ctx.now);

    // `.ok()` turns each `Result` into an `Option`: a failed lookup is
    // "not shown", not an error for the whole reply.
    let at = apis.coordinates(&settings).await.ok();
    let rise_set = match at {
        Some(at) => apis
            .weather
            .moon_times(at, settings.timezone, today)
            .await
            .inspect_err(|err| println!("moon times: {err}"))
            .ok(),
        None => None,
    };
    // The Hijri date comes with the prayer times (usually already cached).
    let hijri = match apis.prayer_location(&settings).await {
        Ok(location) => apis
            .prayer_times
            .for_date(&location, today)
            .await
            .ok()
            .and_then(|day| day.hijri),
        Err(_) => None,
    };
    Ok(moon::format(ctx.now, &settings, rise_set, hijri))
}

/// The sender's own settings: in a DM, the DM's; in a group, their personal
/// ones (from their DM with the bot) on top of the group's. Prayer times
/// and "today" follow the person, so someone abroad sees their own.
async fn my_settings(
    conn: &mut SqliteConnection,
    ctx: &Context,
    prayer_times: &PrayerTimes,
) -> Result<Settings, AppError> {
    let (here, _) = chat_settings(conn, ctx, prayer_times).await?;
    if !ctx.is_group {
        return Ok(here);
    }
    Ok(match stored::personal(conn, ctx.sender.id).await? {
        Some(own) => own.overrides.apply(&here),
        None => here,
    })
}

/// Today's prayer times for these settings, with the sender's ticks.
async fn prayer_day(
    conn: &mut SqliteConnection,
    ctx: &Context,
    apis: &Apis,
    settings: &Settings,
    date: NaiveDate,
) -> Result<String, AppError> {
    let location = apis.prayer_location(settings).await?;
    let day = apis.prayer_times.for_date(&location, date).await?;
    let log: Vec<(Prayer, Status)> = prayers::log_between(conn, ctx.sender.id, date, date)
        .await?
        .into_iter()
        .map(|e| (e.prayer, e.status))
        .collect();
    Ok(format_prayer_day(&day, settings, &log, Moon::at(ctx.now)))
}

/// `/settings ...`. Each change is checked first, then saved, then the
/// reply shows the chat's full settings so people see the result.
async fn settings_command(
    conn: &mut SqliteConnection,
    ctx: &Context,
    apis: &Apis,
    command: SettingsCommand,
) -> Result<String, AppError> {
    let prayer_times = &apis.prayer_times;
    let (current, _) = chat_settings(conn, ctx, prayer_times).await?;
    let id = ctx.chat_id;

    // The changes are checked and saved by `settings_edit`, under the same
    // rules the admin dashboard uses; only the wording here is the bot's.
    let change = match command {
        SettingsCommand::Show => None,
        SettingsCommand::Everyone => {
            if !ctx.is_group {
                return Ok(format!("{PEOPLE} In a DM there's only you: see /settings."));
            }
            let members = stored::members(conn, id).await?;
            return Ok(format_everyone(&members, &prayer_times.defaults));
        }
        SettingsCommand::Methods => return Ok(format_methods()),
        SettingsCommand::Reset => Some(Change::Reset),
        SettingsCommand::City { city, country } => {
            // No country typed: keep the one already in use.
            let country = country.unwrap_or_else(|| current.country.clone());
            Some(Change::City { city, country })
        }
        SettingsCommand::Timezone(name) => Some(Change::Timezone(name)),
        SettingsCommand::PrayerMethod(method) => Some(Change::PrayerMethod(method)),
        SettingsCommand::PrayerSchool(school) => Some(Change::PrayerSchool(school)),
        SettingsCommand::PrayerEnding(minutes) => Some(Change::PrayerEnding(minutes)),
    };

    // `note` is an extra first line for the reply; most changes have none.
    let note = match change {
        None => None,
        Some(change) => {
            match settings_edit::apply(conn, apis, id, &current, &change).await? {
                Ok(Saved::Town { name, timezone }) => Some(format!(
                    "Found {name}. Timezone set to {timezone} to match."
                )),
                Ok(Saved::Done) if change == Change::Reset => {
                    Some("Back to the defaults.".to_string())
                }
                Ok(Saved::Done) => None,
                // Refused: the explanation is the whole reply.
                Err(refused) => return Ok(format_refused(refused)),
            }
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
    titles: &[String],
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

    // All in the webhook's one transaction: if one fails, none are added.
    for title in titles {
        todos::add(
            conn,
            ctx.chat_id,
            title,
            ctx.sender.id,
            Some(assignee.id),
            ctx.now,
        )
        .await?;
    }
    Ok(format_todos_added(titles, &assignee.display_name))
}

/// "📝 Added *fix the tap* for Bilal", or for several:
/// "📝 Added 2 todos for Bilal:" and one line each.
fn format_todos_added(titles: &[String], name: &str) -> String {
    match titles {
        [title] => format!("{ADDED} Added *{title}* for {name}"),
        _ => {
            let lines: Vec<String> = titles.iter().map(|t| format!("• {t}")).collect();
            format!(
                "{ADDED} Added {} todos for {name}:\n{}",
                titles.len(),
                lines.join("\n")
            )
        }
    }
}

/// Runs a reply to one of the bot's numbered lists. `items` is what that
/// list showed (never empty: the webhook only calls this for real lists).
pub async fn run_list_reply(
    conn: &mut SqliteConnection,
    ctx: &Context,
    items: &[ListItem],
    reply: ReplyCommand,
) -> Result<Outcome, AppError> {
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
    // Shopping items ticked off: reported together, mentioning the buyer.
    let mut bought = Vec::new();
    // Any todo completed: the reply ends with the updated progress bar.
    let mut completed_any = false;
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
        match (name, is_shop, removing) {
            (None, _, _) => lines.push(gone(n)),
            (Some(name), _, true) => lines.push(format!("{REMOVED} Removed *{name}*")),
            (Some(name), false, false) => {
                lines.push(format!("{DONE} Done: *{name}*"));
                completed_any = true;
            }
            (Some(name), true, false) => bought.push(name),
        }
    }
    if is_shop && !removing {
        return Ok(bought_reply(ctx, &bought, &lines));
    }
    if completed_any {
        let (done, total) = todos::progress(conn, ctx.chat_id).await?;
        lines.push(progress_bar(done, total));
    }
    Ok(Outcome::Reply(lines.join("\n")))
}

/// `+done gym`: completes the open todo whose title matches each name.
/// Several matches is a question back rather than a guess.
async fn done_by_name(
    conn: &mut SqliteConnection,
    ctx: &Context,
    names: &[String],
) -> Result<String, AppError> {
    let mut lines = Vec::new();
    let mut completed_any = false;
    for name in names {
        let matches = todos::find_open(conn, ctx.chat_id, name).await?;
        // A slice pattern: exactly one match, bound as `(id, title)`.
        match matches.as_slice() {
            [] => lines.push(format!("{PROBLEM} No open todo matches \"{name}\".")),
            [(id, title)] => {
                if todos::complete(conn, *id, ctx.chat_id, ctx.sender.id, ctx.now)
                    .await?
                    .is_some()
                {
                    lines.push(format!("{DONE} Done: *{title}*"));
                    completed_any = true;
                }
            }
            several => {
                let titles: Vec<&str> = several.iter().map(|(_, t)| t.as_str()).collect();
                lines.push(format!(
                    "{PROBLEM} \"{name}\" matches {}: {}. Say more of the title, or use \
                     the number from /todo (`+done 2`).",
                    titles.len(),
                    titles.join(", ")
                ));
            }
        }
    }
    if completed_any {
        let (done, total) = todos::progress(conn, ctx.chat_id).await?;
        lines.push(progress_bar(done, total));
    }
    Ok(lines.join("\n"))
}

/// "■■■■□□□□□□ 4/10 done" (the bar itself is `progress::bar`).
fn progress_bar(done: i64, total: i64) -> String {
    if total == 0 {
        return String::new();
    }
    format!("{} {done}/{total} done", progress::bar(done, total))
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
        Layout::ByPerson | Layout::All => ("*Todos*".to_string(), numbered),
    };

    let total = shown.len() as i64;
    let done = shown.iter().filter(|(_, t)| t.done.is_some()).count() as i64;
    let mut text = format!("{LIST} {title}\n{}\n", progress_bar(done, total));
    // How much of the sender's day is left to do them in.
    if let Some(day) = calendar::day_line(now, settings.timezone) {
        text.push_str(&day);
        text.push('\n');
    }

    let groups = match layout {
        Layout::ByPerson => group_by_person(&shown),
        // One group, with no heading.
        Layout::All | Layout::Only(_) => vec![(None, shown.clone())],
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

    if done < total {
        text.push_str(
            "\nFinish one: reply to this message with `done 2`, or send `+done 2` or \
             `+done gym` any time.",
        );
    } else {
        text.push_str("\nAll done! 🎉");
    }
    if done > 0 {
        text.push_str(" `/todo clear done` removes the ✅ ones; `/todo clear` empties the list.");
    }
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

/// "⬜ 4. fix the tap", or "✅ 2. ~bins~ · Bilal 14:05" when done. The
/// "Bilal" (who did it) is left out when it was the assignee themselves,
/// since they're already named. `show_assignee` adds " (Khadija)".
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
    let Some(done) = &todo.done else {
        return format!("{NOT_YET} {number}. {}{assignee}", todo.title);
    };
    // "14:05" if it was today (their today), else "Tue 14:05".
    let when = if settings.today(done.at) == settings.today(now) {
        settings.clock(done.at)
    } else {
        let local = done.at.with_timezone(&settings.timezone);
        local.format("%a %H:%M").to_string()
    };
    let by_themselves = !show_assignee && is_for(todo, &done.by);
    let by = if by_themselves {
        String::new()
    } else {
        format!("{} ", done.by)
    };
    format!("{DONE} {number}. ~{}~{assignee} · {by}{when}", todo.title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::parser::Checkin;
    use crate::person::Person;
    use crate::prayer::{HijriDate, Window};
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
        ListItem {
            position,
            item_type: TODO_ITEM.to_string(),
            item_id,
        }
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
                done: None,
            },
            ListedTodo {
                id: 3,
                title: "fix the tap".to_string(),
                assignee: None,
                done: Some(todos::Completion {
                    by: "Khadija".to_string(),
                    at: at("2026-10-28T14:05:00Z"),
                }),
            },
            ListedTodo {
                id: 4,
                title: "bins".to_string(),
                assignee: None,
                done: Some(todos::Completion {
                    by: "Bilal".to_string(),
                    at: at("2026-10-27T08:30:00Z"),
                }),
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
            done: done_by.map(|by| todos::Completion {
                by: by.to_string(),
                at: at("2026-10-28T14:05:00Z"),
            }),
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
        assert_eq!(progress_bar(0, 4), "□□□□□□□□□□ 0/4 done");
        assert_eq!(progress_bar(1, 3), "■■■□□□□□□□ 1/3 done");
        assert_eq!(progress_bar(4, 4), "■■■■■■■■■■ 4/4 done");
        assert_eq!(progress_bar(0, 0), "");
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
            titles: vec![title.to_string()],
            assignee,
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
        let one = format_todos_added(&["go gym".to_string()], "Bilal");
        assert_eq!(one, "📝 Added *go gym* for Bilal");
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
        assert_eq!(reply, "✅ Done: *go gym*\n■■■□□□□□□□ 1/3 done");

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
        let items: Vec<ListItem> = item_ids
            .iter()
            .zip(1..)
            .map(|(&id, pos)| item(pos, id))
            .collect();

        let reply = run_list_reply(&mut conn, &ctx, &items, ReplyCommand::Tick(vec![1, 3]))
            .await
            .unwrap()
            .into_text();
        assert_eq!(
            reply,
            "✅ Done: *go gym*\n✅ Done: *book dentist*\n■■■■■■■□□□ 2/3 done"
        );

        // The numbers still refer to the old list: 2 is "fix the tap".
        let reply = run_list_reply(&mut conn, &ctx, &items, ReplyCommand::Remove(2))
            .await
            .unwrap()
            .into_text();
        assert_eq!(reply, "🗑️ Removed *fix the tap*");

        let reply = run_list_reply(&mut conn, &ctx, &items, ReplyCommand::Done(1))
            .await
            .unwrap()
            .into_text();
        assert!(reply.contains("already ticked off"), "{reply}");

        let reply = run_list_reply(&mut conn, &ctx, &items, ReplyCommand::Done(5))
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

    /// Runs `/shop` and returns (text, the ListItems a reply would see).
    async fn shop_list(conn: &mut SqliteConnection, ctx: &Context) -> (String, Vec<ListItem>) {
        let Outcome::List {
            text,
            item_type,
            item_ids,
        } = run_command(conn, ctx, &Apis::new(times()), BotCommand::ListShop)
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
        assert_eq!(items.len(), 2);
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

        let reply = run_list_reply(&mut conn, &ctx, &items, ReplyCommand::Tick(vec![1, 3]))
            .await
            .unwrap()
            .into_text();
        assert_eq!(reply, "✅ @447700900001 bought *milk*, *bread*");

        let reply = run_list_reply(&mut conn, &ctx, &items, ReplyCommand::Remove(4))
            .await
            .unwrap()
            .into_text();
        assert_eq!(reply, "🗑️ Removed *rice*");

        let reply = run_list_reply(&mut conn, &ctx, &items, ReplyCommand::Done(1))
            .await
            .unwrap()
            .into_text();
        assert!(reply.contains("already ticked off"), "{reply}");

        // Eggs still to buy first, then milk and bread, bought.
        let (text, items) = shop_list(&mut conn, &ctx).await;
        assert_eq!(items.len(), 3);
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
        assert!(left.is_empty());

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
        assert!(theirs.is_empty());

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
        let (_, items) = shop_list(&mut conn, &ctx).await;
        let ids: Vec<i64> = items.iter().map(|i| i.item_id).collect();
        messages::save_list(&mut conn, "3EB0LIST", ctx.chat_id, SHOP_ITEM, &ids, ctx.now)
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
}

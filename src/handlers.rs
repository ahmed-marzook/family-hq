//! What the bot does for each command: database changes, then the text to
//! send back. No GOWA here; the webhook does the sending.

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;

use crate::aladhan::PrayerTimes;
use crate::error::AppError;
use crate::parser::{BotCommand, ReplyCommand};
use crate::person::Assignee;
use crate::prayer::{self, DayTimes, Prayer};
use crate::repo::messages::ListItem;
use crate::repo::prayers;
use crate::repo::shop::{self, ShopItem};
use crate::repo::todos::{self, OpenTodo};
use crate::repo::users::{self, User};

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

/// `item_type` values in `wa_message_context` (the schema's CHECK allows
/// exactly these two).
pub const TODO_ITEM: &str = "todo";
pub const SHOP_ITEM: &str = "shop";

const HELP: &str = "🤖 *Family HQ*
+shop milk, eggs — add to this chat's shopping list
/shop — what's left to buy; reply `1 3` to tick items off
/shop clear — delete the ticked-off items
+todo fix the tap — add a todo (assigned to you)
+todo @Name book dentist — assign it to someone here
/todo — numbered list of open todos
Reply to a list with `done 2` (or `1 3`) or `remove 2`
/prayers — today's prayer times and what you've prayed
/prayers on — prayer reminders in this chat (/prayers off to stop)
+prayed asr — mark a prayer as prayed (or react to its reminder)
/help — this message";

/// Who is asking, and where.
pub struct Context {
    /// `chats.id` of the chat the message came from.
    pub chat_id: i64,
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
/// `prayer_times` is only used by `/prayers`. It answers from its cache
/// almost always; at most once a day it calls AlAdhan first.
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
        BotCommand::ClearShop => match shop::clear_bought(conn, ctx.chat_id).await? {
            0 => format!("{SHOP} Nothing bought to clear."),
            1 => format!("{CLEARED} Cleared 1 bought item."),
            n => format!("{CLEARED} Cleared {n} bought items."),
        },
        BotCommand::Prayers => {
            let day = prayer_times.for_date(prayer::london_date(ctx.now)).await?;
            let prayed = prayers::prayed_on(conn, ctx.sender.id, day.date).await?;
            format_prayer_day(&day, &prayer_times.location.city, &prayed)
        }
        BotCommand::Prayed(names) => {
            // For people who didn't catch the reminder: same effect as
            // reacting to it, for today.
            let today = prayer::london_date(ctx.now);
            for prayer in names {
                prayers::mark_prayed(conn, ctx.sender.id, today, prayer, ctx.now).await?;
            }
            let day = prayer_times.for_date(today).await?;
            let prayed = prayers::prayed_on(conn, ctx.sender.id, today).await?;
            format_prayer_day(&day, &prayer_times.location.city, &prayed)
        }
        BotCommand::PrayerReminders(on) => {
            prayers::set_reminders(conn, ctx.chat_id, on).await?;
            if on {
                format!(
                    "{BELL} Prayer reminders are on in this chat. \
                     React to a reminder to mark that prayer as prayed."
                )
            } else {
                format!("{BELL_OFF} Prayer reminders are off in this chat.")
            }
        }
        BotCommand::Help => HELP.to_string(),
    };
    Ok(Outcome::Reply(text))
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
    text.push_str("\nReply to this message with the numbers you bought, e.g. `1 3`.");
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

/// The `/prayers` reply: each time on the London clock, ticked if the
/// sender has prayed it.
fn format_prayer_day(day: &DayTimes, city: &str, prayed: &[Prayer]) -> String {
    let mut text = format!(
        "{MOSQUE} *Prayer times* ({city}, {})\n",
        day.date.format("%a %-d %b")
    );
    for (prayer, start) in &day.times {
        let mark = if prayed.contains(prayer) {
            DONE
        } else {
            NOT_YET
        };
        text.push_str(&format!(
            "{mark} {prayer} {}\n",
            prayer::london_hhmm(*start)
        ));
    }
    text.push_str("\nTick one off with `+prayed asr`, or react to its reminder.");
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
    use crate::aladhan::Location;
    use crate::db::test_pool;
    use crate::person::Person;
    use crate::repo::chats;
    use chrono::NaiveDate;

    /// 27 Oct 2026 in Milton Keynes (GMT, so London time = UTC).
    fn day() -> DayTimes {
        let at = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        DayTimes {
            date: NaiveDate::from_ymd_opt(2026, 10, 27).unwrap(),
            times: [
                (Prayer::Fajr, at("2026-10-27T04:55:00Z")),
                (Prayer::Dhuhr, at("2026-10-27T11:47:00Z")),
                (Prayer::Asr, at("2026-10-27T14:16:00Z")),
                (Prayer::Maghrib, at("2026-10-27T16:44:00Z")),
                (Prayer::Isha, at("2026-10-27T18:31:00Z")),
            ],
        }
    }

    /// Prayer times that come from memory, never the network.
    fn times() -> PrayerTimes {
        let location = Location {
            city: "Milton Keynes".to_string(),
            country: "GB".to_string(),
            method: 3,
            school: 0,
        };
        PrayerTimes::with_cached(location, day())
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
        let text = format_prayer_day(&day(), "Milton Keynes", &[Prayer::Fajr, Prayer::Asr]);
        assert!(
            text.starts_with("🕌 *Prayer times* (Milton Keynes, Tue 27 Oct)\n"),
            "{text}"
        );
        assert!(text.contains("✅ Fajr 04:55\n"));
        assert!(text.contains("⬜ Dhuhr 11:47\n"));
        assert!(text.contains("✅ Asr 14:16\n"));
        assert!(text.contains("⬜ Isha 18:31\n"));
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
}

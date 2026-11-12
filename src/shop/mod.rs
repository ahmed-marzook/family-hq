//! The shopping list: `+shop`, `/shop`, `+got` (by name or number) and
//! `/shop clear`. `bot` passes each one here. Replies to a `/shop` list
//! (`1 3`, `remove 2`) go through `bot::lists`, which uses `bought_reply`
//! from here.

use sqlx::SqliteConnection;

use crate::bot::emoji::{CLEARED, DONE, NOT_YET, PROBLEM};
use crate::bot::lists::run_list_reply;
use crate::bot::{Context, Outcome};
use crate::error::AppError;
use crate::parser::ReplyCommand;
use crate::repo::messages::{self, ListKind};
use crate::repo::shop::{self as stored, ShopItem};
use crate::settings::Settings;

const SHOP: &str = "🛒";

/// `/shop`: everything on the list, numbered so a reply can tick items off.
pub async fn list(conn: &mut SqliteConnection, ctx: &Context) -> Result<Outcome, AppError> {
    let items = stored::list_all(conn, ctx.chat_id).await?;
    Ok(Outcome::List {
        text: format_shop_list(&items),
        kind: ListKind::Shop,
        item_ids: items.iter().map(|i| i.id).collect(),
    })
}

/// `+got 1 3`: ticks items off by their numbers in the newest `/shop` list.
pub async fn got_by_numbers(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    numbers: Vec<usize>,
) -> Result<Outcome, AppError> {
    // Same as replying `1 3` to the newest `/shop` list here.
    match messages::latest_list_items(conn, ctx.chat_id, ListKind::Shop).await? {
        None => Ok(Outcome::Reply(format!(
            "{PROBLEM} No shopping list sent here yet. Send /shop, then `+got 1 3`."
        ))),
        Some(list) => run_list_reply(conn, ctx, defaults, &list, ReplyCommand::Tick(numbers)).await,
    }
}

/// `/shop clear` empties the list; `/shop clear bought` takes off only
/// what's been bought.
pub async fn clear(
    conn: &mut SqliteConnection,
    ctx: &Context,
    bought_only: bool,
) -> Result<String, AppError> {
    let text = if bought_only {
        match stored::clear_bought(conn, ctx.chat_id).await? {
            0 => format!("{SHOP} Nothing bought to clear."),
            1 => format!("{CLEARED} Cleared 1 bought item."),
            n => format!("{CLEARED} Cleared {n} bought items."),
        }
    } else {
        match stored::clear_all(conn, ctx.chat_id).await? {
            0 => format!("{SHOP} The shopping list is already empty."),
            1 => format!("{CLEARED} Cleared the shopping list (1 item)."),
            n => format!("{CLEARED} Cleared the shopping list ({n} items)."),
        }
    };
    Ok(text)
}

/// Adds each item that isn't already on the list (ignoring case), and says
/// which were added and which were already there.
pub async fn add(
    conn: &mut SqliteConnection,
    ctx: &Context,
    names: &[String],
) -> Result<String, AppError> {
    let mut added: Vec<&str> = Vec::new();
    let mut already: Vec<&str> = Vec::new();
    for name in names {
        // Also catches the same item twice in one message ("milk, Milk"):
        // the first one is on the list by the time the second is checked.
        if stored::is_on_list(conn, ctx.chat_id, name).await? {
            already.push(name);
        } else {
            stored::add(conn, ctx.chat_id, name, ctx.sender.id, ctx.now).await?;
            added.push(name);
        }
    }
    Ok(format_shop_added(&added, &already))
}

/// `+got milk, eggs`: ticks each named item off.
pub async fn got_by_name(
    conn: &mut SqliteConnection,
    ctx: &Context,
    names: &[String],
) -> Result<Outcome, AppError> {
    let mut bought = Vec::new();
    let mut problems = Vec::new();
    for name in names {
        let ticked = match stored::find_to_buy(conn, ctx.chat_id, name).await? {
            Some(id) => stored::mark_bought(conn, id, ctx.chat_id, ctx.sender.id, ctx.now).await?,
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
pub fn bought_reply(ctx: &Context, bought: &[String], problems: &[String]) -> Outcome {
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::Apis;
    use crate::bot::run_command;
    use crate::bot::tests::{sent, setup, times};
    use crate::db::test_pool;
    use crate::parser::{BotCommand, Got};
    use crate::repo::chats;
    use crate::repo::messages::SentList;
    use crate::settings::tests::defaults;

    pub(crate) fn shop_cmd(items: &[&str]) -> BotCommand {
        BotCommand::AddShop(items.iter().map(|i| i.to_string()).collect())
    }

    /// Runs `/shop` and returns (text, the list a reply would see).
    pub(crate) async fn shop_list(
        conn: &mut SqliteConnection,
        ctx: &Context,
    ) -> (String, SentList) {
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
}

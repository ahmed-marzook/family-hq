//! Replies to the bot's numbered lists (`done 2`, `1 3`, `remove 2`,
//! `skip 2`, `due 2 fri`), which work the same on a todo list and on the
//! shopping list, so they live here rather than in either feature. Each
//! item's own change is the feature's: `todo` closes and changes todos,
//! `shop` words what was bought.
//!
//! What a list was (`ListKind`, `SentList`) is kept in `repo::messages`,
//! next to the table it's saved in.

use sqlx::SqliteConnection;

use super::emoji::{DONE, PROBLEM, SKIPPED};
use super::{Context, Outcome, my_settings};
use crate::error::AppError;
use crate::parser::ReplyCommand;
use crate::points::{self, Finish};
use crate::repo::messages::{ListItem, ListKind, SentList};
use crate::repo::shop;
use crate::repo::todos;
use crate::settings::Settings;
use crate::shop::bought_reply;
use crate::todo::format::progress_bar;
use crate::todo::{self, TodoChange, close_todo};

const REMOVED: &str = "🗑️";

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
            return todo::change(conn, ctx, defaults, list, n, TodoChange::Due(due)).await;
        }
        ReplyCommand::SetPriority(n, priority) => {
            return todo::change(conn, ctx, defaults, list, n, TodoChange::Priority(priority))
                .await;
        }
        ReplyCommand::BadChange(err) => return Ok(Outcome::Reply(format!("{PROBLEM} {err}"))),
    };
    act_on_list(conn, ctx, defaults, list, &numbers, action).await
}

/// What a reply (or `+done` / `+skip`) does to the items it numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Tick off: a todo done, a shopping item bought.
    Tick,
    Remove,
    /// Todos only: closed as "won't be done".
    Skip,
}

/// `impl From<A> for B` is the standard way to say "an A can always be
/// turned into a B": here `Action::from(Finish::Done)` is `Action::Tick`.
impl From<Finish> for Action {
    fn from(finish: Finish) -> Self {
        match finish {
            Finish::Done => Action::Tick,
            Finish::Skipped => Action::Skip,
        }
    }
}

/// What happens to one item of a list: the list's kind and the `Action`
/// together. There's no "skip a shopping item" here at all, so the code
/// that works through the items never has to handle one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListStep {
    /// A todo done or skipped, which earns or costs points.
    CloseTodo(Finish),
    /// A todo removed: it neither earns nor costs anything.
    RemoveTodo,
    /// A shopping item ticked off.
    Buy,
    RemoveShop,
}

/// Does `action` to the items at `numbers` in `list`.
pub async fn act_on_list(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    list: &SentList,
    numbers: &[usize],
    action: Action,
) -> Result<Outcome, AppError> {
    // Every pair of list kind and action, so the compiler checks none is
    // missed. Skipping a shopping item is the one that makes no sense.
    let step = match (list.kind, action) {
        (ListKind::Todo, Action::Tick) => ListStep::CloseTodo(Finish::Done),
        (ListKind::Todo, Action::Skip) => ListStep::CloseTodo(Finish::Skipped),
        (ListKind::Todo, Action::Remove) => ListStep::RemoveTodo,
        (ListKind::Shop, Action::Tick) => ListStep::Buy,
        (ListKind::Shop, Action::Remove) => ListStep::RemoveShop,
        (ListKind::Shop, Action::Skip) => {
            return Ok(Outcome::Reply(format!(
                "{PROBLEM} `skip` is for todos. To take something off the shopping \
                 list, reply `remove 2`."
            )));
        }
    };

    // The closer's own today, for "done by its due date?". Only todos earn
    // points, so a shopping list doesn't need to look it up.
    let today = if list.kind == ListKind::Shop {
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
        // The repo call that matches the step. Each returns the item's
        // name, or `None` if it's already gone.
        let (chat, by, now) = (ctx.chat_id, ctx.sender.id, ctx.now);
        // The points a closed todo earned, for its line.
        let mut earned = None;
        let name = match step {
            ListStep::CloseTodo(finish) => {
                // `map` with a closure that also sets `earned`: the title
                // goes on to the `match` below, the points to the line.
                close_todo(conn, ctx, id, finish, today)
                    .await?
                    .map(|(title, points)| {
                        earned = Some(points);
                        title
                    })
            }
            ListStep::RemoveTodo => todos::remove(conn, id, chat).await?,
            ListStep::Buy => shop::mark_bought(conn, id, chat, by, now).await?,
            ListStep::RemoveShop => shop::remove(conn, id, chat).await?,
        };
        match (name, step) {
            (None, _) => lines.push(gone(n)),
            (Some(name), ListStep::RemoveTodo | ListStep::RemoveShop) => {
                lines.push(format!("{REMOVED} Removed *{name}*"));
            }
            (Some(name), ListStep::CloseTodo(Finish::Done)) => {
                lines.push(format!(
                    "{DONE} Done: *{name}*{}",
                    points::earned_text(earned)
                ));
                closed_any = true;
            }
            (Some(name), ListStep::CloseTodo(Finish::Skipped)) => {
                lines.push(format!(
                    "{SKIPPED} Skipped: *{name}*{}",
                    points::earned_text(earned)
                ));
                closed_any = true;
            }
            (Some(name), ListStep::Buy) => bought.push(name),
        }
    }
    if step == ListStep::Buy {
        return Ok(bought_reply(ctx, &bought, &lines));
    }
    if closed_any {
        let p = todos::progress(conn, ctx.chat_id).await?;
        lines.push(progress_bar(p.done, p.skipped, p.total));
    }
    Ok(Outcome::Reply(lines.join("\n")))
}

/// The row id at list number `n`, or the message to send if there's no
/// such number.
pub fn item_at(items: &[ListItem], n: usize) -> Result<i64, String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Apis;
    use crate::bot::run_command;
    use crate::bot::tests::{item, sent, setup, times, typed};
    use crate::db::test_pool;
    use crate::repo::messages;
    use crate::settings::tests::defaults;
    use crate::shop::tests::{shop_cmd, shop_list};

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
}

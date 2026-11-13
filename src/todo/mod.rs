//! Todos: `+todo`, `/todo` (and its views), `+done` / `+skip` by number or
//! name, `/todo clear` and `/todo reminders`. `bot` passes each one here.
//! Replies to a `/todo` list (`done 2`, `due 2 fri`) go through
//! `bot::lists`, which uses `close_todo` and `change` from here.
//!
//! `format` words the replies, `flags` reads the `-d fri` / `-p high`
//! flags, and `reminders` sends the reminders through the day.

use chrono::NaiveDate;
use sqlx::SqliteConnection;

use crate::bot::emoji::{CLEARED, DONE, PROBLEM, SKIPPED};
use crate::bot::lists::{Action, act_on_list, item_at};
use crate::bot::{Context, Outcome, my_settings};
use crate::error::AppError;
use crate::parser::{TodoView, Toggle};
use crate::person::Assignee;
use crate::points::{self, Finish, Source};
use crate::repo::messages::{self, ListKind, SentList};
use crate::repo::settings as stored_settings;
use crate::repo::todo_reminders;
use crate::repo::todos::{self, Details};
use crate::repo::users;
use crate::settings::Settings;
use flags::{Due, Priority};
use format::{Layout, format_todo_list, format_todo_reminders, format_todos_added, progress_bar};

pub mod flags;
pub mod format;
pub mod reminders;

const LIST: &str = "📋";
const DUE: &str = "📅";

/// `/todo` and its views: the whole list, one person's, or a filtered
/// slice, numbered so a reply can act on it.
pub async fn list(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    view: TodoView,
) -> Result<Outcome, AppError> {
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
    Ok(Outcome::List {
        text: format_todo_list(&items, &layout, &settings, ctx.now),
        kind: ListKind::Todo,
        // Every todo, even in one person's view, so the numbers
        // mean the same todo in every view.
        item_ids: items.iter().map(|t| t.id).collect(),
    })
}

/// `/todo clear` empties the list; `/todo clear done` takes off only the
/// done and skipped ones.
///
/// Clearing a todo that's still open counts as skipping it, so clearing
/// the list can't be used to dodge a skip's −1. It costs the person it was
/// for (its assignee, or whoever added it), not whoever cleared it, on
/// their own day. Done and skipped todos have earned theirs already, so
/// `/todo clear done` costs nothing.
pub async fn clear(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    done_only: bool,
) -> Result<String, AppError> {
    if !done_only {
        let open = charge_open_todos(conn, ctx, defaults).await?;
        let reply = match todos::clear_all(conn, ctx.chat_id).await? {
            0 => format!("{LIST} There are no todos to clear."),
            1 => format!("{CLEARED} Cleared the todo list (1 todo)."),
            n => format!("{CLEARED} Cleared the todo list ({n} todos)."),
        };
        return Ok(match open {
            Some(open) => format!("{reply}\n{open}"),
            None => reply,
        });
    }
    // Counted first, to say how many of each went.
    let skipped = todos::progress(conn, ctx.chat_id).await?.skipped;
    Ok(
        match (todos::clear_done(conn, ctx.chat_id).await?, skipped) {
            (0, _) => format!("{LIST} No completed todos to clear."),
            (1, 0) => format!("{CLEARED} Cleared 1 completed todo."),
            (n, 0) => format!("{CLEARED} Cleared {n} completed todos."),
            (n, skipped) => format!(
                "{CLEARED} Cleared {} completed and {skipped} skipped.",
                n - skipped as u64
            ),
        },
    )
}

/// Before `/todo clear` deletes them: records a skip for each open todo,
/// against the person it was for. Returns the line saying so, or `None` if
/// nothing was open.
async fn charge_open_todos(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
) -> Result<Option<String>, AppError> {
    let open = todos::open_with_owner(conn, ctx.chat_id).await?;
    // Each person's total, in the order they first appear.
    let mut losses: Vec<(String, i64)> = Vec::new();
    for todo in &open {
        let theirs = stored_settings::effective(conn, defaults, ctx.chat_id, Some(todo.owner_id));
        let today = theirs.await?.today(ctx.now);
        // A skip's rule: priority and due date don't change it.
        let award = points::for_todo(Finish::Skipped, None, None, today);
        let points = points::award::award_to(
            conn,
            ctx,
            todo.owner_id,
            Source::Todo,
            todo.id,
            award,
            today,
        )
        .await?;
        match losses.iter_mut().find(|(name, _)| *name == todo.owner_name) {
            Some((_, total)) => *total += points,
            None => losses.push((todo.owner_name.clone(), points)),
        }
    }
    Ok(points::cleared_text(open.len(), &losses))
}

/// `/todo reminders [on|off]`: the sender's own todo reminders in this
/// chat, turned on or off first if asked.
pub async fn reminders(
    conn: &mut SqliteConnection,
    ctx: &Context,
    toggle: Toggle,
) -> Result<String, AppError> {
    let (chat_id, user_id) = (ctx.chat_id, ctx.sender.id);
    match toggle {
        Toggle::On => todo_reminders::enable(conn, chat_id, user_id, ctx.now).await?,
        Toggle::Off => todo_reminders::disable(conn, chat_id, user_id).await?,
        Toggle::Show => {}
    }
    let is_on = todo_reminders::is_on(conn, chat_id, user_id).await?;
    Ok(format_todo_reminders(is_on, ctx.is_group))
}

pub async fn add(
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
    // One `match` covers "no date", "a date" and "a bad date".
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

/// A change to one todo, from a reply (`due 2 fri`, `priority 2 none`).
#[derive(Debug, Clone, Copy)]
pub enum TodoChange {
    Due(Option<Due>),
    Priority(Option<Priority>),
}

/// `due 2 fri` / `priority 2 high` (or `none`): changes todo 2 in the list
/// that was replied to.
pub async fn change(
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
                    flags::show_date(date, today)
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

/// The `+` command that closes a todo as `finish` without replying.
fn close_command(finish: Finish) -> &'static str {
    match finish {
        Finish::Done => "+done",
        Finish::Skipped => "+skip",
    }
}

/// `+done 2` / `+skip 1 3`: the same as replying to the newest `/todo`
/// list sent in this chat.
pub async fn close_by_numbers(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    numbers: &[usize],
    finish: Finish,
) -> Result<Outcome, AppError> {
    let Some(list) = messages::latest_list_items(conn, ctx.chat_id, ListKind::Todo).await? else {
        let command = close_command(finish);
        return Ok(Outcome::Reply(format!(
            "{PROBLEM} No todo list sent here yet. Send /todo, then `{command} 2`."
        )));
    };
    act_on_list(conn, ctx, defaults, &list, numbers, Action::from(finish)).await
}

/// Closes todo `id` in this chat as `finish` (done or skipped) and records
/// the points that earns the sender (`points::for_todo`), counted on their
/// own `today`. Returns its title and the points, or `None` if it was
/// already closed or gone (and so earns nothing).
///
/// No transaction of its own: the webhook runs each command in one, so the
/// todo and its points are saved together or not at all.
pub async fn close_todo(
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
    let Some(todo) = closed else {
        return Ok(None);
    };
    // What it's worth is the todo's own rule; saving it is shared.
    let award = points::for_todo(finish, todo.priority, todo.due, today);
    let earned = points::award::award(conn, ctx, Source::Todo, id, award, today).await?;
    Ok(Some((todo.title, earned)))
}

/// `+done gym` / `+skip gym`: closes the open todo whose title matches each
/// name. Several matches is a question back rather than a guess.
pub async fn close_by_name(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    names: &[String],
    finish: Finish,
) -> Result<String, AppError> {
    // The closer's own today, for "done by its due date?".
    let today = my_settings(conn, ctx, defaults).await?.today(ctx.now);
    let mut lines = Vec::new();
    let mut closed_any = false;
    for name in names {
        let matches = todos::find_open(conn, ctx.chat_id, name).await?;
        match matches.as_slice() {
            [] => lines.push(format!("{PROBLEM} No open todo matches \"{name}\".")),
            [(id, title)] => {
                if let Some((_, points)) = close_todo(conn, ctx, *id, finish, today).await? {
                    let mark = match finish {
                        Finish::Done => format!("{DONE} Done"),
                        Finish::Skipped => format!("{SKIPPED} Skipped"),
                    };
                    let earned = points::earned_text(Some(points));
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
                    close_command(finish)
                ));
            }
        }
    }
    if closed_any {
        let p = todos::progress(conn, ctx.chat_id).await?;
        lines.push(progress_bar(p.done, p.skipped, p.total));
        lines.extend(points::level_ups::for_sender(conn, ctx, today).await?);
    }
    Ok(lines.join("\n"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Apis;
    use crate::bot::lists::run_list_reply;
    use crate::bot::run_command;
    use crate::bot::tests::{sent, setup, times, typed};
    use crate::db::test_pool;
    use crate::parser::{BotCommand, Got, ReplyCommand};
    use crate::person::Person;
    use crate::repo::chats;
    use crate::settings::tests::defaults;

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
        let mut run = async |toggle| {
            run_command(&mut conn, &ctx, &apis, BotCommand::TodoReminders(toggle))
                .await
                .unwrap()
                .into_text()
        };

        assert!(
            run(Toggle::Show).await.contains("are off for you"),
            "off by default"
        );
        assert!(
            run(Toggle::On)
                .await
                .contains("are on for you in this group")
        );
        assert!(run(Toggle::Show).await.contains("are on for you"));
        assert!(run(Toggle::Off).await.contains("are off for you"));
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
             🥇 Bilal · Lv 2 · 4\n\
             🥈 Khadija · Lv 2 · 1\n\
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
        // His skip's −1 lowers the week (4) but not his XP (5).
        assert!(
            mine.starts_with(
                "📊 *Your stats* · Overall Lv 2\n\
                 🕌 Prayers Lv 1 □□□□□□□□□□ 0 / 35\n\
                 📝 Todos Lv 1 ■■■□□□□□□□ 5 / 15\n\
                 \n\
                 Today: 0 · This week: 4 · Last week: 0"
            ),
            "{mine}"
        );
        assert!(mine.contains("Fri 6 ■■■■■■■■■■ 4"), "{mine}");
    }

    /// Pins what closing a todo saves in the points ledger: one row per
    /// todo, for whoever closed it, with its reason, amount and the
    /// closer's own day. Closing it again, or removing a todo, adds none.
    #[tokio::test]
    async fn closing_a_todo_saves_one_award_with_its_reason() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        // Friday 6 November, lunchtime in London.
        ctx.now = "2026-11-06T12:00:00Z".parse().unwrap();
        typed(&mut conn, &ctx, "+todo report -d today -p high").await;
        typed(&mut conn, &ctx, "+todo bins -d today").await;
        typed(&mut conn, &ctx, "+todo gym, hoover, tap").await;

        typed(&mut conn, &ctx, "+done report").await;
        typed(&mut conn, &ctx, "+done tap").await;
        // Saturday: bins is late, gym is skipped.
        ctx.now = "2026-11-07T10:00:00Z".parse().unwrap();
        typed(&mut conn, &ctx, "+done bins").await;
        typed(&mut conn, &ctx, "+skip gym").await;
        // Already closed (done, then skipped): nothing more is saved.
        typed(&mut conn, &ctx, "+done report").await;
        typed(&mut conn, &ctx, "+skip gym").await;
        // Removed: neither earns nor costs.
        let Outcome::List { item_ids, .. } = typed(&mut conn, &ctx, "/todo all").await else {
            panic!("expected a list");
        };
        let open = todos::list_open(&mut conn, ctx.chat_id).await.unwrap();
        assert_eq!(open.len(), 1, "only hoover is open");
        let n = item_ids.iter().position(|&id| id == open[0].id).unwrap() + 1;
        let items = sent(ListKind::Todo, &item_ids);
        run_list_reply(
            &mut conn,
            &ctx,
            &defaults(),
            &items,
            ReplyCommand::Remove(n),
        )
        .await
        .unwrap();

        // A runtime query (not `query!`), so this test needs no `.sqlx` file.
        let rows: Vec<(i64, String, String, i64, String)> =
            sqlx::query_as("SELECT user_id, source, reason, points, day FROM points ORDER BY id")
                .fetch_all(&mut *conn)
                .await
                .unwrap();
        let me = ctx.sender.id;
        let row = |reason: &str, points, day: &str| {
            let (source, reason, day) = ("todo".to_string(), reason.to_string(), day.to_string());
            (me, source, reason, points, day)
        };
        assert_eq!(
            rows,
            [
                row("done_on_time", 4, "2026-11-06"),
                row("done", 2, "2026-11-06"),
                row("done_late", 1, "2026-11-07"),
                row("skipped", -1, "2026-11-07"),
            ]
        );
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
        assert_eq!(
            reply,
            "🧹 Cleared the todo list (1 todo).\n\
             1 was still open, so it counts as skipped: Bilal −1 ⭐"
        );
    }

    #[tokio::test]
    async fn clearing_open_todos_costs_whoever_they_were_for() {
        use crate::repo::points as stored_points;
        use crate::reports::Period;

        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        // Bilal adds one for himself, one for Khadija and one he then does.
        typed(&mut conn, &ctx, "+todo fix the tap").await;
        typed(&mut conn, &ctx, "+todo @Khadija call gran").await;
        typed(&mut conn, &ctx, "+todo go gym").await;
        typed(&mut conn, &ctx, "+done gym").await;
        // Khadija adds one with nobody named: hers.
        let khadija = users::find_member_by_name(&mut conn, ctx.chat_id, "Khadija")
            .await
            .unwrap()
            .unwrap();
        let hers = Context {
            chat_id: ctx.chat_id,
            is_group: true,
            sender: khadija,
            now: ctx.now,
        };
        typed(&mut conn, &hers, "+todo book dentist").await;

        // Clearing done ones first costs nothing.
        typed(&mut conn, &hers, "/todo clear done").await;
        // Khadija clears the list: the two open todos that were hers cost
        // her, and Bilal's open one costs him, though she cleared it.
        let reply = typed(&mut conn, &hers, "/todo clear").await.into_text();
        assert_eq!(
            reply,
            "🧹 Cleared the todo list (3 todos).\n\
             3 were still open, so each counts as skipped: Bilal −1 · Khadija −2 ⭐"
        );

        let today = ctx.now.date_naive();
        let around = Period {
            first: today - chrono::Duration::days(1),
            last: today + chrono::Duration::days(1),
        };
        let mut totals = stored_points::totals(&mut conn, ctx.chat_id, around)
            .await
            .unwrap();
        totals.sort();
        // Bilal: +2 for the gym, −1 for the tap.
        assert_eq!(
            totals,
            vec![("Bilal".to_string(), 1), ("Khadija".to_string(), -2)]
        );

        // An empty list: nothing more to charge.
        let reply = typed(&mut conn, &hers, "/todo clear").await.into_text();
        assert_eq!(reply, "📋 There are no todos to clear.");
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
}

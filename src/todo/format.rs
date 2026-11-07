//! The text of the todo replies: the `/todo` list in each of its layouts,
//! "Added ..." after `+todo`, the progress bar, due dates and the
//! `/todo reminders` reply. Pure functions: the todos come in, the text
//! goes out.

use chrono::{DateTime, NaiveDate, Utc};

use super::flags::{self, Priority};
use super::{DUE, LIST};
use crate::bot::emoji::{BELL, BELL_OFF, DONE, NOT_YET, SKIPPED};
use crate::calendar;
use crate::parser::TodoFilter;
use crate::progress;
use crate::repo::todos::{Details, ListedTodo};
use crate::settings::Settings;

const ADDED: &str = "📝";
const OVERDUE: &str = "⚠️";

/// "📝 Added *fix the tap* for Bilal", or for several:
/// "📝 Added 2 todos for Bilal:" and one line each. Then the due date and
/// priority, if given, on a line of their own (they're for all of them).
pub fn format_todos_added(
    titles: &[String],
    name: &str,
    details: Details,
    today: NaiveDate,
) -> String {
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
        .map(|date| format!("{DUE} Due {}", flags::show_date(date, today)));
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

/// "■■■■□□□□□□ 4/10 done" (the bar itself is `progress::bar`), and
/// "· 1 skipped" after it if any were. `total` counts the skipped ones,
/// but the bar leaves them out: a todo that won't be done isn't one still
/// to do.
pub fn progress_bar(done: i64, skipped: i64, total: i64) -> String {
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

/// The reply to `/todo reminders`.
pub fn format_todo_reminders(on: bool, is_group: bool) -> String {
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
pub enum Layout {
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
pub fn format_todo_list(
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
        let day = flags::show_date(due, today);
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
    use crate::repo::todos;
    use crate::settings::tests::defaults;

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
}

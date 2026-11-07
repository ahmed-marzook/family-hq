//! Todo reminders: if someone has asked for them (`/todo reminders on`) and
//! has open todos, a message four times a day on their own clock.
//!
//! `due_slot` and `format` are pure; `check` does the database and sending,
//! called by the reminder loop in `reminders.rs` every minute.

use chrono::{DateTime, Duration, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use tracing::warn;

use crate::AppState;
use crate::error::AppError;
use crate::repo::messages::{self, ListKind};
use crate::repo::settings as stored;
use crate::repo::todo_reminders::{self, Subscriber};
use crate::repo::todos::{self, ListedTodo};
use crate::settings::Settings;

const REMINDER: &str = "⏰";
const NOT_YET: &str = "⬜";

/// The local hour and name of each reminder. Roughly six hours apart, kept
/// to waking hours.
const SLOTS: [(u32, &str); 4] = [
    (8, "Morning"),
    (13, "Afternoon"),
    (18, "After work"),
    (22, "Before bed"),
];

/// How long after its time a reminder can still go out. If the bot was down
/// for longer, it's skipped: a "Morning" reminder at noon helps nobody.
const LATE_BY_AT_MOST: i64 = 30;

/// The reminder due at `now` in `timezone`, if any: its name and the moment
/// it was for. A local time the clocks skip that day gives none.
pub fn due_slot(now: DateTime<Utc>, timezone: Tz) -> Option<(&'static str, DateTime<Utc>)> {
    let today = now.with_timezone(&timezone).date_naive();
    SLOTS.iter().find_map(|&(hour, name)| {
        let local = today.and_time(NaiveTime::from_hms_opt(hour, 0, 0)?);
        let at = timezone
            .from_local_datetime(&local)
            .earliest()?
            .with_timezone(&Utc);
        let in_window = at <= now && now < at + Duration::minutes(LATE_BY_AT_MOST);
        in_window.then_some((name, at))
    })
}

/// This person's open todos with their numbers in the whole list, so the
/// numbers match `/todo`. In a DM, unassigned ones count too: it's their
/// private list.
pub fn open_for<'a>(
    items: &'a [ListedTodo],
    name: &str,
    is_dm: bool,
) -> Vec<(usize, &'a ListedTodo)> {
    (1..)
        .zip(items)
        .filter(|(_, todo)| todo.closed.is_none())
        .filter(|(_, todo)| match todo.assignee.as_deref() {
            Some(assignee) => assignee.eq_ignore_ascii_case(name),
            None => is_dm,
        })
        .collect()
}

/// The reminder's text. `mention` is a phone number to @mention in a group.
pub fn format(slot: &str, todos: &[(usize, &ListedTodo)], mention: Option<&str>) -> String {
    let who = mention.map_or(String::new(), |phone| format!("@{phone} "));
    let count = match todos.len() {
        1 => "1 todo".to_string(),
        n => format!("{n} todos"),
    };
    let mut text = format!("{who}{REMINDER} *{slot}*: you have {count}\n");
    for (number, todo) in todos {
        text.push_str(&format!("{NOT_YET} {number}. {}\n", todo.title));
    }
    let first = todos.first().map_or(1, |(number, _)| *number);
    text.push_str(&format!(
        "\nReply `done {first}`, or send `+done {first}` any time. \
         `/todo reminders off` stops these."
    ));
    text
}

/// Sends every todo reminder due at `now`. One person's problem is logged
/// and doesn't stop the others'.
pub async fn check(state: &AppState, now: DateTime<Utc>) -> Result<(), AppError> {
    let subscribers = {
        let mut conn = state.db.acquire().await?;
        todo_reminders::subscribers(&mut conn).await?
    };
    for subscriber in &subscribers {
        if let Err(err) = check_subscriber(state, subscriber, now).await {
            warn!("{} failed: {err}", subscriber.display_name);
        }
    }
    Ok(())
}

/// Their settings as `/todo` sees them: the chat's, with their personal
/// ones on top in a group. That decides whose clock the reminders follow.
async fn settings_for(
    state: &AppState,
    conn: &mut sqlx::SqliteConnection,
    subscriber: &Subscriber,
) -> Result<Settings, AppError> {
    let defaults = &state.apis.prayer_times.defaults;
    let person = Some(subscriber.user_id);
    Ok(stored::effective(conn, defaults, subscriber.chat_id, person).await?)
}

async fn check_subscriber(
    state: &AppState,
    subscriber: &Subscriber,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    let mut conn = state.db.acquire().await?;
    let settings = settings_for(state, &mut conn, subscriber).await?;
    let Some((slot, slot_at)) = due_slot(now, settings.timezone) else {
        return Ok(());
    };
    // Claimed before sending, like prayer reminders: a failed send is
    // logged, never sent twice.
    let (chat_id, user_id) = (subscriber.chat_id, subscriber.user_id);
    if !todo_reminders::claim(&mut conn, chat_id, user_id, slot_at).await? {
        return Ok(());
    }

    let items = todos::list_all(&mut conn, chat_id).await?;
    let is_dm = subscriber.dm_user_id.is_some();
    let open = open_for(&items, &subscriber.display_name, is_dm);
    // Nothing to do: no message at all.
    if open.is_empty() {
        return Ok(());
    }

    let phone = subscriber
        .wa_jid
        .split_once('@')
        .map_or(subscriber.wa_jid.as_str(), |(number, _)| number);
    let mention = (!is_dm).then_some(phone);
    let text = format(slot, &open, mention);
    let mentions: Vec<String> = mention.map(str::to_string).into_iter().collect();
    let sent = state
        .gowa
        .send_text(&subscriber.wa_chat_id, &text, None, &mentions)
        .await;
    match sent {
        // Saved like a `/todo` list, so `done 3` as a reply and `+done 3`
        // both work. Every todo is saved, to keep `/todo`'s numbers.
        Ok(message_id) => {
            let ids: Vec<i64> = items.iter().map(|t| t.id).collect();
            messages::save_list(&mut conn, &message_id, chat_id, ListKind::Todo, &ids, now).await?;
        }
        Err(err) => warn!("couldn't send to {}: {err}", subscriber.display_name),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::todos::Closed;
    use crate::testkit::utc;

    const LONDON: Tz = chrono_tz::Europe::London;

    #[test]
    fn slots_are_due_for_half_an_hour_on_the_persons_clock() {
        // GMT, so London time is UTC.
        let name = |s| due_slot(utc(s), LONDON).map(|(name, _)| name);
        assert_eq!(name("2026-11-02T08:00:00Z"), Some("Morning"));
        assert_eq!(name("2026-11-02T08:29:00Z"), Some("Morning"));
        assert_eq!(name("2026-11-02T08:30:00Z"), None, "too late");
        assert_eq!(name("2026-11-02T07:59:00Z"), None, "too early");
        assert_eq!(name("2026-11-02T13:00:00Z"), Some("Afternoon"));
        assert_eq!(name("2026-11-02T18:10:00Z"), Some("After work"));
        assert_eq!(name("2026-11-02T22:10:00Z"), Some("Before bed"));
        assert_eq!(
            name("2026-11-02T03:00:00Z"),
            None,
            "the middle of the night"
        );
    }

    #[test]
    fn another_timezone_has_its_own_times() {
        // 08:00 in Colombo (UTC+5:30) is 02:30 UTC.
        let colombo = chrono_tz::Asia::Colombo;
        let (name, at) = due_slot(utc("2026-11-02T02:35:00Z"), colombo).unwrap();
        assert_eq!((name, at), ("Morning", utc("2026-11-02T02:30:00Z")));
        assert_eq!(due_slot(utc("2026-11-02T02:35:00Z"), LONDON), None);
    }

    fn todo(title: &str, assignee: Option<&str>, done: bool) -> ListedTodo {
        ListedTodo {
            id: 1,
            title: title.to_string(),
            assignee: assignee.map(str::to_string),
            closed: done.then(|| Closed {
                by: "Bilal".to_string(),
                at: utc("2026-11-02T08:00:00Z"),
                skipped: false,
            }),
            due: None,
            priority: None,
        }
    }

    fn sample() -> Vec<ListedTodo> {
        vec![
            todo("hoover", Some("Bilal"), false),
            todo("call the school", Some("Khadija"), false),
            todo("old one", None, false),
            todo("bins", Some("bilal"), false),
            todo("gym", Some("Bilal"), true),
        ]
    }

    #[test]
    fn only_their_own_open_todos_keeping_the_numbers() {
        let items = sample();
        let numbers = |open: Vec<(usize, &ListedTodo)>| -> Vec<usize> {
            open.into_iter().map(|(number, _)| number).collect()
        };
        assert_eq!(numbers(open_for(&items, "Bilal", false)), [1, 4]);
        assert_eq!(numbers(open_for(&items, "Khadija", false)), [2]);
        assert_eq!(numbers(open_for(&items, "Yusuf", false)), [0_usize; 0]);
        assert_eq!(numbers(open_for(&items, "Bilal", true)), [1, 3, 4], "a DM");
    }

    #[test]
    fn the_message_lists_them_and_how_to_stop() {
        let items = sample();
        let open = open_for(&items, "Bilal", false);
        assert_eq!(
            format("After work", &open, Some("447700900001")),
            "@447700900001 ⏰ *After work*: you have 2 todos\n\
             ⬜ 1. hoover\n⬜ 4. bins\n\n\
             Reply `done 1`, or send `+done 1` any time. `/todo reminders off` stops these."
        );
        let one = open_for(&items, "Khadija", false);
        assert!(format("Morning", &one, None).starts_with("⏰ *Morning*: you have 1 todo\n"));
    }

    // ---- whose settings apply (`settings_for`) ----
    //
    // Pinning tests, like `my_settings`'s in `bot`: what the code does
    // today. Defaults Europe/London, the group Asia/Colombo, Bilal's own
    // (from his DM) America/New_York.

    use crate::db::test_pool;
    use crate::repo::users::User;
    use crate::testkit::{Family, seed_family, test_state};

    const NEW_YORK: Tz = chrono_tz::America::New_York;
    const COLOMBO: Tz = chrono_tz::Asia::Colombo;

    /// `user`'s todo reminders in chat `chat_id`, as `subscribers` lists them.
    fn subscriber(chat_id: i64, dm_user_id: Option<i64>, user: &User) -> Subscriber {
        Subscriber {
            chat_id,
            wa_chat_id: String::new(),
            dm_user_id,
            user_id: user.id,
            display_name: user.display_name.clone(),
            wa_jid: user.wa_jid.clone(),
        }
    }

    /// The family, with the group on Colombo time and Bilal on New York
    /// time in his DM. An address nothing listens on is fine for GOWA:
    /// nothing is sent.
    async fn colombo_and_new_york() -> (std::sync::Arc<AppState>, Family) {
        let state = test_state(test_pool().await, "http://localhost:1");
        let mut conn = state.db.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T12:00:00Z")).await;
        stored::set_timezone(&mut conn, family.group, "Asia/Colombo")
            .await
            .unwrap();
        stored::set_timezone(&mut conn, family.dm, "America/New_York")
            .await
            .unwrap();
        drop(conn);
        (state, family)
    }

    #[tokio::test]
    async fn settings_for_a_dm_is_the_dms_own_settings() {
        let (state, family) = colombo_and_new_york().await;
        let mut conn = state.db.acquire().await.unwrap();
        let bilal = subscriber(family.dm, Some(family.bilal.id), &family.bilal);
        let settings = settings_for(&state, &mut conn, &bilal).await.unwrap();
        assert_eq!(settings.timezone, NEW_YORK);
    }

    #[tokio::test]
    async fn settings_for_a_group_uses_personal_timezone_over_the_groups() {
        let (state, family) = colombo_and_new_york().await;
        let mut conn = state.db.acquire().await.unwrap();
        let bilal = subscriber(family.group, None, &family.bilal);
        let settings = settings_for(&state, &mut conn, &bilal).await.unwrap();
        assert_eq!(settings.timezone, NEW_YORK);
    }

    #[tokio::test]
    async fn settings_for_a_group_member_without_a_dm_is_the_groups() {
        let (state, family) = colombo_and_new_york().await;
        let mut conn = state.db.acquire().await.unwrap();
        let khadija = subscriber(family.group, None, &family.khadija);
        let settings = settings_for(&state, &mut conn, &khadija).await.unwrap();
        assert_eq!(settings.timezone, COLOMBO);
    }

    #[tokio::test]
    async fn settings_for_a_group_without_its_own_is_personal_over_the_defaults() {
        let (state, family) = colombo_and_new_york().await;
        let mut conn = state.db.acquire().await.unwrap();
        stored::reset(&mut conn, family.group).await.unwrap();
        let bilal = subscriber(family.group, None, &family.bilal);
        assert_eq!(
            settings_for(&state, &mut conn, &bilal)
                .await
                .unwrap()
                .timezone,
            NEW_YORK
        );
        let khadija = subscriber(family.group, None, &family.khadija);
        assert_eq!(
            settings_for(&state, &mut conn, &khadija)
                .await
                .unwrap()
                .timezone,
            chrono_tz::Europe::London
        );
    }

    #[tokio::test]
    async fn settings_for_a_group_still_uses_a_dm_marked_left() {
        let (state, family) = colombo_and_new_york().await;
        let mut conn = state.db.acquire().await.unwrap();
        // Only groups are ever marked left by the bot; done by hand here.
        let left = Some(utc("2026-11-01T13:00:00Z"));
        crate::repo::cleanup::set_left(&mut conn, family.dm, left)
            .await
            .unwrap();
        let bilal = subscriber(family.group, None, &family.bilal);
        let settings = settings_for(&state, &mut conn, &bilal).await.unwrap();
        assert_eq!(settings.timezone, NEW_YORK);
    }

    #[test]
    fn slots_follow_bst_in_summer() {
        // British Summer Time is UTC+1, so 08:00 on the clock is 07:00 UTC.
        let (name, at) = due_slot(utc("2026-07-06T07:05:00Z"), LONDON).unwrap();
        assert_eq!((name, at), ("Morning", utc("2026-07-06T07:00:00Z")));
        assert_eq!(
            due_slot(utc("2026-07-06T08:05:00Z"), LONDON),
            None,
            "09:05 BST: the morning slot is over"
        );
    }

    // ---- the whole check, with a fake GOWA ----
    //
    // Pinning tests: what a tick of the loop sends, and what it leaves
    // behind for replies.

    use crate::bot::Context;
    use crate::bot::lists::run_list_reply;
    use crate::repo::todos::Details;
    use crate::testkit::{FakeGowa, fake_gowa};

    /// A fake GOWA, a state on it, and the family, set up on 1 November.
    async fn family_on_fake_gowa() -> (std::sync::Arc<AppState>, FakeGowa, Family) {
        let gowa = fake_gowa().await;
        let state = test_state(test_pool().await, &gowa.url);
        let mut conn = state.db.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T12:00:00Z")).await;
        drop(conn);
        (state, gowa, family)
    }

    /// Adds a todo to `chat_id`, by `by`, for `assignee` if given.
    async fn add_todo(state: &AppState, chat_id: i64, by: i64, title: &str, assignee: Option<i64>) {
        let mut conn = state.db.acquire().await.unwrap();
        let at = utc("2026-11-01T12:00:00Z");
        todos::add(
            &mut conn,
            chat_id,
            title,
            by,
            assignee,
            Details::default(),
            at,
        )
        .await
        .unwrap();
    }

    /// Turns todo reminders on for `user_id` in `chat_id`.
    async fn reminders_on(state: &AppState, chat_id: i64, user_id: i64) {
        let mut conn = state.db.acquire().await.unwrap();
        todo_reminders::enable(&mut conn, chat_id, user_id, utc("2026-11-01T12:00:00Z"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_reminder_goes_once_per_slot_and_a_reply_to_it_ticks_off() {
        let (state, gowa, family) = family_on_fake_gowa().await;
        let bilal = family.bilal.id;
        add_todo(&state, family.dm, bilal, "hoover", None).await;
        add_todo(&state, family.dm, bilal, "bins", None).await;
        reminders_on(&state, family.dm, bilal).await;

        // 08:05 GMT on Monday 2 November: the morning slot.
        check(&state, utc("2026-11-02T08:05:00Z")).await.unwrap();
        {
            let sent = gowa.sent.lock().unwrap();
            assert_eq!(sent.len(), 1, "{sent:?}");
            assert_eq!(sent[0].0, family.bilal.wa_jid, "to his DM");
            assert!(
                sent[0]
                    .1
                    .starts_with("⏰ *Morning*: you have 2 todos\n⬜ 1. hoover\n⬜ 2. bins\n"),
                "{}",
                sent[0].1
            );
        }

        // The next tick, still in the morning slot: already claimed.
        check(&state, utc("2026-11-02T08:06:00Z")).await.unwrap();
        assert_eq!(gowa.sent.lock().unwrap().len(), 1);

        // The list was saved against the sent message's ID ("FAKE1"), so a
        // reply `done 2` to it finds the right todo.
        let mut conn = state.db.acquire().await.unwrap();
        let list = messages::list_items(&mut conn, "FAKE1", family.dm)
            .await
            .unwrap()
            .expect("the reminder's list was saved");
        let ctx = Context {
            chat_id: family.dm,
            is_group: false,
            sender: family.bilal.clone(),
            now: utc("2026-11-02T08:10:00Z"),
        };
        let reply = crate::parser::parse_reply("done 2").unwrap();
        let defaults = crate::settings::tests::defaults();
        let answer = run_list_reply(&mut conn, &ctx, &defaults, &list, reply)
            .await
            .unwrap()
            .into_text();
        assert!(answer.starts_with("✅ Done: *bins*"), "{answer}");
    }

    #[tokio::test]
    async fn nothing_open_sends_nothing_but_still_uses_up_the_slot() {
        let (state, gowa, family) = family_on_fake_gowa().await;
        reminders_on(&state, family.dm, family.bilal.id).await;

        check(&state, utc("2026-11-02T08:05:00Z")).await.unwrap();
        assert!(gowa.sent.lock().unwrap().is_empty());

        // A todo added later in the same slot waits for the next one: the
        // slot was claimed even though there was nothing to say.
        add_todo(&state, family.dm, family.bilal.id, "hoover", None).await;
        check(&state, utc("2026-11-02T08:10:00Z")).await.unwrap();
        assert!(gowa.sent.lock().unwrap().is_empty());

        check(&state, utc("2026-11-02T13:00:00Z")).await.unwrap();
        assert_eq!(gowa.sent.lock().unwrap().len(), 1, "the afternoon's");
    }

    #[tokio::test]
    async fn in_a_group_it_mentions_them_and_lists_only_theirs() {
        let (state, gowa, family) = family_on_fake_gowa().await;
        let (bilal, khadija) = (family.bilal.id, family.khadija.id);
        add_todo(&state, family.group, bilal, "hoover", Some(bilal)).await;
        add_todo(
            &state,
            family.group,
            bilal,
            "call the school",
            Some(khadija),
        )
        .await;
        reminders_on(&state, family.group, khadija).await;

        // 18:00 GMT: the "After work" slot.
        check(&state, utc("2026-11-02T18:00:00Z")).await.unwrap();
        let sent = gowa.sent.lock().unwrap();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, crate::testkit::GROUP);
        assert!(
            sent[0].1.starts_with(
                "@447700900002 ⏰ *After work*: you have 1 todo\n⬜ 2. call the school\n"
            ),
            "{}",
            sent[0].1
        );
    }
}

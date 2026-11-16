//! Jot: an opt-in log of what someone has done. While it's on, the bot DMs
//! them every N hours of their active hours, and what they reply is saved
//! as an entry. DM only.
//!
//! `slots_on`, `due_slot`, `next_prompt` and `plan` are pure; `check` reads
//! the database and hands the plans to the reminder engine to claim and
//! send, called by the reminder loop every minute. Active hours are on the
//! person's own clock, so a prompt keeps its local time across the BST/GMT
//! change.

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use sqlx::SqliteConnection;
use tracing::warn;

use crate::AppState;
use crate::bot::emoji::{DONE, PROBLEM};
use crate::bot::{Context, my_settings};
use crate::error::AppError;
use crate::parser::JotCommand;
use crate::reminders::engine::{self, Claim, OnSent, Outgoing, Planned};
use crate::repo::jot::{self, JotSettings, Subscriber};
use crate::repo::settings as stored;
use crate::settings::Settings;

const PROMPT: &str = "✏️";

/// What `/jot on` uses for whatever it isn't told.
pub const DEFAULT_INTERVAL_MINUTES: u32 = 120;
pub const DEFAULT_START: NaiveTime = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
pub const DEFAULT_END: NaiveTime = NaiveTime::from_hms_opt(18, 0, 0).unwrap();

/// How long after its time a prompt can still go out. If the bot was down
/// for longer it's skipped: "what have you done since 09:00?" at 11:50
/// helps nobody.
const LATE_BY_AT_MOST: i64 = 30;

/// The moment of a local clock time on `date`. `None` if the clocks skip
/// that time (spring forward); the earlier moment if it happens twice.
fn local(date: NaiveDate, time: NaiveTime, timezone: Tz) -> Option<DateTime<Utc>> {
    let moment = timezone
        .from_local_datetime(&date.and_time(time))
        .earliest()?;
    Some(moment.with_timezone(&Utc))
}

/// The prompts of one local day, in order: the start time, then every
/// interval after it, while before the end. The steps are on the clock
/// (09:00, 11:00, ...), not elapsed time, so they don't move when the
/// clocks change.
pub fn slots_on(
    date: NaiveDate,
    timezone: Tz,
    interval_minutes: u32,
    start: NaiveTime,
    end: NaiveTime,
) -> Vec<DateTime<Utc>> {
    let minutes = |t: NaiveTime| t.hour() * 60 + t.minute();
    let (first, last) = (minutes(start), minutes(end));
    let step = interval_minutes.max(1) as usize;
    (first..last)
        .step_by(step)
        .filter_map(|m| NaiveTime::from_hms_opt(m / 60, m % 60, 0))
        .filter_map(|time| local(date, time, timezone))
        .collect()
}

/// A prompt that's due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DueSlot {
    pub at: DateTime<Utc>,
    /// The prompt before it that day, `None` for the day's first.
    pub previous: Option<DateTime<Utc>>,
}

/// The prompt due at `now` on the person's clock, if any.
pub fn due_slot(now: DateTime<Utc>, timezone: Tz, settings: &JotSettings) -> Option<DueSlot> {
    let today = now.with_timezone(&timezone).date_naive();
    let slots = slots_on(
        today,
        timezone,
        settings.interval_minutes,
        settings.start,
        settings.end,
    );
    let index = slots
        .iter()
        .position(|&at| at <= now && now < at + Duration::minutes(LATE_BY_AT_MOST))?;
    Some(DueSlot {
        at: slots[index],
        previous: index.checked_sub(1).map(|i| slots[i]),
    })
}

/// The next prompt after `now`: later today, or the first of a following
/// day.
pub fn next_prompt(
    now: DateTime<Utc>,
    timezone: Tz,
    settings: &JotSettings,
) -> Option<DateTime<Utc>> {
    let today = now.with_timezone(&timezone).date_naive();
    // A few days, in case clocks skipping a time empties one.
    (0..4).find_map(|days| {
        let date = today + Duration::days(days);
        slots_on(
            date,
            timezone,
            settings.interval_minutes,
            settings.start,
            settings.end,
        )
        .into_iter()
        .find(|&at| at > now)
    })
}

/// The prompt's text: `✏️ 11:00 · Jot time: what have you done since 09:00?`
pub fn prompt_text(slot: &DueSlot, settings: &Settings) -> String {
    let since = match slot.previous {
        Some(previous) => format!("since {}", settings.clock(previous)),
        None => "since you started".to_string(),
    };
    format!(
        "{PROMPT} {} · Jot time: what have you done {since}?",
        settings.clock(slot.at)
    )
}

/// What one person's prompt for `slot` sends. Claimed before sending, like
/// every reminder: a failed send is logged, never sent twice. Pure.
pub fn plan(subscriber: &Subscriber, slot: &DueSlot, settings: &Settings) -> Planned {
    Planned {
        claim: Claim::JotPrompt {
            chat_id: subscriber.chat_id,
            user_id: subscriber.user_id,
            slot_at: slot.at,
        },
        messages: vec![Outgoing {
            to: subscriber.wa_chat_id.clone(),
            text: prompt_text(slot, settings),
            mentions: Vec::new(),
            quote: None,
            reminder: None,
            // So a reply quoting it finds its slot.
            on_sent: OnSent::JotPrompt {
                user_id: subscriber.user_id,
                slot_at: slot.at,
            },
            push: Vec::new(),
            what: format!("{}'s jot prompt", subscriber.display_name),
        }],
    }
}

/// Sends every Jot prompt due at `now`. One person's problem is logged and
/// doesn't stop the others'.
pub async fn check(state: &AppState, now: DateTime<Utc>) -> Result<(), AppError> {
    let subscribers = {
        let mut conn = state.db.acquire().await?;
        jot::subscribers(&mut conn).await?
    };
    let mut planned = Vec::new();
    for subscriber in &subscribers {
        match plan_subscriber(state, subscriber, now).await {
            Ok(plan) => planned.extend(plan),
            Err(err) => warn!("{} failed: {err}", subscriber.display_name),
        }
    }
    engine::deliver(
        &state.gowa,
        &state.db,
        state.apis.push.as_ref(),
        planned,
        now,
    )
    .await;
    Ok(())
}

async fn plan_subscriber(
    state: &AppState,
    subscriber: &Subscriber,
    now: DateTime<Utc>,
) -> Result<Option<Planned>, AppError> {
    let defaults = &state.apis.prayer_times.defaults;
    let settings = {
        let mut conn = state.db.acquire().await?;
        stored::effective(
            &mut conn,
            defaults,
            subscriber.chat_id,
            Some(subscriber.user_id),
        )
        .await?
    };
    let Some(slot) = due_slot(now, settings.timezone, &subscriber.settings) else {
        return Ok(None);
    };
    // Turning it on at 11:10 doesn't send the 11:00 prompt.
    if slot.at < subscriber.settings.updated_at {
        return Ok(None);
    }
    Ok(Some(plan(subscriber, &slot, &settings)))
}

fn interval_label(minutes: u32) -> String {
    if minutes.is_multiple_of(60) {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

fn hours_label(settings: &JotSettings) -> String {
    format!(
        "{}–{}",
        settings.start.format("%H:%M"),
        settings.end.format("%H:%M")
    )
}

fn next_line(settings: &JotSettings, now: DateTime<Utc>, mine: &Settings) -> String {
    match next_prompt(now, mine.timezone, settings) {
        Some(at) => {
            let today = mine.today(now);
            let day = if mine.today(at) == today {
                String::new()
            } else {
                format!(
                    " on {}",
                    at.with_timezone(&mine.timezone).format("%a %-d %b")
                )
            };
            format!("Next prompt: {}{day}.", mine.clock(at))
        }
        None => "No prompt fits those hours.".to_string(),
    }
}

/// `/jot`, `/jot on`, `/jot off`, `+jot` and a reply to a prompt: the
/// handler for every Jot command. DM only; in a group nothing is stored.
pub async fn command(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    command: JotCommand,
) -> Result<String, AppError> {
    if ctx.is_group {
        return Ok(format!(
            "{PROBLEM} Jot only works in a DM with me. Message me directly and send `/jot on`."
        ));
    }
    let user_id = ctx.sender.id;
    let mine = my_settings(conn, ctx, defaults).await?;
    let current = jot::get(conn, user_id).await?;
    Ok(match command {
        JotCommand::Invalid(err) => format!("{PROBLEM} {err}"),
        JotCommand::On {
            interval_minutes,
            hours,
        } => {
            // What isn't said stays as it was, or is the default.
            let interval_minutes = interval_minutes
                .or(current.map(|c| c.interval_minutes))
                .unwrap_or(DEFAULT_INTERVAL_MINUTES);
            let (start, end) = hours
                .or(current.map(|c| (c.start, c.end)))
                .unwrap_or((DEFAULT_START, DEFAULT_END));
            jot::turn_on(conn, user_id, interval_minutes, start, end, ctx.now).await?;
            let now_on = JotSettings {
                enabled: true,
                interval_minutes,
                start,
                end,
                updated_at: ctx.now,
            };
            format!(
                "{PROMPT} Jot is on: every {}, {}. {}\nReply to a prompt to log it, or send `+jot <text>` any time. `/jot off` stops it.",
                interval_label(interval_minutes),
                hours_label(&now_on),
                next_line(&now_on, ctx.now, &mine)
            )
        }
        JotCommand::Off => {
            jot::turn_off(conn, user_id).await?;
            format!("{PROMPT} Jot is off. `/jot on` turns it back on.")
        }
        JotCommand::Status => match current {
            Some(c) if c.enabled => format!(
                "{PROMPT} Jot is on: every {}, {}. {}",
                interval_label(c.interval_minutes),
                hours_label(&c),
                next_line(&c, ctx.now, &mine)
            ),
            _ => format!(
                "{PROMPT} Jot is off. `/jot on 2h 09:00-18:00` turns it on: I'll ask what you've done every 2 hours, 09:00–18:00."
            ),
        },
        JotCommand::Log(text) => {
            jot::add_entry(conn, user_id, ctx.now, mine.today(ctx.now), None, &text).await?;
            format!("{DONE} Jotted.")
        }
        JotCommand::Reply { text, slot_at } => {
            jot::add_entry(
                conn,
                user_id,
                ctx.now,
                mine.today(ctx.now),
                Some(slot_at),
                &text,
            )
            .await?;
            format!("{DONE} Jotted for {}.", mine.clock(slot_at))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::parser;
    use crate::settings::tests::defaults;
    use crate::testkit::{date, seed_family, utc};

    const LONDON: Tz = chrono_tz::Europe::London;

    fn at(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn two_hourly() -> JotSettings {
        JotSettings {
            enabled: true,
            interval_minutes: 120,
            start: at(9, 0),
            end: at(18, 0),
            updated_at: utc("2026-11-01T00:00:00Z"),
        }
    }

    fn slots(day: &str) -> Vec<DateTime<Utc>> {
        slots_on(date(day), LONDON, 120, at(9, 0), at(18, 0))
    }

    #[test]
    fn two_hourly_from_nine_to_eighteen_is_nine_to_seventeen() {
        // GMT, so London time is UTC.
        let expected: Vec<_> = ["09", "11", "13", "15", "17"]
            .iter()
            .map(|h| utc(&format!("2026-11-02T{h}:00:00Z")))
            .collect();
        assert_eq!(slots("2026-11-02"), expected);
    }

    #[test]
    fn ninety_minutes_steps_on_the_clock() {
        let found = slots_on(date("2026-11-02"), LONDON, 90, at(9, 0), at(12, 0));
        assert_eq!(
            found,
            [utc("2026-11-02T09:00:00Z"), utc("2026-11-02T10:30:00Z"),]
        );
    }

    #[test]
    fn prompts_keep_their_local_time_across_the_clocks_changing() {
        // BST (UTC+1) until Sunday 25 October 2026, GMT after.
        assert_eq!(slots("2026-10-24")[0], utc("2026-10-24T08:00:00Z"));
        assert_eq!(slots("2026-10-24")[4], utc("2026-10-24T16:00:00Z"));
        assert_eq!(slots("2026-10-25")[0], utc("2026-10-25T09:00:00Z"));
        assert_eq!(slots("2026-10-25")[4], utc("2026-10-25T17:00:00Z"));
        // And the other way: GMT until Sunday 29 March 2026.
        assert_eq!(slots("2026-03-28")[1], utc("2026-03-28T11:00:00Z"));
        assert_eq!(slots("2026-03-29")[1], utc("2026-03-29T10:00:00Z"));
    }

    #[test]
    fn a_prompt_is_due_for_half_an_hour_and_never_outside_the_hours() {
        let due = |s: &str| due_slot(utc(s), LONDON, &two_hourly()).map(|d| d.at);
        assert_eq!(
            due("2026-11-02T11:00:00Z"),
            Some(utc("2026-11-02T11:00:00Z"))
        );
        assert_eq!(
            due("2026-11-02T11:29:00Z"),
            Some(utc("2026-11-02T11:00:00Z"))
        );
        assert_eq!(due("2026-11-02T11:30:00Z"), None, "too late");
        assert_eq!(due("2026-11-02T10:59:00Z"), None, "too early");
        assert_eq!(due("2026-11-02T19:00:00Z"), None, "after the hours");
        assert_eq!(due("2026-11-02T07:00:00Z"), None, "before the hours");
        assert_eq!(due("2026-11-02T18:00:00Z"), None, "the end is not a prompt");
    }

    #[test]
    fn the_first_prompt_has_no_previous_and_the_rest_name_the_one_before() {
        let first = due_slot(utc("2026-11-02T09:00:00Z"), LONDON, &two_hourly()).unwrap();
        assert_eq!(first.previous, None);
        let second = due_slot(utc("2026-11-02T11:00:00Z"), LONDON, &two_hourly()).unwrap();
        assert_eq!(second.previous, Some(utc("2026-11-02T09:00:00Z")));
    }

    #[test]
    fn the_prompt_reads_as_in_the_issue() {
        let settings = defaults();
        let first = due_slot(utc("2026-11-02T09:00:00Z"), LONDON, &two_hourly()).unwrap();
        assert_eq!(
            prompt_text(&first, &settings),
            "✏️ 09:00 · Jot time: what have you done since you started?"
        );
        let second = due_slot(utc("2026-11-02T11:00:00Z"), LONDON, &two_hourly()).unwrap();
        assert_eq!(
            prompt_text(&second, &settings),
            "✏️ 11:00 · Jot time: what have you done since 09:00?"
        );
        // In summer the same local times are an hour earlier in UTC.
        let summer = due_slot(utc("2026-07-01T10:00:00Z"), LONDON, &two_hourly()).unwrap();
        assert_eq!(
            prompt_text(&summer, &settings),
            "✏️ 11:00 · Jot time: what have you done since 09:00?"
        );
    }

    #[test]
    fn the_next_prompt_is_later_today_or_the_first_tomorrow() {
        let next = |s: &str| next_prompt(utc(s), LONDON, &two_hourly());
        assert_eq!(
            next("2026-11-02T10:00:00Z"),
            Some(utc("2026-11-02T11:00:00Z"))
        );
        assert_eq!(
            next("2026-11-02T11:00:00Z"),
            Some(utc("2026-11-02T13:00:00Z"))
        );
        assert_eq!(
            next("2026-11-02T17:30:00Z"),
            Some(utc("2026-11-03T09:00:00Z"))
        );
        assert_eq!(
            next("2026-11-02T06:00:00Z"),
            Some(utc("2026-11-02T09:00:00Z"))
        );
    }

    // ---- the commands, with a database ----

    fn command_of(text: &str) -> JotCommand {
        match parser::parse(text) {
            Some(crate::parser::BotCommand::Jot(command)) => command,
            other => panic!("expected a jot command, got {other:?}"),
        }
    }

    fn ctx(chat_id: i64, is_group: bool, user: &crate::repo::users::User, now: &str) -> Context {
        Context {
            chat_id,
            is_group,
            sender: user.clone(),
            now: utc(now),
        }
    }

    async fn count(conn: &mut SqliteConnection, table: &'static str) -> i64 {
        // Literal names from this module's tests only.
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
            .fetch_one(conn)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn on_off_and_status_in_a_dm() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-02T08:00:00Z")).await;
        let ctx = ctx(family.dm, false, &family.bilal, "2026-11-02T10:00:00Z");

        let status = command(&mut conn, &ctx, &defaults(), command_of("/jot"))
            .await
            .unwrap();
        assert!(status.contains("Jot is off"), "{status}");

        let on = command(
            &mut conn,
            &ctx,
            &defaults(),
            command_of("/jot on 2h 09:00-18:00"),
        )
        .await
        .unwrap();
        assert!(on.contains("every 2h, 09:00–18:00"), "{on}");
        assert!(on.contains("Next prompt: 11:00."), "{on}");

        let status = command(&mut conn, &ctx, &defaults(), command_of("/j"))
            .await
            .unwrap();
        assert!(
            status.contains("Jot is on: every 2h, 09:00–18:00. Next prompt: 11:00."),
            "{status}"
        );

        let off = command(&mut conn, &ctx, &defaults(), command_of("/jot off"))
            .await
            .unwrap();
        assert!(off.contains("Jot is off"), "{off}");
        assert!(jot::subscribers(&mut conn).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn plus_jot_saves_an_entry_without_a_slot_and_a_reply_saves_one_with() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-02T08:00:00Z")).await;
        let ctx = ctx(family.dm, false, &family.bilal, "2026-11-02T11:10:00Z");

        let said = command(
            &mut conn,
            &ctx,
            &defaults(),
            command_of("+j wrote the report"),
        )
        .await
        .unwrap();
        assert!(said.starts_with(DONE), "{said}");
        let slot = utc("2026-11-02T11:00:00Z");
        let reply = JotCommand::Reply {
            text: "emails".to_string(),
            slot_at: slot,
        };
        command(&mut conn, &ctx, &defaults(), reply).await.unwrap();

        let rows: Vec<(String, Option<String>, String)> =
            sqlx::query_as("SELECT text, slot_at, day FROM jot_entries ORDER BY id")
                .fetch_all(&mut *conn)
                .await
                .unwrap();
        assert_eq!(
            rows[0],
            (
                "wrote the report".to_string(),
                None,
                "2026-11-02".to_string()
            )
        );
        assert_eq!(rows[1].0, "emails");
        assert!(rows[1].1.is_some());
    }

    #[tokio::test]
    async fn in_a_group_it_refuses_politely_and_stores_nothing() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-02T08:00:00Z")).await;
        let ctx = ctx(family.group, true, &family.bilal, "2026-11-02T10:00:00Z");

        for text in ["/jot on", "/jot", "+jot hello", "/jot off"] {
            let said = command(&mut conn, &ctx, &defaults(), command_of(text))
                .await
                .unwrap();
            assert!(said.contains("Jot only works in a DM with me"), "{said}");
        }
        assert_eq!(count(&mut conn, "jot_settings").await, 0);
        assert_eq!(count(&mut conn, "jot_entries").await, 0);
    }

    #[tokio::test]
    async fn turning_it_on_mid_slot_does_not_send_the_slot_that_just_passed() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-02T08:00:00Z")).await;
        let on_at = utc("2026-11-02T11:10:00Z");
        jot::turn_on(&mut conn, family.bilal.id, 120, at(9, 0), at(18, 0), on_at)
            .await
            .unwrap();
        let subscriber = jot::subscribers(&mut conn).await.unwrap().remove(0);
        let slot = due_slot(on_at, LONDON, &subscriber.settings).unwrap();
        assert!(slot.at < subscriber.settings.updated_at);
        // `plan_subscriber` skips it for that reason; the plan itself is
        // for the 11:00 slot.
        let planned = plan(&subscriber, &slot, &defaults());
        assert_eq!(planned.messages.len(), 1);
        assert!(planned.messages[0].text.starts_with("✏️ 11:00 · Jot time"));
    }
}

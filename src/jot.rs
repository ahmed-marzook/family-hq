//! Jot: an opt-in log of what someone has done. While it's on, the bot DMs
//! them every N hours of their active hours, and what they reply is saved
//! as an entry. DM only.
//!
//! Prompts are smart about when they go: not on days the person left out,
//! not while paused, moved to 15 minutes after a prayer time that they'd
//! land just after, and skipped (the entry fills the slot) if something
//! was logged in the last 30 minutes.
//!
//! `slots_on`, `due_slot`, `next_prompt` and `plan` are pure; `check` reads
//! the database and hands the plans to the reminder engine to claim and
//! send, called by the reminder loop every minute. Active hours are on the
//! person's own clock, so a prompt keeps its local time across the BST/GMT
//! change.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use sqlx::SqliteConnection;
use tracing::warn;

use crate::AppState;
use crate::bot::emoji::{DONE, PROBLEM};
use crate::bot::{Context, my_settings};
use crate::error::AppError;
use crate::parser::{JotCommand, JotDay, JotDays, JotPause};
use crate::reminders::engine::{self, Claim, OnSent, Outgoing, Planned};
use crate::repo::jot::{self, Entry, JotSettings, Subscriber};
use crate::repo::settings as stored;
use crate::settings::Settings;

const PROMPT: &str = "✏️";
/// What a reply of `skip` can also be.
pub const SKIP: &str = "😴";

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

/// The prompts of one local day, or none if the person left that weekday
/// out (`-days mon-fri`).
fn day_slots(date: NaiveDate, timezone: Tz, settings: &JotSettings) -> Vec<DateTime<Utc>> {
    if !settings.days.contains(date.weekday()) {
        return Vec::new();
    }
    slots_on(
        date,
        timezone,
        settings.interval_minutes,
        settings.start,
        settings.end,
    )
}

/// A prompt due just after a prayer time waits until this long after it,
/// so it doesn't land in the middle of someone's prayer.
const AFTER_PRAYER: Duration = Duration::minutes(15);

/// When the prompt for `slot` is sent: the slot itself, unless a prayer
/// time started at most 15 minutes before it, then 15 minutes after that
/// prayer starts. Pure, so it's tested with fixed times.
pub fn after_prayers(slot: DateTime<Utc>, prayer_starts: &[DateTime<Utc>]) -> DateTime<Utc> {
    prayer_starts
        .iter()
        .find(|&&start| start <= slot && slot < start + AFTER_PRAYER)
        .map_or(slot, |&start| start + AFTER_PRAYER)
}

/// How far past a slot its prompt can still be due: a prayer shift of up to
/// 15 minutes, then `LATE_BY_AT_MOST`.
fn near(now: DateTime<Utc>, timezone: Tz, settings: &JotSettings) -> bool {
    let today = now.with_timezone(&timezone).date_naive();
    day_slots(today, timezone, settings)
        .iter()
        .any(|&at| at <= now && now < at + AFTER_PRAYER + Duration::minutes(LATE_BY_AT_MOST))
}

/// A prompt that's due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DueSlot {
    /// The slot, which is what an answer is saved against.
    pub at: DateTime<Utc>,
    /// When it's sent: `at`, or later if a prayer time is just before it.
    pub send_at: DateTime<Utc>,
    /// The prompt before it that day, `None` for the day's first.
    pub previous: Option<DateTime<Utc>>,
}

/// The prompt due at `now` on the person's clock, if any. `prayer_starts`
/// are the day's prayer times, `&[]` if they're not known.
pub fn due_slot(
    now: DateTime<Utc>,
    timezone: Tz,
    settings: &JotSettings,
    prayer_starts: &[DateTime<Utc>],
) -> Option<DueSlot> {
    let today = now.with_timezone(&timezone).date_naive();
    let slots = day_slots(today, timezone, settings);
    let index = slots.iter().position(|&at| {
        let send_at = after_prayers(at, prayer_starts);
        send_at <= now && now < send_at + Duration::minutes(LATE_BY_AT_MOST)
    })?;
    Some(DueSlot {
        at: slots[index],
        send_at: after_prayers(slots[index], prayer_starts),
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
    // A week, for a person who left most days out (and in case clocks
    // skipping a time empties one).
    (0..8).find_map(|days| {
        let date = today + Duration::days(days);
        day_slots(date, timezone, settings)
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

/// The end-of-day summary: that day's timeline, once, in the person's DM.
/// Claimed per day like a prompt. Pure.
pub fn summary_plan(
    subscriber: &Subscriber,
    day: NaiveDate,
    label: &str,
    entries: &[Entry],
    slots: &[DateTime<Utc>],
    skipped: &[DateTime<Utc>],
    settings: &Settings,
) -> Planned {
    Planned {
        claim: Claim::JotSummary {
            user_id: subscriber.user_id,
            day,
        },
        messages: vec![Outgoing {
            to: subscriber.wa_chat_id.clone(),
            text: timeline(label, entries, slots, skipped, settings),
            mentions: Vec::new(),
            quote: None,
            reminder: None,
            on_sent: OnSent::Nothing,
            push: Vec::new(),
            what: format!("{}'s jot summary", subscriber.display_name),
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
    for day in summary_days(now, settings.timezone, &subscriber.settings) {
        let mut conn = state.db.acquire().await?;
        // Checked first: the summary stays due for hours after it's sent,
        // and building it means reading the whole day.
        if jot::summary_claimed(&mut conn, subscriber.user_id, day).await? {
            continue;
        }
        let (entries, slots, skipped) =
            day_view(&mut conn, subscriber.user_id, day, settings.timezone).await?;
        let label = label_of(day, settings.today(now));
        return Ok(Some(summary_plan(
            subscriber, day, &label, &entries, &slots, &skipped, &settings,
        )));
    }
    if subscriber.settings.paused_at(now).is_some()
        || !near(now, settings.timezone, &subscriber.settings)
    {
        return Ok(None);
    }
    let prayer_starts = prayer_starts(state, &settings, now).await;
    let Some(slot) = due_slot(now, settings.timezone, &subscriber.settings, &prayer_starts) else {
        return Ok(None);
    };
    // Turning it on at 11:10 doesn't send the 11:00 prompt.
    if slot.at < subscriber.settings.updated_at {
        return Ok(None);
    }
    let mut conn = state.db.acquire().await?;
    // Already sent: the claim keeps it to one send, and an entry logged
    // since is that prompt's answer, not a reason to skip it.
    if !jot::prompt_claimed(&mut conn, subscriber.user_id, slot.at).await? {
        let since = slot.send_at - Duration::minutes(LATE_BY_AT_MOST);
        if let Some(recent) = jot::recent_entry(&mut conn, subscriber.user_id, since, now).await? {
            // They've just said what they're doing, so no prompt; a `+jot`
            // becomes this slot's answer so the timeline shows it there.
            jot::fill_slot(&mut conn, recent.id, slot.at).await?;
            return Ok(None);
        }
    }
    Ok(Some(plan(subscriber, &slot, &settings)))
}

/// When today's prayers start where this person is. Empty if they can't be
/// found: a prompt then goes out at its own time rather than not at all.
async fn prayer_starts(
    state: &AppState,
    settings: &Settings,
    now: DateTime<Utc>,
) -> Vec<DateTime<Utc>> {
    let found = async {
        let location = state.apis.prayer_location(settings).await?;
        state
            .apis
            .prayer_times
            .for_date(&location, settings.today(now))
            .await
    }
    .await;
    match found {
        Ok(day) => day.windows.iter().map(|w| w.start).collect(),
        Err(err) => {
            warn!("jot: no prayer times, sending prompts on time: {err}");
            Vec::new()
        }
    }
}

/// What a day with nothing logged says: short and kind, never an empty
/// timeline.
fn nothing_logged(label: &str) -> String {
    format!(
        "{PROMPT} {label}: nothing logged. That's okay, some days go like that. `+jot <text>` adds a line any time."
    )
}

/// How many of `slots` have at least one entry answering them.
pub fn logged_count(entries: &[Entry], slots: &[DateTime<Utc>]) -> usize {
    slots
        .iter()
        .filter(|slot| entries.iter().any(|e| e.slot_at == Some(**slot)))
        .count()
}

/// The slots that count: not the ones skipped with `skip` and never
/// answered since (a late quoted reply can still answer one). A skipped
/// slot is neither logged nor missed.
pub fn scored_slots(
    slots: &[DateTime<Utc>],
    entries: &[Entry],
    skipped: &[DateTime<Utc>],
) -> Vec<DateTime<Utc>> {
    slots
        .iter()
        .copied()
        .filter(|slot| !is_skipped(*slot, entries, skipped))
        .collect()
}

fn is_skipped(slot: DateTime<Utc>, entries: &[Entry], skipped: &[DateTime<Utc>]) -> bool {
    skipped.contains(&slot) && !entries.iter().any(|e| e.slot_at == Some(slot))
}

/// One day as a timeline: every slot in order, `—` where nothing was
/// logged and `skipped` where the prompt was skipped, then the `+jot`
/// entries as extras, and how many slots were logged. Pure: entries, slots
/// and the clock settings in, text out.
pub fn timeline(
    label: &str,
    entries: &[Entry],
    slots: &[DateTime<Utc>],
    skipped: &[DateTime<Utc>],
    mine: &Settings,
) -> String {
    let counted = scored_slots(slots, entries, skipped);
    if entries.is_empty() && counted.len() == slots.len() {
        return nothing_logged(label);
    }
    let mut lines = vec![format!("{PROMPT} {label}")];
    for slot in slots {
        let texts: Vec<&str> = entries
            .iter()
            .filter(|e| e.slot_at == Some(*slot))
            .map(|e| e.text.as_str())
            .collect();
        let text = if texts.is_empty() {
            if is_skipped(*slot, entries, skipped) {
                "skipped".to_string()
            } else {
                "—".to_string()
            }
        } else {
            texts.join(", ")
        };
        lines.push(format!("{} {text}", mine.clock(*slot)));
    }
    // An entry for a prompt that isn't in today's slots any more (the hours
    // were changed since) is shown as an extra rather than dropped.
    for entry in entries
        .iter()
        .filter(|e| e.slot_at.is_none_or(|s| !slots.contains(&s)))
    {
        lines.push(format!(
            "{} (extra) {}",
            mine.clock(entry.logged_at),
            entry.text
        ));
    }
    if !counted.is_empty() {
        lines.push(format!(
            "{} of {} logged",
            logged_count(entries, &counted),
            counted.len()
        ));
    }
    lines.join("\n")
}

/// One day of `/jot week`: how many of its prompts were answered, out of
/// how many were sent, and how many entries it has in all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeekDay {
    pub date: NaiveDate,
    pub logged: usize,
    pub prompts: usize,
    pub entries: usize,
}

/// `Mon 4/5 · Tue 5/5 · ...`. A day with no prompts (Jot was off, or it
/// was before it was turned on) shows just its entry count, or `—`,
/// rather than a meaningless `0/0`.
pub fn week_line(days: &[WeekDay]) -> String {
    days.iter()
        .map(|d| {
            let name = d.date.format("%a");
            match (d.prompts, d.entries) {
                (0, 0) => format!("{name} —"),
                (0, n) => format!("{name} {n}"),
                (total, _) => format!("{name} {}/{total}", d.logged),
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The local date `day` means on `today`. A weekday is its latest
/// occurrence in the last 7 days, the days `/jot week` lists.
pub fn resolve_day(day: JotDay, today: NaiveDate) -> NaiveDate {
    match day {
        JotDay::Today => today,
        JotDay::Yesterday => today - Duration::days(1),
        JotDay::Weekday(weekday) => (0..7)
            .map(|back| today - Duration::days(back))
            .find(|date| date.weekday() == weekday)
            .unwrap_or(today),
    }
}

fn label_of(date: NaiveDate, today: NaiveDate) -> String {
    if date == today {
        "Today".to_string()
    } else if date == today - Duration::days(1) {
        "Yesterday".to_string()
    } else {
        date.format("%a %-d %b").to_string()
    }
}

/// How long a day's summary stays due once its last prompt has had its
/// time. Long enough to cover a last slot late in the evening and the bot
/// being down overnight; short enough that a summary never turns up days
/// late.
const SUMMARY_DUE_FOR: Duration = Duration::hours(12);

/// The days whose end-of-day summary is due at `now`, oldest first: today
/// and yesterday, each once its last prompt has had `LATE_BY_AT_MOST` to be
/// answered. Yesterday is checked too because a last slot at 23:30 or later
/// only becomes due after local midnight. The claim keeps each to one send.
pub fn summary_days(now: DateTime<Utc>, timezone: Tz, settings: &JotSettings) -> Vec<NaiveDate> {
    let today = now.with_timezone(&timezone).date_naive();
    [today - Duration::days(1), today]
        .into_iter()
        .filter(|&day| {
            let slots = day_slots(day, timezone, settings);
            let Some(&last) = slots.last() else {
                return false;
            };
            let due = last + Duration::minutes(LATE_BY_AT_MOST);
            // Not for a day it was turned on part-way through after the
            // last slot.
            last >= settings.updated_at && now >= due && now < due + SUMMARY_DUE_FOR
        })
        .collect()
}

/// The UTC moments a local day starts and ends.
fn day_bounds(date: NaiveDate, timezone: Tz) -> (DateTime<Utc>, DateTime<Utc>) {
    let midnight = |d: NaiveDate| {
        local(d, NaiveTime::MIN, timezone).unwrap_or_else(|| d.and_time(NaiveTime::MIN).and_utc())
    };
    (midnight(date), midnight(date + Duration::days(1)))
}

/// The slots a day is shown and scored against: the prompts that were
/// actually sent, plus any slot an entry answers. Taken from what happened
/// rather than from the current settings, so `/jot off` or new hours leave
/// past days as they were, and today only counts the prompts sent so far.
pub fn shown_slots(prompts: &[DateTime<Utc>], entries: &[Entry]) -> Vec<DateTime<Utc>> {
    let mut slots: Vec<DateTime<Utc>> = prompts
        .iter()
        .copied()
        .chain(entries.iter().filter_map(|e| e.slot_at))
        .collect();
    slots.sort();
    slots.dedup();
    slots
}

/// One day's entries, the slots to show them against, and which of those
/// were skipped.
async fn day_view(
    conn: &mut SqliteConnection,
    user_id: i64,
    date: NaiveDate,
    timezone: Tz,
) -> Result<(Vec<Entry>, Vec<DateTime<Utc>>, Vec<DateTime<Utc>>), AppError> {
    let entries = entries_of(conn, user_id, date, date).await?;
    let (from, to) = day_bounds(date, timezone);
    let prompts = jot::prompts_between(conn, user_id, from, to).await?;
    let skipped = jot::skipped_between(conn, user_id, from, to).await?;
    let slots = shown_slots(&prompts, &entries);
    Ok((entries, slots, skipped))
}

fn interval_label(minutes: u32) -> String {
    if minutes.is_multiple_of(60) {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

/// `09:00–18:00`, then the days if they aren't every day.
fn hours_label(settings: &JotSettings) -> String {
    let hours = format!(
        "{}–{}",
        settings.start.format("%H:%M"),
        settings.end.format("%H:%M")
    );
    if settings.days == JotDays::EVERY_DAY {
        hours
    } else {
        format!("{hours}, {}", settings.days.label())
    }
}

/// `Next prompt: 11:00.`, after a pause if there's one.
fn next_line(settings: &JotSettings, now: DateTime<Utc>, mine: &Settings) -> String {
    let paused = settings.paused_at(now);
    let after = paused.map_or(now, |until| until.max(now));
    let pause = paused.map_or(String::new(), |until| {
        format!("Paused until {}. ", when_label(until, now, mine))
    });
    format!("{pause}{}", next_line_after(settings, after, now, mine))
}

/// `11:00`, or `Tue 3 Nov 11:00` if it isn't today.
fn when_label(at: DateTime<Utc>, now: DateTime<Utc>, mine: &Settings) -> String {
    if mine.today(at) == mine.today(now) {
        mine.clock(at)
    } else {
        format!(
            "{} {}",
            at.with_timezone(&mine.timezone).format("%a %-d %b"),
            mine.clock(at)
        )
    }
}

fn next_line_after(
    settings: &JotSettings,
    after: DateTime<Utc>,
    now: DateTime<Utc>,
    mine: &Settings,
) -> String {
    match next_prompt(after, mine.timezone, settings) {
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

async fn entries_of(
    conn: &mut SqliteConnection,
    user_id: i64,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<Entry>, AppError> {
    let rows = jot::entries_between(conn, user_id, from, to).await?;
    Ok(rows.into_iter().map(|(_, e)| e).collect())
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
            days,
        } => {
            // What isn't said stays as it was, or is the default.
            let interval_minutes = interval_minutes
                .or(current.map(|c| c.interval_minutes))
                .unwrap_or(DEFAULT_INTERVAL_MINUTES);
            let (start, end) = hours
                .or(current.map(|c| (c.start, c.end)))
                .unwrap_or((DEFAULT_START, DEFAULT_END));
            let days = days
                .or(current.map(|c| c.days))
                .unwrap_or(JotDays::EVERY_DAY);
            jot::turn_on(conn, user_id, interval_minutes, start, end, ctx.now).await?;
            jot::set_days(conn, user_id, days).await?;
            let now_on = JotSettings {
                enabled: true,
                interval_minutes,
                start,
                end,
                updated_at: ctx.now,
                days,
                paused_until: None,
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
        JotCommand::Pause(pause) => match current {
            Some(c) if c.enabled => {
                let until = match pause {
                    JotPause::For { minutes } => ctx.now + Duration::minutes(i64::from(minutes)),
                    // The start of the person's next day, so it resumes by
                    // itself in the morning.
                    JotPause::Today => day_bounds(mine.today(ctx.now), mine.timezone).1,
                };
                jot::pause_until(conn, user_id, Some(until)).await?;
                let paused = JotSettings {
                    paused_until: Some(until),
                    ..c
                };
                let how_long = match pause {
                    JotPause::Today => "for the rest of today".to_string(),
                    JotPause::For { .. } => format!("until {}", when_label(until, ctx.now, &mine)),
                };
                format!(
                    "{PROMPT} Jot is paused {how_long}. {}",
                    next_line_after(&paused, until, ctx.now, &mine)
                )
            }
            _ => {
                format!("{PROMPT} Jot is off, so there's nothing to pause. `/jot on` turns it on.")
            }
        },
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
        JotCommand::Day(day) => {
            let today = mine.today(ctx.now);
            let date = resolve_day(day, today);
            let (entries, slots, skipped) = day_view(conn, user_id, date, mine.timezone).await?;
            timeline(&label_of(date, today), &entries, &slots, &skipped, &mine)
        }
        JotCommand::Week => {
            let today = mine.today(ctx.now);
            let from = today - Duration::days(6);
            let all = jot::entries_between(conn, user_id, from, today).await?;
            let prompts = jot::prompts_between(
                conn,
                user_id,
                day_bounds(from, mine.timezone).0,
                day_bounds(today, mine.timezone).1,
            )
            .await?;
            let skipped_all = jot::skipped_between(
                conn,
                user_id,
                day_bounds(from, mine.timezone).0,
                day_bounds(today, mine.timezone).1,
            )
            .await?;
            let days: Vec<WeekDay> = (0..7)
                .map(|n| {
                    let date = from + Duration::days(n);
                    let entries: Vec<Entry> = all
                        .iter()
                        .filter(|(d, _)| *d == date)
                        .map(|(_, e)| e.clone())
                        .collect();
                    let sent: Vec<DateTime<Utc>> = prompts
                        .iter()
                        .copied()
                        .filter(|p| p.with_timezone(&mine.timezone).date_naive() == date)
                        .collect();
                    let slots = scored_slots(&shown_slots(&sent, &entries), &entries, &skipped_all);
                    WeekDay {
                        date,
                        logged: logged_count(&entries, &slots),
                        prompts: slots.len(),
                        entries: entries.len(),
                    }
                })
                .collect();
            if all.is_empty() {
                nothing_logged("This week")
            } else {
                format!(
                    "{PROMPT} This week\n{}\nSend `/jot mon` (or any day) to see that day.",
                    week_line(&days)
                )
            }
        }
        JotCommand::Log(text) => {
            // A prompt that's still open takes it, as if it were a reply.
            let open = jot::open_prompt(conn, ctx.chat_id, ctx.now).await?;
            jot::add_entry(conn, user_id, ctx.now, mine.today(ctx.now), open, &text).await?;
            match open {
                Some(slot_at) => format!("{DONE} Jotted for {}.", mine.clock(slot_at)),
                None => format!("{DONE} Jotted."),
            }
        }
        JotCommand::Skip { slot_at } => {
            // An answered prompt stays answered: `skip` doesn't hide an
            // entry, so say so rather than claim it was skipped.
            if jot::skip_prompt(conn, user_id, slot_at, ctx.now).await? {
                format!("{SKIP} Skipped {}.", mine.clock(slot_at))
            } else {
                format!(
                    "{PROBLEM} {} already has an entry, so I left it.",
                    mine.clock(slot_at)
                )
            }
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
            days: JotDays::EVERY_DAY,
            paused_until: None,
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
        let due = |s: &str| due_slot(utc(s), LONDON, &two_hourly(), &[]).map(|d| d.at);
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
        let first = due_slot(utc("2026-11-02T09:00:00Z"), LONDON, &two_hourly(), &[]).unwrap();
        assert_eq!(first.previous, None);
        let second = due_slot(utc("2026-11-02T11:00:00Z"), LONDON, &two_hourly(), &[]).unwrap();
        assert_eq!(second.previous, Some(utc("2026-11-02T09:00:00Z")));
    }

    #[test]
    fn the_prompt_reads_as_in_the_issue() {
        let settings = defaults();
        let first = due_slot(utc("2026-11-02T09:00:00Z"), LONDON, &two_hourly(), &[]).unwrap();
        assert_eq!(
            prompt_text(&first, &settings),
            "✏️ 09:00 · Jot time: what have you done since you started?"
        );
        let second = due_slot(utc("2026-11-02T11:00:00Z"), LONDON, &two_hourly(), &[]).unwrap();
        assert_eq!(
            prompt_text(&second, &settings),
            "✏️ 11:00 · Jot time: what have you done since 09:00?"
        );
        // In summer the same local times are an hour earlier in UTC.
        let summer = due_slot(utc("2026-07-01T10:00:00Z"), LONDON, &two_hourly(), &[]).unwrap();
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

    fn entry(slot: Option<&str>, logged: &str, text: &str) -> Entry {
        Entry {
            logged_at: utc(logged),
            slot_at: slot.map(utc),
            text: text.to_string(),
        }
    }

    #[test]
    fn the_timeline_marks_missed_slots_and_extras() {
        let slots = slots("2026-11-02");
        let entries = [
            entry(
                Some("2026-11-02T09:00:00Z"),
                "2026-11-02T09:05:00Z",
                "Emails and planning",
            ),
            entry(
                Some("2026-11-02T11:00:00Z"),
                "2026-11-02T11:02:00Z",
                "Finished the report",
            ),
            entry(
                Some("2026-11-02T15:00:00Z"),
                "2026-11-02T15:01:00Z",
                "School run",
            ),
            entry(
                Some("2026-11-02T15:00:00Z"),
                "2026-11-02T15:02:00Z",
                "groceries",
            ),
            entry(
                Some("2026-11-02T17:00:00Z"),
                "2026-11-02T17:01:00Z",
                "Cooked",
            ),
            entry(None, "2026-11-02T14:20:00Z", "Called the garage"),
        ];
        assert_eq!(
            timeline("Today", &entries, &slots, &[], &defaults()),
            "✏️ Today\n09:00 Emails and planning\n11:00 Finished the report\n13:00 —\n15:00 School run, groceries\n17:00 Cooked\n14:20 (extra) Called the garage\n4 of 5 logged"
        );
    }

    #[test]
    fn a_day_with_nothing_logged_is_a_kind_line() {
        let text = timeline("Today", &[], &slots("2026-11-02"), &[], &defaults());
        assert!(text.contains("nothing logged"), "{text}");
        assert!(!text.contains("13:00"), "{text}");
    }

    #[test]
    fn the_week_line_and_day_names() {
        let day = |d: &str, logged, prompts, entries| WeekDay {
            date: date(d),
            logged,
            prompts,
            entries,
        };
        assert_eq!(
            week_line(&[
                day("2026-11-02", 4, 5, 5),
                day("2026-11-03", 5, 5, 6),
                // Jot was off: only `+jot` entries, or nothing at all.
                day("2026-11-04", 0, 0, 2),
                day("2026-11-05", 0, 0, 0),
            ]),
            "Mon 4/5 · Tue 5/5 · Wed 2 · Thu —"
        );
        let today = date("2026-11-04"); // Wednesday
        assert_eq!(resolve_day(JotDay::Today, today), today);
        assert_eq!(resolve_day(JotDay::Yesterday, today), date("2026-11-03"));
        assert_eq!(
            resolve_day(JotDay::Weekday(chrono::Weekday::Wed), today),
            today
        );
        assert_eq!(
            resolve_day(JotDay::Weekday(chrono::Weekday::Thu), today),
            date("2026-10-29")
        );
    }

    #[test]
    fn the_summary_is_due_after_the_last_slot_has_had_its_time() {
        let due = |s: &str| summary_days(utc(s), LONDON, &two_hourly());
        assert_eq!(due("2026-11-02T17:29:00Z"), vec![]);
        assert_eq!(due("2026-11-02T17:30:00Z"), vec![date("2026-11-02")]);
        assert_eq!(due("2026-11-02T23:00:00Z"), vec![date("2026-11-02")]);
        // Still due just after midnight, then no longer once it's 12 hours
        // late; the next day's isn't due until its own last slot.
        assert_eq!(due("2026-11-03T01:00:00Z"), vec![date("2026-11-02")]);
        assert_eq!(due("2026-11-03T05:30:00Z"), vec![]);
        assert_eq!(due("2026-11-02T12:00:00Z"), vec![]);
        // Turned on after the last slot: no summary for that day.
        let late = JotSettings {
            updated_at: utc("2026-11-02T17:10:00Z"),
            ..two_hourly()
        };
        assert_eq!(
            summary_days(utc("2026-11-02T18:00:00Z"), LONDON, &late),
            vec![]
        );
    }

    #[test]
    fn a_last_slot_at_half_past_eleven_is_summed_up_after_midnight() {
        let night = JotSettings {
            interval_minutes: 30,
            start: at(22, 0),
            end: at(23, 59),
            ..two_hourly()
        };
        // London is on GMT in November, so local and UTC times match.
        let due = |s: &str| summary_days(utc(s), LONDON, &night);
        assert_eq!(due("2026-11-02T23:45:00Z"), vec![]);
        assert_eq!(due("2026-11-03T00:00:00Z"), vec![date("2026-11-02")]);
        assert_eq!(due("2026-11-03T00:30:00Z"), vec![date("2026-11-02")]);
    }

    #[test]
    fn a_day_is_scored_against_the_prompts_sent_and_the_slots_answered() {
        let nine = utc("2026-11-02T09:00:00Z");
        let eleven = utc("2026-11-02T11:00:00Z");
        let one = utc("2026-11-02T13:00:00Z");
        let entries = [
            entry(
                Some("2026-11-02T13:00:00Z"),
                "2026-11-02T13:05:00Z",
                "lunch",
            ),
            entry(None, "2026-11-02T14:00:00Z", "extra"),
        ];
        // The 13:00 prompt's send wasn't recorded, but it was answered.
        assert_eq!(
            shown_slots(&[eleven, nine, eleven], &entries),
            vec![nine, eleven, one]
        );
        assert_eq!(shown_slots(&[], &[]), Vec::<DateTime<Utc>>::new());
    }

    #[test]
    fn the_summary_plan_is_claimed_per_day_and_sent_to_the_dm() {
        let subscriber = Subscriber {
            user_id: 7,
            chat_id: 3,
            wa_chat_id: "447000000001@s.whatsapp.net".to_string(),
            display_name: "Bilal".to_string(),
            settings: two_hourly(),
        };
        let slots = slots("2026-11-02");
        let entries = [entry(
            Some("2026-11-02T09:00:00Z"),
            "2026-11-02T09:05:00Z",
            "emails",
        )];
        let planned = summary_plan(
            &subscriber,
            date("2026-11-02"),
            "Yesterday",
            &entries,
            &slots,
            &[],
            &defaults(),
        );
        assert!(matches!(
            planned.claim,
            Claim::JotSummary { user_id: 7, day } if day == date("2026-11-02")
        ));
        assert_eq!(planned.messages.len(), 1);
        assert_eq!(planned.messages[0].to, subscriber.wa_chat_id);
        let text = &planned.messages[0].text;
        assert!(text.starts_with("✏️ Yesterday\n09:00 emails"), "{text}");
        assert!(text.ends_with("1 of 5 logged"), "{text}");
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
        let slot = due_slot(on_at, LONDON, &subscriber.settings, &[]).unwrap();
        assert!(slot.at < subscriber.settings.updated_at);
        // `plan_subscriber` skips it for that reason; the plan itself is
        // for the 11:00 slot.
        let planned = plan(&subscriber, &slot, &defaults());
        assert_eq!(planned.messages.len(), 1);
        assert!(planned.messages[0].text.starts_with("✏️ 11:00 · Jot time"));
    }

    /// A prompt that went out: claimed, then accepted by WhatsApp.
    async fn sent_prompt(conn: &mut SqliteConnection, user: i64, dm: i64, slot: &str) {
        let slot = utc(slot);
        assert!(jot::claim_prompt(conn, user, dm, slot, slot).await.unwrap());
        jot::set_prompt_message(conn, user, slot, &format!("P{}", slot.timestamp()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn the_week_keeps_past_days_after_jot_is_turned_off() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T08:00:00Z")).await;
        let (user, dm) = (family.bilal.id, family.dm);
        jot::turn_on(
            &mut conn,
            user,
            120,
            at(9, 0),
            at(13, 0),
            utc("2026-11-01T08:00:00Z"),
        )
        .await
        .unwrap();
        // Monday: both prompts sent, one answered.
        sent_prompt(&mut conn, user, dm, "2026-11-02T09:00:00Z").await;
        sent_prompt(&mut conn, user, dm, "2026-11-02T11:00:00Z").await;
        jot::add_entry(
            &mut conn,
            user,
            utc("2026-11-02T09:10:00Z"),
            date("2026-11-02"),
            Some(utc("2026-11-02T09:00:00Z")),
            "emails",
        )
        .await
        .unwrap();
        jot::turn_off(&mut conn, user).await.unwrap();
        // Tuesday, with Jot off: a `+jot` only.
        jot::add_entry(
            &mut conn,
            user,
            utc("2026-11-03T15:00:00Z"),
            date("2026-11-03"),
            None,
            "called the garage",
        )
        .await
        .unwrap();

        let ctx = ctx(dm, false, &family.bilal, "2026-11-04T10:00:00Z");
        let week = command(&mut conn, &ctx, &defaults(), command_of("/jot week"))
            .await
            .unwrap();
        assert!(week.contains("Mon 1/2 · Tue 1 · Wed —"), "{week}");
        assert!(!week.contains("0/0"), "{week}");

        let monday = command(&mut conn, &ctx, &defaults(), command_of("/jot mon"))
            .await
            .unwrap();
        assert!(monday.contains("09:00 emails\n11:00 —"), "{monday}");
        assert!(monday.ends_with("1 of 2 logged"), "{monday}");
    }

    #[tokio::test]
    async fn the_summary_after_midnight_goes_once_then_is_skipped() {
        let pool = test_pool().await;
        let state = crate::testkit::test_state(pool.clone(), "http://127.0.0.1:9");
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T08:00:00Z")).await;
        let (user, dm) = (family.bilal.id, family.dm);
        jot::turn_on(
            &mut conn,
            user,
            30,
            at(22, 0),
            at(23, 59),
            utc("2026-11-01T08:00:00Z"),
        )
        .await
        .unwrap();
        sent_prompt(&mut conn, user, dm, "2026-11-02T23:30:00Z").await;
        jot::add_entry(
            &mut conn,
            user,
            utc("2026-11-02T23:40:00Z"),
            date("2026-11-02"),
            Some(utc("2026-11-02T23:30:00Z")),
            "read a chapter",
        )
        .await
        .unwrap();
        let subscriber = jot::subscribers(&mut conn).await.unwrap().remove(0);
        // `plan_subscriber` takes its own connection from the pool.
        drop(conn);

        let now = utc("2026-11-03T00:05:00Z");
        let planned = plan_subscriber(&state, &subscriber, now)
            .await
            .unwrap()
            .expect("yesterday's summary");
        assert!(matches!(
            planned.claim,
            Claim::JotSummary { day, .. } if day == date("2026-11-02")
        ));
        let text = &planned.messages[0].text;
        assert!(text.starts_with("✏️ Yesterday"), "{text}");
        assert!(text.contains("23:30 read a chapter"), "{text}");

        // Once claimed, it isn't built again.
        {
            let mut conn = pool.acquire().await.unwrap();
            assert!(
                jot::claim_summary(&mut conn, user, date("2026-11-02"), now)
                    .await
                    .unwrap()
            );
        }
        let later = utc("2026-11-03T00:06:00Z");
        assert!(
            plan_subscriber(&state, &subscriber, later)
                .await
                .unwrap()
                .is_none()
        );
    }

    // ---- J3: smarter prompts ----

    /// A Monday in November (GMT) whose Dhuhr is pretended to start at
    /// 10:50, so the 11:00 slot lands just after a prayer time.
    fn monday_prayers() -> crate::prayer::DayTimes {
        use crate::prayer::{DayTimes, Prayer, Window};
        let window = |prayer, start: &str, end: &str| Window {
            prayer,
            start: utc(start),
            end: utc(end),
        };
        DayTimes {
            date: date("2026-11-02"),
            hijri: None,
            windows: [
                window(Prayer::Fajr, "2026-11-02T05:30:00Z", "2026-11-02T07:15:00Z"),
                window(
                    Prayer::Dhuhr,
                    "2026-11-02T10:50:00Z",
                    "2026-11-02T14:00:00Z",
                ),
                window(Prayer::Asr, "2026-11-02T14:00:00Z", "2026-11-02T16:30:00Z"),
                window(
                    Prayer::Maghrib,
                    "2026-11-02T16:30:00Z",
                    "2026-11-02T18:00:00Z",
                ),
                window(Prayer::Isha, "2026-11-02T18:00:00Z", "2026-11-03T00:00:00Z"),
            ],
        }
    }

    /// A state whose prayer times for 2 November are already known.
    fn state_with_prayers(pool: sqlx::SqlitePool) -> std::sync::Arc<AppState> {
        let mut state = crate::testkit::test_state(pool, "http://127.0.0.1:9");
        std::sync::Arc::get_mut(&mut state)
            .expect("not shared yet")
            .apis
            .prayer_times =
            crate::aladhan::PrayerTimes::with_cached_days(defaults(), vec![monday_prayers()]);
        state
    }

    async fn subscriber(conn: &mut SqliteConnection, user: i64) -> Subscriber {
        jot::turn_on(
            conn,
            user,
            120,
            at(9, 0),
            at(18, 0),
            utc("2026-11-01T08:00:00Z"),
        )
        .await
        .unwrap();
        jot::subscribers(conn).await.unwrap().remove(0)
    }

    #[test]
    fn a_prompt_just_after_a_prayer_moves_to_fifteen_minutes_after_it() {
        let dhuhr = utc("2026-11-02T10:50:00Z");
        let eleven = utc("2026-11-02T11:00:00Z");
        assert_eq!(after_prayers(eleven, &[dhuhr]), utc("2026-11-02T11:05:00Z"));
        // At the prayer's own start, and one minute before 15 have passed.
        assert_eq!(after_prayers(dhuhr, &[dhuhr]), utc("2026-11-02T11:05:00Z"));
        assert_eq!(
            after_prayers(utc("2026-11-02T11:04:00Z"), &[dhuhr]),
            utc("2026-11-02T11:05:00Z")
        );
        // 15 minutes after, before it, or no prayer times: unchanged.
        let later = utc("2026-11-02T11:05:00Z");
        assert_eq!(after_prayers(later, &[dhuhr]), later);
        assert_eq!(after_prayers(dhuhr, &[eleven]), dhuhr);
        assert_eq!(after_prayers(eleven, &[]), eleven);
    }

    #[test]
    fn a_moved_prompt_is_due_from_its_new_time_for_half_an_hour() {
        let starts = [utc("2026-11-02T10:50:00Z")];
        let due = |s: &str| due_slot(utc(s), LONDON, &two_hourly(), &starts);
        assert_eq!(due("2026-11-02T11:00:00Z"), None);
        assert_eq!(due("2026-11-02T11:04:00Z"), None);
        let moved = due("2026-11-02T11:05:00Z").unwrap();
        assert_eq!(
            moved.at,
            utc("2026-11-02T11:00:00Z"),
            "still the 11:00 slot"
        );
        assert_eq!(moved.send_at, utc("2026-11-02T11:05:00Z"));
        assert!(due("2026-11-02T11:34:00Z").is_some());
        assert_eq!(due("2026-11-02T11:35:00Z"), None);
        // Other slots aren't touched.
        assert!(due("2026-11-02T13:00:00Z").is_some());
    }

    #[tokio::test]
    async fn the_loop_holds_a_prompt_back_after_a_prayer_with_a_fake_clock() {
        let pool = test_pool().await;
        let state = state_with_prayers(pool.clone());
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T08:00:00Z")).await;
        let subscriber = subscriber(&mut conn, family.bilal.id).await;
        drop(conn);

        let plan_at = |s: &str| {
            let state = state.clone();
            let subscriber = subscriber.clone();
            let now = utc(s);
            async move { plan_subscriber(&state, &subscriber, now).await.unwrap() }
        };
        assert!(plan_at("2026-11-02T11:00:00Z").await.is_none());
        assert!(plan_at("2026-11-02T11:04:00Z").await.is_none());
        let planned = plan_at("2026-11-02T11:05:00Z").await.expect("moved prompt");
        assert!(planned.messages[0].text.starts_with("✏️ 11:00 · Jot time"));
        assert!(matches!(
            planned.claim,
            Claim::JotPrompt { slot_at, .. } if slot_at == utc("2026-11-02T11:00:00Z")
        ));
    }

    #[tokio::test]
    async fn a_recent_entry_skips_the_prompt_and_fills_the_slot() {
        let pool = test_pool().await;
        let state = state_with_prayers(pool.clone());
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T08:00:00Z")).await;
        let user = family.bilal.id;
        let subscriber = subscriber(&mut conn, user).await;
        // 13:00 has an entry from 12:20, 40 minutes before: too old.
        jot::add_entry(
            &mut conn,
            user,
            utc("2026-11-02T12:20:00Z"),
            date("2026-11-02"),
            None,
            "an old one",
        )
        .await
        .unwrap();
        jot::add_entry(
            &mut conn,
            user,
            utc("2026-11-02T10:45:00Z"),
            date("2026-11-02"),
            None,
            "called the garage",
        )
        .await
        .unwrap();
        drop(conn);

        // 10:45 is within 30 minutes of the 11:00 slot (sent at 11:05).
        for now in ["2026-11-02T11:05:00Z", "2026-11-02T11:06:00Z"] {
            let found = plan_subscriber(&state, &subscriber, utc(now))
                .await
                .unwrap();
            assert!(found.is_none(), "no prompt at {now}");
        }
        let mut conn = pool.acquire().await.unwrap();
        let ctx = ctx(family.dm, false, &family.bilal, "2026-11-02T11:10:00Z");
        let today = command(&mut conn, &ctx, &defaults(), command_of("/jot today"))
            .await
            .unwrap();
        assert!(today.contains("11:00 called the garage"), "{today}");
        drop(conn);

        // The 13:00 slot has nothing recent (12:20 is 40 minutes before).
        let planned = plan_subscriber(&state, &subscriber, utc("2026-11-02T13:00:00Z"))
            .await
            .unwrap();
        assert!(planned.is_some(), "an old entry doesn't skip a prompt");
    }

    #[tokio::test]
    async fn plus_jot_answers_an_open_prompt_and_is_an_extra_otherwise() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-02T08:00:00Z")).await;
        let (user, dm) = (family.bilal.id, family.dm);
        sent_prompt(&mut conn, user, dm, "2026-11-02T11:00:00Z").await;

        let ctx = ctx(dm, false, &family.bilal, "2026-11-02T11:10:00Z");
        let said = command(
            &mut conn,
            &ctx,
            &defaults(),
            command_of("+j wrote the report"),
        )
        .await
        .unwrap();
        assert_eq!(said, format!("{DONE} Jotted for 11:00."));
        // The slot is answered now, so the next one is an extra.
        let said = command(&mut conn, &ctx, &defaults(), command_of("+j and emails"))
            .await
            .unwrap();
        assert_eq!(said, format!("{DONE} Jotted."));

        let slots: Vec<Option<String>> =
            sqlx::query_scalar("SELECT slot_at FROM jot_entries ORDER BY id")
                .fetch_all(&mut *conn)
                .await
                .unwrap();
        assert!(slots[0].is_some());
        assert_eq!(slots[1], None);
    }

    #[tokio::test]
    async fn skip_is_shown_as_skipped_and_counts_as_neither_logged_nor_missed() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-02T08:00:00Z")).await;
        let (user, dm) = (family.bilal.id, family.dm);
        sent_prompt(&mut conn, user, dm, "2026-11-02T09:00:00Z").await;
        sent_prompt(&mut conn, user, dm, "2026-11-02T11:00:00Z").await;
        sent_prompt(&mut conn, user, dm, "2026-11-02T13:00:00Z").await;
        let nine = ctx(dm, false, &family.bilal, "2026-11-02T09:05:00Z");
        command(&mut conn, &nine, &defaults(), command_of("/jot")) // warm-up, harmless
            .await
            .unwrap();
        let reply = JotCommand::Reply {
            text: "emails".to_string(),
            slot_at: utc("2026-11-02T09:00:00Z"),
        };
        command(&mut conn, &nine, &defaults(), reply).await.unwrap();

        let eleven = ctx(dm, false, &family.bilal, "2026-11-02T11:05:00Z");
        let skip = JotCommand::Skip {
            slot_at: utc("2026-11-02T11:00:00Z"),
        };
        let said = command(&mut conn, &eleven, &defaults(), skip)
            .await
            .unwrap();
        assert_eq!(said, "😴 Skipped 11:00.");

        // A skipped prompt isn't open any more.
        assert_eq!(
            jot::open_prompt(&mut conn, dm, utc("2026-11-02T11:06:00Z"))
                .await
                .unwrap(),
            None
        );
        let later = ctx(dm, false, &family.bilal, "2026-11-02T13:10:00Z");
        let today = command(&mut conn, &later, &defaults(), command_of("/jot today"))
            .await
            .unwrap();
        assert!(
            today.contains("09:00 emails\n11:00 skipped\n13:00 —"),
            "{today}"
        );
        // 2 scored: 09:00 logged, 13:00 missed; the skipped one isn't.
        assert!(today.ends_with("1 of 2 logged"), "{today}");

        let week = command(&mut conn, &later, &defaults(), command_of("/jot week"))
            .await
            .unwrap();
        assert!(week.contains("Mon 1/2"), "{week}");

        // Skipping the answered 09:00 changes nothing and says so.
        let skip_answered = JotCommand::Skip {
            slot_at: utc("2026-11-02T09:00:00Z"),
        };
        let said = command(&mut conn, &later, &defaults(), skip_answered)
            .await
            .unwrap();
        assert_eq!(said, "❓ 09:00 already has an entry, so I left it.");
        let today = command(&mut conn, &later, &defaults(), command_of("/jot today"))
            .await
            .unwrap();
        assert!(today.contains("09:00 emails\n11:00 skipped"), "{today}");
        assert!(today.ends_with("1 of 2 logged"), "{today}");
    }

    #[test]
    fn a_day_of_only_skips_is_not_called_empty() {
        let slots = [utc("2026-11-02T11:00:00Z")];
        let text = timeline("Today", &[], &slots, &slots, &defaults());
        assert_eq!(text, "✏️ Today\n11:00 skipped");
    }

    #[tokio::test]
    async fn days_leave_out_the_weekend_and_say_so() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-02T08:00:00Z")).await;
        let ctx = ctx(family.dm, false, &family.bilal, "2026-11-06T17:30:00Z");
        let on = command(
            &mut conn,
            &ctx,
            &defaults(),
            command_of("/jot on 2h 09:00-18:00 -days mon-fri"),
        )
        .await
        .unwrap();
        assert!(on.contains("every 2h, 09:00–18:00, Mon–Fri"), "{on}");
        // Friday evening: the next prompt is Monday's.
        assert!(on.contains("Next prompt: 09:00 on Mon 9 Nov."), "{on}");
        let settings = jot::get(&mut conn, family.bilal.id).await.unwrap().unwrap();

        let due = |s: &str| due_slot(utc(s), LONDON, &settings, &[]);
        assert!(due("2026-11-06T11:00:00Z").is_some(), "Friday");
        assert!(due("2026-11-07T11:00:00Z").is_none(), "Saturday");
        assert!(due("2026-11-08T11:00:00Z").is_none(), "Sunday");
        assert!(due("2026-11-09T11:00:00Z").is_some(), "Monday");
        assert!(summary_days(utc("2026-11-07T23:00:00Z"), LONDON, &settings).is_empty());

        // Saying nothing about days later keeps them.
        command(&mut conn, &ctx, &defaults(), command_of("/jot on 3h"))
            .await
            .unwrap();
        let kept = jot::get(&mut conn, family.bilal.id).await.unwrap().unwrap();
        assert_eq!(kept.days, settings.days);
        command(
            &mut conn,
            &ctx,
            &defaults(),
            command_of("/jot on -days daily"),
        )
        .await
        .unwrap();
        let all = jot::get(&mut conn, family.bilal.id).await.unwrap().unwrap();
        assert_eq!(all.days, JotDays::EVERY_DAY);
    }

    #[tokio::test]
    async fn a_pause_stops_prompts_and_ends_by_itself() {
        let pool = test_pool().await;
        let state = state_with_prayers(pool.clone());
        let mut conn = pool.acquire().await.unwrap();
        let family = seed_family(&mut conn, utc("2026-11-01T08:00:00Z")).await;
        let user = family.bilal.id;
        let _ = subscriber(&mut conn, user).await;
        let ctx = ctx(family.dm, false, &family.bilal, "2026-11-02T10:00:00Z");

        let said = command(&mut conn, &ctx, &defaults(), command_of("/jot pause 3h"))
            .await
            .unwrap();
        assert!(said.contains("paused until 13:00"), "{said}");
        let paused = jot::subscribers(&mut conn).await.unwrap().remove(0);
        // 11:00 (moved to 11:05) is inside the pause; 13:00 is where it ends.
        drop(conn);
        for now in ["2026-11-02T11:05:00Z", "2026-11-02T12:59:00Z"] {
            assert!(
                plan_subscriber(&state, &paused, utc(now))
                    .await
                    .unwrap()
                    .is_none(),
                "paused at {now}"
            );
        }
        let after = plan_subscriber(&state, &paused, utc("2026-11-02T13:00:00Z"))
            .await
            .unwrap();
        assert!(after.is_some(), "the pause is over");

        // `today` runs to the person's midnight, then Tuesday's prompts go.
        let mut conn = pool.acquire().await.unwrap();
        let said = command(&mut conn, &ctx, &defaults(), command_of("/jot pause today"))
            .await
            .unwrap();
        assert!(said.contains("paused for the rest of today"), "{said}");
        assert!(said.contains("Next prompt: 09:00 on Tue 3 Nov."), "{said}");
        let paused = jot::subscribers(&mut conn).await.unwrap().remove(0);
        drop(conn);
        assert!(
            plan_subscriber(&state, &paused, utc("2026-11-02T15:00:00Z"))
                .await
                .unwrap()
                .is_none()
        );
        let _ = pool.acquire().await.unwrap();
        // Tuesday has no cached prayer times, so prompts go on time.
        let next_day = plan_subscriber(&state, &paused, utc("2026-11-03T09:00:00Z"))
            .await
            .unwrap();
        assert!(next_day.is_some(), "paused only for today");

        let off = {
            let mut conn = pool.acquire().await.unwrap();
            jot::turn_off(&mut conn, user).await.unwrap();
            command(&mut conn, &ctx, &defaults(), command_of("/jot pause 1h"))
                .await
                .unwrap()
        };
        assert!(off.contains("nothing to pause"), "{off}");
    }
}

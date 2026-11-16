//! Jot: each person's settings, the prompts sent and what they jotted down.

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Utc};
use sqlx::SqliteConnection;

use crate::parser::JotDays;
use crate::reports::Period;

/// The text form of a clock time in the database ("09:00"): zero-padded, so
/// the schema can compare two of them as text.
const CLOCK: &str = "%H:%M";

/// Someone's Jot settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JotSettings {
    pub enabled: bool,
    pub interval_minutes: u32,
    /// Local clock times, in the person's own timezone.
    pub start: NaiveTime,
    pub end: NaiveTime,
    /// When it was last turned on or changed.
    pub updated_at: DateTime<Utc>,
    /// The weekdays it prompts on.
    pub days: JotDays,
    /// No prompts before this. Past or `None` means not paused.
    pub paused_until: Option<DateTime<Utc>>,
}

impl JotSettings {
    /// Whether a pause is still on at `now`.
    pub fn paused_at(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.paused_until.filter(|until| *until > now)
    }
}

fn decode_days(bits: i64) -> Result<JotDays, sqlx::Error> {
    JotDays::from_bits(bits)
        .ok_or_else(|| sqlx::Error::Decode(format!("bad jot days {bits}").into()))
}

fn decode_time(text: &str) -> Result<NaiveTime, sqlx::Error> {
    NaiveTime::parse_from_str(text, CLOCK).map_err(|err| sqlx::Error::Decode(Box::new(err)))
}

fn decode_interval(minutes: i64) -> Result<u32, sqlx::Error> {
    u32::try_from(minutes).map_err(|err| sqlx::Error::Decode(Box::new(err)))
}

/// This person's settings, if they've ever turned Jot on.
pub async fn get(
    conn: &mut SqliteConnection,
    user_id: i64,
) -> Result<Option<JotSettings>, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT enabled AS "enabled: bool", interval_minutes, start_local, end_local,
                  updated_at AS "updated_at: DateTime<Utc>", days,
                  paused_until AS "paused_until: DateTime<Utc>"
           FROM jot_settings WHERE user_id = ?"#,
        user_id
    )
    .fetch_optional(conn)
    .await?;
    row.map(|r| {
        Ok(JotSettings {
            enabled: r.enabled,
            interval_minutes: decode_interval(r.interval_minutes)?,
            start: decode_time(&r.start_local)?,
            end: decode_time(&r.end_local)?,
            updated_at: r.updated_at,
            days: decode_days(r.days)?,
            paused_until: r.paused_until,
        })
    })
    .transpose()
}

/// Turns Jot on with these settings (or changes them). `now` is kept, so no
/// prompt from before it is sent.
pub async fn turn_on(
    conn: &mut SqliteConnection,
    user_id: i64,
    interval_minutes: u32,
    start: NaiveTime,
    end: NaiveTime,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let (start, end) = (
        start.format(CLOCK).to_string(),
        end.format(CLOCK).to_string(),
    );
    sqlx::query!(
        "INSERT INTO jot_settings (user_id, enabled, interval_minutes, start_local, end_local, updated_at)
         VALUES (?, 1, ?, ?, ?, ?)
         ON CONFLICT (user_id) DO UPDATE SET
             enabled = 1,
             interval_minutes = excluded.interval_minutes,
             start_local = excluded.start_local,
             end_local = excluded.end_local,
             updated_at = excluded.updated_at,
             paused_until = NULL",
        user_id,
        interval_minutes,
        start,
        end,
        now
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Sets the weekdays prompts go out on.
pub async fn set_days(
    conn: &mut SqliteConnection,
    user_id: i64,
    days: JotDays,
) -> Result<(), sqlx::Error> {
    let bits = i64::from(days.bits());
    sqlx::query!(
        "UPDATE jot_settings SET days = ? WHERE user_id = ?",
        bits,
        user_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// No prompts until `until`; `None` ends a pause.
pub async fn pause_until(
    conn: &mut SqliteConnection,
    user_id: i64,
    until: Option<DateTime<Utc>>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE jot_settings SET paused_until = ? WHERE user_id = ?",
        until,
        user_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Turns Jot off, keeping the interval and hours for next time.
pub async fn turn_off(conn: &mut SqliteConnection, user_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE jot_settings SET enabled = 0 WHERE user_id = ?",
        user_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Someone with Jot on, and the DM their prompts go to.
#[derive(Debug, Clone, PartialEq)]
pub struct Subscriber {
    pub user_id: i64,
    pub chat_id: i64,
    pub wa_chat_id: String,
    pub display_name: String,
    pub settings: JotSettings,
}

/// Everyone with Jot on whose DM the bot is still in.
pub async fn subscribers(conn: &mut SqliteConnection) -> Result<Vec<Subscriber>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT u.id AS user_id, u.display_name, c.id AS chat_id, c.wa_chat_id,
                  s.interval_minutes, s.start_local, s.end_local,
                  s.updated_at AS "updated_at: DateTime<Utc>", s.days,
                  s.paused_until AS "paused_until: DateTime<Utc>"
           FROM jot_settings s
           JOIN users u ON u.id = s.user_id
           JOIN chats c ON c.dm_user_id = u.id AND c.left_at IS NULL
           WHERE s.enabled = 1"#
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|r| {
            Ok(Subscriber {
                user_id: r.user_id,
                chat_id: r.chat_id,
                wa_chat_id: r.wa_chat_id,
                display_name: r.display_name,
                settings: JotSettings {
                    enabled: true,
                    interval_minutes: decode_interval(r.interval_minutes)?,
                    start: decode_time(&r.start_local)?,
                    end: decode_time(&r.end_local)?,
                    updated_at: r.updated_at,
                    days: decode_days(r.days)?,
                    paused_until: r.paused_until,
                },
            })
        })
        .collect()
}

/// Claims the prompt for `slot_at`. `true` the first time only, so it's sent
/// once even if the loop runs again or restarts.
pub async fn claim_prompt(
    conn: &mut SqliteConnection,
    user_id: i64,
    chat_id: i64,
    slot_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "INSERT INTO jot_prompts (user_id, chat_id, slot_at, claimed_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT (user_id, slot_at) DO NOTHING",
        user_id,
        chat_id,
        slot_at,
        now
    )
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Remembers the WhatsApp ID of a sent prompt, so a reply quoting it can be
/// matched to its slot.
pub async fn set_prompt_message(
    conn: &mut SqliteConnection,
    user_id: i64,
    slot_at: DateTime<Utc>,
    wa_message_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE jot_prompts SET wa_message_id = ? WHERE user_id = ? AND slot_at = ?",
        wa_message_id,
        user_id,
        slot_at
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The slot of the prompt a message quoted, if it is one of the prompts
/// sent in this chat.
pub async fn slot_of_message(
    conn: &mut SqliteConnection,
    wa_message_id: &str,
    chat_id: i64,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT slot_at AS "slot_at: DateTime<Utc>"
           FROM jot_prompts WHERE wa_message_id = ? AND chat_id = ?"#,
        wa_message_id,
        chat_id
    )
    .fetch_optional(conn)
    .await
}

/// How long after a prompt a plain message still answers it.
pub const OPEN_FOR: Duration = Duration::minutes(30);

/// The newest prompt in this chat that was sent within `OPEN_FOR` of `now`
/// and has no answer yet: what a plain message is taken to be a reply to.
pub async fn open_prompt(
    conn: &mut SqliteConnection,
    chat_id: i64,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    let since = now - OPEN_FOR;
    sqlx::query_scalar!(
        r#"SELECT p.slot_at AS "slot_at: DateTime<Utc>"
           FROM jot_prompts p
           WHERE p.chat_id = ?
             AND p.wa_message_id IS NOT NULL
             AND p.skipped_at IS NULL
             AND datetime(p.claimed_at) > datetime(?)
             AND datetime(p.claimed_at) <= datetime(?)
             AND NOT EXISTS (SELECT 1 FROM jot_entries e
                             WHERE e.user_id = p.user_id AND e.slot_at = p.slot_at)
           ORDER BY datetime(p.claimed_at) DESC
           LIMIT 1"#,
        chat_id,
        since,
        now
    )
    .fetch_optional(conn)
    .await
}

/// Marks the prompt for `slot_at` as skipped, unless it was answered.
pub async fn skip_prompt(
    conn: &mut SqliteConnection,
    user_id: i64,
    slot_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "UPDATE jot_prompts SET skipped_at = ?
         WHERE user_id = ? AND slot_at = ?
           AND NOT EXISTS (SELECT 1 FROM jot_entries e
                           WHERE e.user_id = jot_prompts.user_id AND e.slot_at = jot_prompts.slot_at)",
        now,
        user_id,
        slot_at
    )
    .execute(conn)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// The sent prompts in `from..to` (UTC) that were skipped and still have no
/// entry.
pub async fn skipped_between(
    conn: &mut SqliteConnection,
    user_id: i64,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<DateTime<Utc>>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT p.slot_at AS "slot_at: DateTime<Utc>"
           FROM jot_prompts p
           WHERE p.user_id = ? AND p.wa_message_id IS NOT NULL AND p.skipped_at IS NOT NULL
             AND datetime(p.slot_at) >= datetime(?) AND datetime(p.slot_at) < datetime(?)
           ORDER BY datetime(p.slot_at)"#,
        user_id,
        from,
        to
    )
    .fetch_all(conn)
    .await
}

/// Whether a prompt for `slot_at` was ever claimed, so one that's been sent
/// is never also filled by an earlier entry.
pub async fn prompt_claimed(
    conn: &mut SqliteConnection,
    user_id: i64,
    slot_at: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let found = sqlx::query_scalar!(
        "SELECT 1 FROM jot_prompts WHERE user_id = ? AND slot_at = ?",
        user_id,
        slot_at
    )
    .fetch_optional(conn)
    .await?;
    Ok(found.is_some())
}

/// What `recent_entry` found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecentEntry {
    pub id: i64,
    pub slot_at: Option<DateTime<Utc>>,
}

/// The newest entry logged after `since` and up to `now`, if any.
pub async fn recent_entry(
    conn: &mut SqliteConnection,
    user_id: i64,
    since: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Option<RecentEntry>, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT id, slot_at AS "slot_at: DateTime<Utc>"
           FROM jot_entries
           WHERE user_id = ? AND datetime(logged_at) > datetime(?) AND datetime(logged_at) <= datetime(?)
           ORDER BY datetime(logged_at) DESC, id DESC
           LIMIT 1"#,
        user_id,
        since,
        now
    )
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|r| RecentEntry {
        id: r.id,
        slot_at: r.slot_at,
    }))
}

/// Makes a `+jot` entry the answer to `slot_at`. Returns whether it did:
/// an entry that already answers another slot is left there.
pub async fn fill_slot(
    conn: &mut SqliteConnection,
    entry_id: i64,
    slot_at: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "UPDATE jot_entries SET slot_at = ? WHERE id = ? AND slot_at IS NULL",
        slot_at,
        entry_id
    )
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Saves what someone jotted down. `slot_at` is the prompt it answers, or
/// `None` for a `+jot`.
pub async fn add_entry(
    conn: &mut SqliteConnection,
    user_id: i64,
    logged_at: DateTime<Utc>,
    day: NaiveDate,
    slot_at: Option<DateTime<Utc>>,
    text: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO jot_entries (user_id, logged_at, day, slot_at, text)
         VALUES (?, ?, ?, ?, ?)",
        user_id,
        logged_at,
        day,
        slot_at,
        text
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// One saved entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub logged_at: DateTime<Utc>,
    pub slot_at: Option<DateTime<Utc>>,
    pub text: String,
}

/// Everything someone jotted on the local days `from..=to`, oldest first.
pub async fn entries_between(
    conn: &mut SqliteConnection,
    user_id: i64,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<(NaiveDate, Entry)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT day AS "day: NaiveDate", logged_at AS "logged_at: DateTime<Utc>",
                  slot_at AS "slot_at: DateTime<Utc>", text
           FROM jot_entries
           WHERE user_id = ? AND day >= ? AND day <= ?
           ORDER BY datetime(logged_at), id"#,
        user_id,
        from,
        to
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.day,
                Entry {
                    logged_at: r.logged_at,
                    slot_at: r.slot_at,
                    text: r.text,
                },
            )
        })
        .collect())
}

/// Claims the end-of-day summary for `day`. `true` the first time only.
pub async fn claim_summary(
    conn: &mut SqliteConnection,
    user_id: i64,
    day: NaiveDate,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "INSERT INTO jot_summaries (user_id, day, claimed_at) VALUES (?, ?, ?)
         ON CONFLICT (user_id, day) DO NOTHING",
        user_id,
        day,
        now
    )
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Whether the end-of-day summary for `day` has already been claimed, so
/// the loop can skip building it again every minute until it stops being
/// due.
pub async fn summary_claimed(
    conn: &mut SqliteConnection,
    user_id: i64,
    day: NaiveDate,
) -> Result<bool, sqlx::Error> {
    let found = sqlx::query_scalar!(
        "SELECT 1 FROM jot_summaries WHERE user_id = ? AND day = ?",
        user_id,
        day
    )
    .fetch_optional(conn)
    .await?;
    Ok(found.is_some())
}

/// How many slots someone answered on the local days `from..=to`: a slot
/// with several entries counts once, and `+jot` extras not at all.
pub async fn logged_slots_between(
    conn: &mut SqliteConnection,
    user_id: i64,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT COUNT(DISTINCT slot_at) AS "logged!: i64" FROM jot_entries
           WHERE user_id = ? AND slot_at IS NOT NULL AND day >= ? AND day <= ?"#,
        user_id,
        from,
        to
    )
    .fetch_one(conn)
    .await
}

/// The days someone was sent (or at least due) an end-of-day summary, on or
/// after `since`, newest first: the days a full-day streak is counted over.
pub async fn summary_days_since(
    conn: &mut SqliteConnection,
    user_id: i64,
    since: NaiveDate,
) -> Result<Vec<NaiveDate>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT day AS "day: NaiveDate" FROM jot_summaries
           WHERE user_id = ? AND day >= ?
           ORDER BY day DESC"#,
        user_id,
        since
    )
    .fetch_all(conn)
    .await
}

/// What the admin dashboard may know about someone's Jot: counts only,
/// never what they wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JotCounts {
    pub on: bool,
    pub entries_this_week: i64,
    pub entries_last_week: i64,
}

/// `JotCounts` for `user_id`, the weeks being their local days.
pub async fn counts(
    conn: &mut SqliteConnection,
    user_id: i64,
    this_week: Period,
    last_week: Period,
) -> Result<JotCounts, sqlx::Error> {
    sqlx::query_as!(
        JotCounts,
        r#"SELECT
             EXISTS (SELECT 1 FROM jot_settings WHERE user_id = ?1 AND enabled = 1) AS "on!: bool",
             (SELECT COUNT(*) FROM jot_entries
              WHERE user_id = ?1 AND day >= ?2 AND day <= ?3) AS "entries_this_week!: i64",
             (SELECT COUNT(*) FROM jot_entries
              WHERE user_id = ?1 AND day >= ?4 AND day <= ?5) AS "entries_last_week!: i64""#,
        user_id,
        this_week.first,
        this_week.last,
        last_week.first,
        last_week.last
    )
    .fetch_one(conn)
    .await
}

/// The prompts actually sent to someone in `from..to` (UTC), oldest first.
/// A past day is scored against these, not against today's settings, so
/// `/jot off` or new hours don't rewrite its totals. A claim WhatsApp never
/// accepted wasn't seen, so it doesn't count.
pub async fn prompts_between(
    conn: &mut SqliteConnection,
    user_id: i64,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<DateTime<Utc>>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT slot_at AS "slot_at: DateTime<Utc>"
           FROM jot_prompts
           WHERE user_id = ? AND wa_message_id IS NOT NULL
             AND datetime(slot_at) >= datetime(?) AND datetime(slot_at) < datetime(?)
           ORDER BY datetime(slot_at)"#,
        user_id,
        from,
        to
    )
    .fetch_all(conn)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_pool;
    use crate::testkit::{date, seed_family, utc};

    fn at(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[tokio::test]
    async fn off_until_turned_on_and_remembers_its_settings_after_off() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = utc("2026-11-16T10:00:00Z");
        let family = seed_family(&mut conn, now).await;
        let user = family.bilal.id;

        assert_eq!(get(&mut conn, user).await.unwrap(), None);
        assert!(subscribers(&mut conn).await.unwrap().is_empty());

        turn_on(&mut conn, user, 120, at(9, 0), at(18, 0), now)
            .await
            .unwrap();
        let found = subscribers(&mut conn).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].chat_id, family.dm);
        assert_eq!(found[0].settings.interval_minutes, 120);

        turn_off(&mut conn, user).await.unwrap();
        assert!(subscribers(&mut conn).await.unwrap().is_empty());
        let kept = get(&mut conn, user).await.unwrap().unwrap();
        assert!(!kept.enabled);
        assert_eq!((kept.start, kept.end), (at(9, 0), at(18, 0)));
    }

    #[tokio::test]
    async fn the_schema_refuses_a_bad_interval_and_an_end_before_the_start() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = utc("2026-11-16T10:00:00Z");
        let family = seed_family(&mut conn, now).await;
        let user = family.bilal.id;

        assert!(
            turn_on(&mut conn, user, 15, at(9, 0), at(18, 0), now)
                .await
                .is_err()
        );
        assert!(
            turn_on(&mut conn, user, 300, at(9, 0), at(18, 0), now)
                .await
                .is_err()
        );
        assert!(
            turn_on(&mut conn, user, 120, at(18, 0), at(9, 0), now)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_prompt_is_claimed_once_and_stays_open_for_half_an_hour_until_answered() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = utc("2026-11-16T11:00:00Z");
        let family = seed_family(&mut conn, now).await;
        let (user, dm) = (family.bilal.id, family.dm);
        let slot = utc("2026-11-16T11:00:00Z");

        assert!(claim_prompt(&mut conn, user, dm, slot, now).await.unwrap());
        assert!(!claim_prompt(&mut conn, user, dm, slot, now).await.unwrap());
        // Not open until WhatsApp has accepted it.
        assert_eq!(open_prompt(&mut conn, dm, now).await.unwrap(), None);
        set_prompt_message(&mut conn, user, slot, "PROMPT1")
            .await
            .unwrap();

        let soon = utc("2026-11-16T11:29:00Z");
        assert_eq!(open_prompt(&mut conn, dm, soon).await.unwrap(), Some(slot));
        let late = utc("2026-11-16T11:31:00Z");
        assert_eq!(open_prompt(&mut conn, dm, late).await.unwrap(), None);
        assert_eq!(
            slot_of_message(&mut conn, "PROMPT1", dm).await.unwrap(),
            Some(slot)
        );
        assert_eq!(
            slot_of_message(&mut conn, "PROMPT1", family.group)
                .await
                .unwrap(),
            None
        );

        add_entry(
            &mut conn,
            user,
            soon,
            date("2026-11-16"),
            Some(slot),
            "emails",
        )
        .await
        .unwrap();
        assert_eq!(
            open_prompt(&mut conn, dm, soon).await.unwrap(),
            None,
            "answered"
        );
    }

    #[tokio::test]
    async fn a_summary_is_claimed_once_per_day() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = utc("2026-11-16T18:00:00Z");
        let family = seed_family(&mut conn, now).await;
        let user = family.bilal.id;
        let day = date("2026-11-16");

        assert!(!summary_claimed(&mut conn, user, day).await.unwrap());
        assert!(claim_summary(&mut conn, user, day, now).await.unwrap());
        assert!(!claim_summary(&mut conn, user, day, now).await.unwrap());
        assert!(summary_claimed(&mut conn, user, day).await.unwrap());
        // The next day is a new summary.
        let next = date("2026-11-17");
        assert!(!summary_claimed(&mut conn, user, next).await.unwrap());
        assert!(claim_summary(&mut conn, user, next, now).await.unwrap());
    }

    #[tokio::test]
    async fn only_prompts_whatsapp_accepted_count_as_sent() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let now = utc("2026-11-16T08:00:00Z");
        let family = seed_family(&mut conn, now).await;
        let (user, dm) = (family.bilal.id, family.dm);
        let (nine, eleven) = (utc("2026-11-16T09:00:00Z"), utc("2026-11-16T11:00:00Z"));

        claim_prompt(&mut conn, user, dm, nine, nine).await.unwrap();
        set_prompt_message(&mut conn, user, nine, "P9")
            .await
            .unwrap();
        // Claimed, but the send failed.
        claim_prompt(&mut conn, user, dm, eleven, eleven)
            .await
            .unwrap();

        let day = (utc("2026-11-16T00:00:00Z"), utc("2026-11-17T00:00:00Z"));
        assert_eq!(
            prompts_between(&mut conn, user, day.0, day.1)
                .await
                .unwrap(),
            vec![nine]
        );
        let next_day = (utc("2026-11-17T00:00:00Z"), utc("2026-11-18T00:00:00Z"));
        assert!(
            prompts_between(&mut conn, user, next_day.0, next_day.1)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

//! Clearing the data of people who stop using the bot.
//!
//! After `INACTIVE_DAYS` without using it, a person's data is deleted: their
//! DM (its lists, settings and reminders), their group memberships and todo
//! reminders, their prayer log and their usage log. Their `users` row goes
//! too, unless a group's todo or shopping list still names them; then only
//! the row stays, and goes once those items are cleared. Group lists are
//! the family's, so they're never touched.
//!
//! Anyone with a DM is warned there first, `WARN_DAYS` before. Anything
//! they send keeps their data.
//!
//! Groups follow the same rules: after `INACTIVE_DAYS` without anyone using
//! the bot in them (and a warning posted there `WARN_DAYS` before), the
//! bot leaves the group and deletes everything stored for it. Groups the
//! bot is no longer in (it was removed, or the group deleted) are deleted
//! too, once GOWA hasn't listed them for `LEFT_GRACE_HOURS`. All of this is
//! on by default (`INACTIVE_DAYS=14`); `INACTIVE_DAYS=0` turns it off.
//!
//! The rules are pure functions here; the database side is
//! `repo::cleanup`, and `check` runs from the reminder loop.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Timelike, Utc};
use sqlx::Connection;
use tracing::{info, warn};

use crate::AppState;
use crate::error::AppError;
use crate::repo::cleanup as repo;

/// How many days before clearing the warning goes out.
pub const WARN_DAYS: i64 = 3;

/// How long a group must be missing from GOWA's list before it's deleted,
/// so one bad answer from GOWA can't delete a group.
pub const LEFT_GRACE_HOURS: i64 = 24;

const WAVE: &str = "👋";

/// What's known about one person's activity.
#[derive(Debug, Clone, PartialEq)]
pub struct Activity {
    pub last_active: DateTime<Utc>,
    pub warned_at: Option<DateTime<Utc>>,
    pub cleared_at: Option<DateTime<Utc>>,
    /// Whether they have a DM the warning can go to.
    pub can_warn: bool,
}

/// What, if anything, is due for one person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    Nothing,
    Warn,
    Clear,
}

/// When the warning goes out: `WARN_DAYS` before the end of the inactive
/// period (or straight away, if the period is shorter than that).
fn warn_from(last_active: DateTime<Utc>, days: i64) -> DateTime<Utc> {
    last_active + Duration::days((days - WARN_DAYS).max(0))
}

/// What's due at `now`, with clearing after `days` (0 or less: never).
///
/// Someone who can be warned is never cleared without a warning at least
/// `WARN_DAYS` old. So turning this on doesn't clear anyone at once: the
/// long-inactive are warned first.
pub fn due(activity: &Activity, now: DateTime<Utc>, days: i64) -> Due {
    if days <= 0 || activity.cleared_at.is_some() {
        return Due::Nothing;
    }
    // Someone who can be warned, but hasn't been yet, is only ever warned:
    // clearing waits for a warning that's really been sent.
    if activity.can_warn && activity.warned_at.is_none() {
        return if now >= warn_from(activity.last_active, days) {
            Due::Warn
        } else {
            Due::Nothing
        };
    }
    match clears_on(activity, days, now) {
        Some(clears) if now >= clears => Due::Clear,
        _ => Due::Nothing,
    }
}

/// When their data will be cleared if they stay inactive, as seen at `now`;
/// `None` if it won't be (clearing is off, or it's already been done).
pub fn clears_on(activity: &Activity, days: i64, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if days <= 0 || activity.cleared_at.is_some() {
        return None;
    }
    let after_inactive = activity.last_active + Duration::days(days);
    if !activity.can_warn {
        return Some(after_inactive);
    }
    // At the end of the period, but never sooner than WARN_DAYS after the
    // warning: the one sent, or the one still to send (at its time, or now
    // if that's already past).
    let warned = activity
        .warned_at
        .unwrap_or_else(|| warn_from(activity.last_active, days).max(now));
    Some(after_inactive.max(warned + Duration::days(WARN_DAYS)))
}

/// What to do with one stored group, given whether GOWA still lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupStep {
    /// Nothing to do.
    Keep,
    /// It's listed again after being missing: forget that it was.
    Back,
    /// Missing for the first time: note when.
    Missing,
    /// Missing for `LEFT_GRACE_HOURS`: delete it.
    Delete,
}

/// `left_at` is when it was first seen missing (`None` if it wasn't).
pub fn group_step(listed: bool, left_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> GroupStep {
    match (listed, left_at) {
        (true, None) => GroupStep::Keep,
        (true, Some(_)) => GroupStep::Back,
        (false, None) => GroupStep::Missing,
        (false, Some(left)) if now - left >= Duration::hours(LEFT_GRACE_HOURS) => GroupStep::Delete,
        (false, Some(_)) => GroupStep::Keep,
    }
}

/// Whether to believe GOWA's list of groups. An empty list while groups are
/// stored is far more likely a GOWA problem than the bot being removed
/// from every group at once, so it's ignored.
pub fn trust_group_list(listed: usize, stored: usize) -> bool {
    listed > 0 || stored == 0
}

/// The warning posted in a group nobody uses the bot in.
pub fn group_warning(days: i64) -> String {
    format!(
        "{WAVE} Nobody has used me in this group for {days} days. To keep things tidy, \
         I'll leave it in {WARN_DAYS} days and delete its lists and settings. Send any \
         command (like /help) to keep me here."
    )
}

/// The warning, sent in their DM.
pub fn warning(days: i64) -> String {
    format!(
        "{WAVE} I haven't heard from you for a while. To keep things tidy, your private \
         lists and settings here will be cleared in {WARN_DAYS} days if you don't use me \
         before then ({days} days without using me). Send me anything to keep them."
    )
}

/// Warns and clears whoever is due at `now`. Runs from the reminder loop.
pub async fn check(state: &AppState, now: DateTime<Utc>) -> Result<(), AppError> {
    let days = state.config.inactive_days;
    if days <= 0 {
        return Ok(());
    }
    // Groups first (asking GOWA once an hour is plenty): deleting one can
    // free people's rows for the steps below.
    if now.minute() == 0 {
        check_groups(state, now).await?;
    }
    check_quiet_groups(state, now, days).await?;
    let people = {
        let mut conn = state.db.acquire().await?;
        repo::people(&mut conn).await?
    };
    for person in people {
        // One person's problem mustn't stop the others, so errors are
        // logged per person.
        if let Err(err) = check_person(state, &person, now, days).await {
            warn!("{} failed: {err}", person.name);
        }
    }
    Ok(())
}

/// Notes groups GOWA no longer lists, and deletes those missing for
/// `LEFT_GRACE_HOURS`. Does nothing if GOWA can't be asked, or gives a list
/// it shouldn't be trusted with.
pub async fn check_groups(state: &AppState, now: DateTime<Utc>) -> Result<(), AppError> {
    if state.config.inactive_days <= 0 {
        return Ok(());
    }
    let listed = match state.gowa.groups().await {
        Ok(groups) => groups,
        Err(err) => {
            warn!("couldn't ask GOWA for its groups: {err}");
            return Ok(());
        }
    };
    let mut conn = state.db.acquire().await?;
    let stored = repo::groups(&mut conn).await?;
    if !trust_group_list(listed.len(), stored.len()) {
        warn!(
            "GOWA lists no groups, but {} are stored; not trusting that",
            stored.len()
        );
        return Ok(());
    }
    // A `HashSet` answers "is this one in it?" without searching the list.
    let listed: HashSet<&str> = listed.iter().map(|g| g.jid.as_str()).collect();
    for group in stored {
        let name = group.name.as_deref().unwrap_or(&group.wa_chat_id);
        match group_step(
            listed.contains(group.wa_chat_id.as_str()),
            group.left_at,
            now,
        ) {
            GroupStep::Keep => {}
            GroupStep::Back => {
                repo::set_left(&mut conn, group.id, None).await?;
                info!("the bot is in {name} again");
            }
            GroupStep::Missing => {
                repo::set_left(&mut conn, group.id, Some(now)).await?;
                info!("the bot isn't in {name} any more; deleting it in {LEFT_GRACE_HOURS} hours");
            }
            GroupStep::Delete => {
                // On the connection already held: asking the pool for a
                // second one while holding this would tie up two (and
                // wait forever with a pool of one).
                let mut tx = conn.begin().await?;
                repo::delete_chat(&mut tx, group.id).await?;
                tx.commit().await?;
                info!("deleted {name} and its lists (the bot left it)");
            }
        }
    }
    Ok(())
}

/// Warns, then leaves, groups nobody has used the bot in for `days`.
async fn check_quiet_groups(
    state: &AppState,
    now: DateTime<Utc>,
    days: i64,
) -> Result<(), AppError> {
    let groups = {
        let mut conn = state.db.acquire().await?;
        repo::groups(&mut conn).await?
    };
    // Groups the bot has already left are `check_groups`'s to delete.
    for group in groups.into_iter().filter(|g| g.left_at.is_none()) {
        let name = group
            .name
            .clone()
            .unwrap_or_else(|| group.wa_chat_id.clone());
        let result = match due(&group.activity(), now, days) {
            Due::Nothing => Ok(()),
            Due::Warn => warn_group(state, &group, now, days).await,
            Due::Clear => {
                leave_and_delete(state, group.id, &group.wa_chat_id, Some(group.last_active))
                    .await
                    .map(|left| {
                        if left {
                            info!("left {name} and deleted its lists (nobody used the bot there)");
                        }
                    })
            }
        };
        if let Err(err) = result {
            warn!("group {name} failed: {err}");
        }
    }
    Ok(())
}

async fn warn_group(
    state: &AppState,
    group: &repo::StoredGroup,
    now: DateTime<Utc>,
    days: i64,
) -> Result<(), AppError> {
    // Marked as warned only once it's really posted.
    state
        .gowa
        .send_text(&group.wa_chat_id, &group_warning(days), None, &[])
        .await?;
    let mut conn = state.db.acquire().await?;
    repo::set_group_warned(&mut conn, group.id, now).await?;
    info!(
        "warned {}",
        group.name.as_deref().unwrap_or(&group.wa_chat_id)
    );
    Ok(())
}

/// Makes the bot leave a group, then deletes the group and everything
/// stored for it. With `only_if_inactive_since`, only if nobody has used
/// the bot there since then (`false` if someone has). If GOWA can't leave,
/// nothing is deleted and the error comes back.
pub async fn leave_and_delete(
    state: &AppState,
    chat_id: i64,
    wa_chat_id: &str,
    only_if_inactive_since: Option<DateTime<Utc>>,
) -> Result<bool, AppError> {
    let mut tx = state.db.begin().await?;
    // A "let chain": the second test only runs if the first matched.
    if let Some(since) = only_if_inactive_since
        && !repo::group_still_inactive(&mut tx, chat_id, since).await?
    {
        return Ok(false);
    }
    // Leave first: if that fails, `?` returns and dropping `tx` keeps
    // everything, so the next check can try again.
    state.gowa.leave_group(wa_chat_id).await?;
    repo::delete_chat(&mut tx, chat_id).await?;
    tx.commit().await?;
    Ok(true)
}

async fn check_person(
    state: &AppState,
    person: &repo::Person,
    now: DateTime<Utc>,
    days: i64,
) -> Result<(), AppError> {
    // Already cleared: the row goes once no group list names them.
    if person.cleared_at.is_some() {
        let mut conn = state.db.acquire().await?;
        if repo::delete_if_unnamed(&mut conn, person.id).await? {
            info!("deleted {}'s last record", person.name);
        }
        return Ok(());
    }
    match due(&person.activity(), now, days) {
        Due::Nothing => {}
        Due::Warn => {
            // `can_warn` means there's a DM, so this is always `Some`.
            let Some(dm) = &person.dm_chat else {
                return Ok(());
            };
            // Marked as warned only once it's really sent; if GOWA is down,
            // the next check tries again.
            state.gowa.send_text(dm, &warning(days), None, &[]).await?;
            let mut conn = state.db.acquire().await?;
            repo::set_warned(&mut conn, person.id, now).await?;
            info!("warned {}", person.name);
        }
        Due::Clear => {
            let mut tx = state.db.begin().await?;
            // `Some` only if they're still as inactive as when `people` read
            // them: a message in the meantime keeps their data.
            let cleared =
                repo::clear_if_inactive(&mut tx, person.id, person.last_active, now).await?;
            tx.commit().await?;
            match cleared {
                Some(repo::Cleared { deleted_user: true }) => {
                    info!(
                        "cleared {} (last active {})",
                        person.name, person.last_active
                    );
                }
                Some(repo::Cleared {
                    deleted_user: false,
                }) => info!(
                    "cleared {} (last active {}); kept their name, a group list mentions them",
                    person.name, person.last_active
                ),
                None => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::utc;

    fn activity(last_active: &str, warned_at: Option<&str>, can_warn: bool) -> Activity {
        Activity {
            last_active: utc(last_active),
            warned_at: warned_at.map(utc),
            cleared_at: None,
            can_warn,
        }
    }

    const DAYS: i64 = 14;

    #[test]
    fn off_means_nothing_ever() {
        let long_ago = activity("2020-01-01T00:00:00Z", None, true);
        assert_eq!(due(&long_ago, utc("2026-11-02T12:00:00Z"), 0), Due::Nothing);
        assert_eq!(clears_on(&long_ago, 0, utc("2026-11-02T12:00:00Z")), None);
    }

    #[test]
    fn warned_three_days_before_then_cleared() {
        // Last active 1 Nov at noon: warned from 12 Nov, cleared from 15 Nov.
        let quiet = activity("2026-11-01T12:00:00Z", None, true);
        assert_eq!(due(&quiet, utc("2026-11-12T11:59:00Z"), DAYS), Due::Nothing);
        assert_eq!(due(&quiet, utc("2026-11-12T12:00:00Z"), DAYS), Due::Warn);
        assert_eq!(
            clears_on(&quiet, DAYS, utc("2026-11-02T00:00:00Z")),
            Some(utc("2026-11-15T12:00:00Z"))
        );

        let warned = activity("2026-11-01T12:00:00Z", Some("2026-11-12T12:05:00Z"), true);
        // Not warned twice, and not cleared before 3 days after the warning.
        assert_eq!(
            due(&warned, utc("2026-11-14T00:00:00Z"), DAYS),
            Due::Nothing
        );
        assert_eq!(
            due(&warned, utc("2026-11-15T12:00:00Z"), DAYS),
            Due::Nothing
        );
        assert_eq!(due(&warned, utc("2026-11-15T12:05:00Z"), DAYS), Due::Clear);
    }

    #[test]
    fn turning_it_on_warns_the_long_inactive_first() {
        // Inactive for months, never warned (it was off): warn now, clear 3
        // days later, never straight away.
        let months = activity("2026-06-01T00:00:00Z", None, true);
        let now = utc("2026-11-02T09:00:00Z");
        assert_eq!(due(&months, now, DAYS), Due::Warn);
        // Before the warning goes, the dashboard already says "3 days from now".
        assert_eq!(clears_on(&months, DAYS, now), Some(now + Duration::days(3)));
        let warned = Activity {
            warned_at: Some(now),
            ..months
        };
        assert_eq!(due(&warned, now + Duration::days(2), DAYS), Due::Nothing);
        assert_eq!(due(&warned, now + Duration::days(3), DAYS), Due::Clear);
    }

    #[test]
    fn without_a_dm_there_is_no_warning() {
        let no_dm = activity("2026-11-01T12:00:00Z", None, false);
        assert_eq!(due(&no_dm, utc("2026-11-12T12:00:00Z"), DAYS), Due::Nothing);
        assert_eq!(due(&no_dm, utc("2026-11-15T12:00:00Z"), DAYS), Due::Clear);
        assert_eq!(
            clears_on(&no_dm, DAYS, utc("2026-11-02T00:00:00Z")),
            Some(utc("2026-11-15T12:00:00Z"))
        );
    }

    #[test]
    fn a_period_shorter_than_the_warning_still_warns_first() {
        let quiet = activity("2026-11-01T12:00:00Z", None, true);
        assert_eq!(due(&quiet, utc("2026-11-01T12:00:00Z"), 1), Due::Warn);
        assert_eq!(
            clears_on(&quiet, 1, utc("2026-11-01T12:00:00Z")),
            Some(utc("2026-11-04T12:00:00Z"))
        );
    }

    #[test]
    fn already_cleared_means_nothing_more() {
        let cleared = Activity {
            cleared_at: Some(utc("2026-10-01T00:00:00Z")),
            ..activity("2026-09-01T00:00:00Z", Some("2026-09-28T00:00:00Z"), true)
        };
        assert_eq!(
            due(&cleared, utc("2026-11-02T00:00:00Z"), DAYS),
            Due::Nothing
        );
        assert_eq!(clears_on(&cleared, DAYS, utc("2026-11-02T00:00:00Z")), None);
    }

    #[test]
    fn a_group_goes_a_day_after_it_disappears() {
        let now = utc("2026-11-02T12:00:00Z");
        assert_eq!(group_step(true, None, now), GroupStep::Keep);
        assert_eq!(group_step(false, None, now), GroupStep::Missing);
        let missing_since = Some(utc("2026-11-01T13:00:00Z"));
        assert_eq!(
            group_step(false, missing_since, now),
            GroupStep::Keep,
            "23 hours"
        );
        assert_eq!(group_step(true, missing_since, now), GroupStep::Back);
        let a_day_ago = Some(utc("2026-11-01T12:00:00Z"));
        assert_eq!(group_step(false, a_day_ago, now), GroupStep::Delete);
    }

    #[test]
    fn an_empty_group_list_is_not_trusted() {
        assert!(!trust_group_list(0, 2));
        assert!(trust_group_list(0, 0));
        assert!(trust_group_list(1, 2));
    }

    #[test]
    fn the_group_warning_says_how_to_keep_the_bot() {
        let text = group_warning(14);
        assert!(text.contains("14 days"), "{text}");
        assert!(text.contains("leave it in 3 days"), "{text}");
        assert!(text.contains("/help"), "{text}");
    }

    #[test]
    fn the_warning_says_when() {
        let text = warning(14);
        assert!(text.contains("in 3 days"), "{text}");
        assert!(text.contains("14 days"), "{text}");
    }
}

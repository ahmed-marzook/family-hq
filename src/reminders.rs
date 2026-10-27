//! The prayer reminder loop: a background task that wakes every minute,
//! and sends each chat the reminders due under that chat's own settings
//! (a DM's are that person's own: their city, method and timezone).

use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};

use crate::AppState;
use crate::error::AppError;
use crate::prayer::{self, ReminderKind};
use crate::repo::prayers::{self, ReminderChat};

/// How often to check. Reminders go out at most this late.
const CHECK_EVERY: StdDuration = StdDuration::from_secs(60);

/// Runs forever. `main` starts it with `tokio::spawn`, which runs it
/// alongside the web server on the same runtime: while this `.await`s the
/// next tick, the thread is free to serve webhooks.
pub async fn run(state: Arc<AppState>) {
    let mut ticks = tokio::time::interval(CHECK_EVERY);
    loop {
        // The first tick completes immediately, so a check runs at startup.
        ticks.tick().await;
        // The clock is read here, at the edge, and passed down.
        if let Err(err) = check(&state, Utc::now()).await {
            println!("reminders: check failed: {err}");
        }
    }
}

/// Sends every reminder due at `now` that hasn't been sent yet.
async fn check(state: &AppState, now: DateTime<Utc>) -> Result<(), AppError> {
    let chats = {
        let mut conn = state.db.acquire().await?;
        prayers::reminder_chats(&mut conn).await?
    };
    for chat in chats {
        // One chat's problem (say AlAdhan can't find its city today) must
        // not stop the others' reminders, so errors are logged per chat.
        if let Err(err) = check_chat(state, &chat, now).await {
            println!("reminders: {} failed: {err}", chat.wa_chat_id);
        }
    }
    Ok(())
}

async fn check_chat(
    state: &AppState,
    chat: &ReminderChat,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    let settings = chat.overrides.apply(&state.prayer_times.defaults);
    let day = state
        .prayer_times
        .for_date(&settings.prayer_location(), settings.today(now))
        .await?;
    let due = prayer::due_reminders(&day, now, settings.prayer_ending_minutes);
    if due.is_empty() {
        return Ok(());
    }

    let mut conn = state.db.acquire().await?;
    for (kind, window) in due {
        // In a DM, "ending soon" is pointless if they've already prayed it.
        // (In a group it still goes out: some members may not have.)
        if kind == ReminderKind::Ending
            && let Some(user_id) = chat.dm_user_id
        {
            let prayed = prayers::prayed_on(&mut conn, user_id, day.date).await?;
            if prayed.contains(&window.prayer) {
                continue;
            }
        }

        // Claim first, send second: if the row already exists, this
        // reminder went out on an earlier tick (or before a restart).
        let Some(reminder_id) = prayers::claim_reminder(
            &mut conn,
            chat.id,
            day.date,
            window.prayer,
            kind,
            window.start,
            now,
        )
        .await?
        else {
            continue;
        };

        let text = prayer::reminder_text(kind, &window, now, &settings);
        match state.gowa.send_text(&chat.wa_chat_id, &text, None).await {
            Ok(message_id) => {
                prayers::set_reminder_message(&mut conn, reminder_id, &message_id).await?;
                println!(
                    "reminders: sent {} {:?} to {}",
                    window.prayer, kind, chat.wa_chat_id
                );
            }
            // Not retried: the claim stays, so a flaky GOWA can't cause a
            // burst of repeats later.
            Err(err) => println!(
                "reminders: couldn't send {} to {}: {err}",
                window.prayer, chat.wa_chat_id
            ),
        }
    }
    Ok(())
}

//! The prayer reminder loop: a background task that wakes every minute,
//! and sends any reminder that's due to every chat that asked for them.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};

use crate::AppState;
use crate::error::AppError;
use crate::prayer;
use crate::repo::prayers;

/// How often to check. Reminders go out at most this late.
const CHECK_EVERY: StdDuration = StdDuration::from_secs(60);

/// Runs forever. `main` starts it with `tokio::spawn`, which runs it
/// alongside the web server on the same runtime: while this `.await`s the
/// next tick, the thread is free to serve webhooks.
pub async fn run(state: Arc<AppState>) {
    let lead = Duration::minutes(state.config.prayer_reminder_minutes);
    let mut ticks = tokio::time::interval(CHECK_EVERY);
    loop {
        // The first tick completes immediately, so a check runs at startup.
        ticks.tick().await;
        // The clock is read here, at the edge, and passed down.
        if let Err(err) = check(&state, Utc::now(), lead).await {
            // Keep going: a failed check (AlAdhan down, say) is retried next
            // minute, and a reminder stays due for a while after its time.
            println!("reminders: check failed: {err}");
        }
    }
}

/// Sends every reminder due at `now` that hasn't been sent yet.
async fn check(state: &AppState, now: DateTime<Utc>, lead: Duration) -> Result<(), AppError> {
    let day = state
        .prayer_times
        .for_date(prayer::london_date(now))
        .await?;
    let due = prayer::due_reminders(&day, now, lead);
    if due.is_empty() {
        return Ok(());
    }

    let mut conn = state.db.acquire().await?;
    let chats = prayers::reminder_chats(&mut conn).await?;

    for (prayer, start) in due {
        for chat in &chats {
            // Claim first, send second: if the row already exists, this
            // reminder went out on an earlier tick (or before a restart).
            let Some(reminder_id) =
                prayers::claim_reminder(&mut conn, chat.id, day.date, prayer, start, now).await?
            else {
                continue;
            };

            let text = prayer::reminder_text(prayer, start, now);
            match state.gowa.send_text(&chat.wa_chat_id, &text, None).await {
                Ok(message_id) => {
                    prayers::set_reminder_message(&mut conn, reminder_id, &message_id).await?;
                    println!("reminders: sent {prayer} to {}", chat.wa_chat_id);
                }
                // Not retried: the claim stays, so a flaky GOWA can't cause
                // a burst of repeats later. The next prayer's reminder still
                // goes out as normal.
                Err(err) => println!(
                    "reminders: couldn't send {prayer} to {}: {err}",
                    chat.wa_chat_id
                ),
            }
        }
    }
    Ok(())
}

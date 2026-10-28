//! The prayer reminder loop: a background task that wakes every minute and
//! sends the reminders that are due.
//!
//! * In a DM: one reminder, on that person's own settings.
//! * In a group: each member's times come from *their own* settings (from
//!   their DM with the bot; members who never set any use the group's).
//!   Members with the same settings share one message that @mentions them,
//!   so a family split between the UK and Sri Lanka gets one reminder per
//!   country, each at the right local time.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};

use crate::AppState;
use crate::error::AppError;
use crate::prayer::{self, ReminderKind};
use crate::repo::prayers::{self, NewReminder, ReminderChat};
use crate::repo::settings::{self as stored, MemberSettings};
use crate::settings::Settings;

/// How often to check. Reminders go out at most this late.
const CHECK_EVERY: StdDuration = StdDuration::from_secs(60);

/// Someone a group reminder is for.
#[derive(Debug, Clone, PartialEq)]
pub struct Recipient {
    pub user_id: i64,
    /// Phone number (no `@s.whatsapp.net`), for the @mention.
    pub phone: String,
}

/// The members of one group who share the same settings, so they get the
/// same reminder at the same moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub settings: Settings,
    pub recipients: Vec<Recipient>,
}

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
        // One chat's problem (say AlAdhan can't find a city today) must not
        // stop the others' reminders, so errors are logged per chat.
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
    let chat_settings = chat.overrides.apply(&state.apis.prayer_times.defaults);

    let places = if let Some(owner) = chat.dm_user_id {
        // A DM: just its owner, on the DM's settings (theirs), no mention.
        vec![Place {
            settings: chat_settings,
            recipients: vec![Recipient {
                user_id: owner,
                phone: String::new(),
            }],
        }]
    } else {
        let members = {
            let mut conn = state.db.acquire().await?;
            stored::members(&mut conn, chat.id).await?
        };
        places_for_group(&chat_settings, &members)
    };

    // In a group with members in more than one place, say which place each
    // reminder is for.
    let name_places = places.len() > 1;
    for place in &places {
        send_due(state, chat, place, name_places, now).await?;
    }
    Ok(())
}

/// Splits a group's members by their own settings. Members who never set
/// their own use the group's. A group whose members aren't known yet (the
/// bot has just been added) gets one reminder on the group's settings with
/// nobody mentioned.
///
/// Pure (no database, no network), so it's tested directly.
pub fn places_for_group(group: &Settings, members: &[MemberSettings]) -> Vec<Place> {
    let mut places: Vec<Place> = Vec::new();
    for member in members {
        let settings = match &member.settings {
            Some(own) => own.overrides.apply(group),
            None => group.clone(),
        };
        let recipient = Recipient {
            user_id: member.user_id,
            phone: member
                .wa_jid
                .split_once('@')
                .map_or(member.wa_jid.clone(), |(phone, _)| phone.to_string()),
        };
        // Same place = same town, method, school, timezone and reminder
        // timing. Exact coordinates may differ a little (two people sharing
        // their location in the same town): they still share one message,
        // on the first one's coordinates.
        let key = settings.place_key();
        match places.iter_mut().find(|p| p.settings.place_key() == key) {
            Some(place) => place.recipients.push(recipient),
            None => places.push(Place {
                settings,
                recipients: vec![recipient],
            }),
        }
    }
    if places.is_empty() {
        places.push(Place {
            settings: group.clone(),
            recipients: Vec::new(),
        });
    }
    places
}

/// Sends whatever is due for one place in one chat.
async fn send_due(
    state: &AppState,
    chat: &ReminderChat,
    place: &Place,
    name_place: bool,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    let settings = &place.settings;
    let location = state.apis.prayer_location(settings).await?;
    let day = state
        .apis
        .prayer_times
        .for_date(&location, settings.today(now))
        .await?;
    let due = prayer::due_reminders(&day, now, settings.prayer_ending_minutes);
    if due.is_empty() {
        return Ok(());
    }

    let is_group = chat.dm_user_id.is_none();
    // A DM's reminders don't need telling apart; a group's are per place.
    let place_key = if is_group {
        location.key()
    } else {
        String::new()
    };
    let mut conn = state.db.acquire().await?;

    for (kind, window) in due {
        // "Ending soon" only goes to people who haven't prayed it yet.
        let mut recipients = place.recipients.clone();
        if kind == ReminderKind::Ending {
            let mut still_to_pray = Vec::new();
            for r in recipients {
                let prayed = prayers::prayed_on(&mut conn, r.user_id, day.date).await?;
                if !prayed.contains(&window.prayer) {
                    still_to_pray.push(r);
                }
            }
            // Everyone it was for has prayed: nothing to send. (A group with
            // no known members has nobody to check, so it still goes out.)
            if still_to_pray.is_empty() && !place.recipients.is_empty() {
                continue;
            }
            recipients = still_to_pray;
        }

        // Claim first, send second: if the row already exists, this
        // reminder went out on an earlier tick (or before a restart).
        let Some(reminder_id) = prayers::claim_reminder(
            &mut conn,
            &NewReminder {
                chat_id: chat.id,
                date: day.date,
                prayer: window.prayer,
                kind,
                place: &place_key,
                prayer_time: window.start,
            },
            now,
        )
        .await?
        else {
            continue;
        };

        let place_name = name_place.then_some(settings.city.as_str());
        let mut text = prayer::reminder_text(kind, &window, now, settings, place_name);
        let mentions: Vec<String> = if is_group {
            recipients.iter().map(|r| r.phone.clone()).collect()
        } else {
            Vec::new()
        };
        if !mentions.is_empty() {
            // `@<number>` in the text is where each mention shows; WhatsApp
            // displays it as the person's name.
            let tags: Vec<String> = mentions.iter().map(|p| format!("@{p}")).collect();
            text = format!("{}\n{text}", tags.join(" "));
        }

        match state
            .gowa
            .send_text(&chat.wa_chat_id, &text, None, &mentions)
            .await
        {
            Ok(message_id) => {
                prayers::set_reminder_message(&mut conn, reminder_id, &message_id).await?;
                println!(
                    "reminders: sent {} {:?} ({}) to {}",
                    window.prayer, kind, settings.city, chat.wa_chat_id
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::settings::ChatSettings;
    use crate::settings::Overrides;
    use crate::settings::tests::defaults;

    fn member(id: i64, phone: &str, own: Option<Overrides>) -> MemberSettings {
        MemberSettings {
            user_id: id,
            display_name: format!("member{id}"),
            wa_jid: format!("{phone}@s.whatsapp.net"),
            settings: own.map(|overrides| ChatSettings {
                prayer_reminders: false,
                overrides,
            }),
        }
    }

    fn colombo() -> Overrides {
        Overrides {
            city: Some("Colombo".to_string()),
            country: Some("LK".to_string()),
            timezone: Some("Asia/Colombo".to_string()),
            ..Overrides::default()
        }
    }

    #[test]
    fn members_in_two_countries_get_two_places() {
        let members = [
            member(1, "447700900001", None),           // UK, group default
            member(2, "94770000002", Some(colombo())), // Sri Lanka
            member(3, "447700900003", None),           // UK too
        ];
        let places = places_for_group(&defaults(), &members);

        assert_eq!(places.len(), 2);
        assert_eq!(places[0].settings.city, "Milton Keynes");
        let uk: Vec<&str> = places[0]
            .recipients
            .iter()
            .map(|r| r.phone.as_str())
            .collect();
        assert_eq!(uk, ["447700900001", "447700900003"]);
        assert_eq!(places[1].settings.city, "Colombo");
        assert_eq!(places[1].settings.timezone, chrono_tz::Asia::Colombo);
        assert_eq!(places[1].recipients[0].phone, "94770000002");
    }

    #[test]
    fn same_city_but_different_school_is_a_different_place() {
        let hanafi = Overrides {
            prayer_school: Some(prayer::HANAFI as i64),
            ..Overrides::default()
        };
        let members = [
            member(1, "447700900001", None),
            member(2, "447700900002", Some(hanafi)),
        ];
        assert_eq!(places_for_group(&defaults(), &members).len(), 2);
    }

    #[test]
    fn a_group_with_no_known_members_still_gets_one_reminder() {
        let places = places_for_group(&defaults(), &[]);
        assert_eq!(places.len(), 1);
        assert!(places[0].recipients.is_empty());
    }
}

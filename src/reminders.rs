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
use tracing::{error, info, warn};

use crate::AppState;
use crate::error::AppError;
use crate::prayer::{self, ReminderKind};
use crate::prayer_tracking;
use crate::push::{self, Feature};
use crate::repo::prayers::{self, NewReminder, ReminderChat};
use crate::repo::settings::{self as stored, MemberSettings};
use crate::settings::Settings;
use crate::todo_reminders;

/// How often to check. Reminders go out at most this late.
const CHECK_EVERY: StdDuration = StdDuration::from_secs(60);

/// Someone a group reminder is for.
#[derive(Debug, Clone, PartialEq)]
pub struct Recipient {
    pub user_id: i64,
    /// Phone number (no `@s.whatsapp.net`), for the @mention.
    pub phone: String,
    /// Display name, for a group's weekly and monthly reports. Empty in a
    /// DM, whose reports say "you".
    pub name: String,
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
            error!("check failed: {err}");
        }
        if let Err(err) = todo_reminders::check(&state, Utc::now()).await {
            error!("todo reminders: check failed: {err}");
        }
        // Sunday evening: each group's week of points.
        if let Err(err) = crate::points_reports::check(&state, Utc::now()).await {
            error!("points summary: check failed: {err}");
        }
        // Not a reminder, but it runs on the same clock: warns, and clears
        // the data of, people who stopped using the bot.
        if let Err(err) = crate::cleanup::check(&state, Utc::now()).await {
            error!("cleanup: check failed: {err}");
        }
        // The daily copy of the database, made on the first tick of each day.
        if let Err(err) = crate::backup::check(&state, Utc::now()).await {
            error!("backup failed: {err}");
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
            warn!("{} failed: {err}", chat.wa_chat_id);
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
                name: String::new(),
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
        // Like one chat in `check`: one place's problem (its town can't be
        // found today) must not stop the other places in the same group,
        // so it's logged and the loop goes on.
        if let Err(err) = check_place(state, chat, place, name_places, now).await {
            warn!(
                "{} ({}) failed: {err}",
                chat.wa_chat_id, place.settings.city
            );
        }
    }
    Ok(())
}

/// Everything the loop does for one place in one chat.
async fn check_place(
    state: &AppState,
    chat: &ReminderChat,
    place: &Place,
    name_place: bool,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    send_due(state, chat, place, name_place, now).await?;
    // After the reminders: prayers whose time has ended are marked
    // missed, then the day's, week's and month's reports go out.
    prayer_tracking::check(state, chat, place, now).await
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
            name: member.display_name.clone(),
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
    let today = settings.today(now);
    // Isha ends at Islamic midnight, often after midnight on the clock
    // (about 01:00 in a UK summer), so its "ending soon" can fall on the
    // next day: yesterday's times are checked too, as in
    // `prayer_tracking::check`. Each reminder keeps the date it's *for*,
    // which is part of its claim, so yesterday's Isha and today's are
    // never mixed up. (`pred_opt` is `None` only on the first day chrono
    // knows; `flatten` skips it.)
    let mut due = Vec::new();
    for date in [today.pred_opt(), Some(today)].into_iter().flatten() {
        let day = state.apis.prayer_times.for_date(&location, date).await?;
        for (kind, window) in prayer::due_reminders(&day, now, settings.prayer_ending_minutes) {
            due.push((day.date, kind, window));
        }
    }
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

    for (date, kind, window) in due {
        // "Ending soon" only goes to people who haven't prayed it yet.
        let mut recipients = place.recipients.clone();
        if kind == ReminderKind::Ending {
            let mut still_to_pray = Vec::new();
            for r in recipients {
                let prayed = prayers::prayed_on(&mut conn, r.user_id, date).await?;
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
                date,
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
                info!(
                    "sent {} {:?} ({}) to {}",
                    window.prayer, kind, settings.city, chat.wa_chat_id
                );
            }
            // Not retried: the claim stays, so a flaky GOWA can't cause a
            // burst of repeats later.
            Err(err) => warn!(
                "couldn't send {} to {}: {err}",
                window.prayer, chat.wa_chat_id
            ),
        }

        // The same reminder as a push, for anyone it's for who has push on.
        if let Some(ntfy) = &state.apis.push {
            let notice = prayer::reminder_notice(kind, &window, date, now, settings, place_name);
            let key = prayer::push_key(date, window.prayer, kind);
            for r in &recipients {
                push::send_once(
                    ntfy,
                    &mut conn,
                    r.user_id,
                    Feature::Prayers,
                    &key,
                    &notice,
                    now,
                )
                .await?;
            }
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
    use crate::testkit::jid;

    fn member(id: i64, phone: &str, own: Option<Overrides>) -> MemberSettings {
        MemberSettings {
            user_id: id,
            display_name: format!("member{id}"),
            wa_jid: jid(phone),
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

    // ---- the loop itself, with a fake GOWA and cached prayer times ----

    use std::sync::Arc;

    use chrono::NaiveDate;

    use crate::aladhan::PrayerTimes;
    use crate::db::test_pool;
    use crate::prayer::{DayTimes, Prayer, Window};
    use crate::repo::chats;
    use crate::reports::Delivery;
    use crate::testkit::{FakeGowa, fake_gowa, register, test_state};

    /// A June day in Milton Keynes (BST, UTC+1; the times here are UTC).
    /// Isha runs to Islamic midnight, 01:00 local, which is already the
    /// next day on the clock.
    fn june(day: u32) -> DayTimes {
        let date = NaiveDate::from_ymd_opt(2026, 6, day).unwrap();
        let at = |h, m| date.and_hms_opt(h, m, 0).unwrap().and_utc();
        let window = |prayer, start, end| Window { prayer, start, end };
        DayTimes {
            date,
            hijri: None,
            windows: [
                window(Prayer::Fajr, at(1, 30), at(3, 45)),
                window(Prayer::Dhuhr, at(12, 5), at(16, 20)),
                window(Prayer::Asr, at(16, 20), at(20, 20)),
                window(Prayer::Maghrib, at(20, 20), at(21, 40)),
                // Midnight UTC, the start of the next day.
                window(
                    Prayer::Isha,
                    at(21, 40),
                    at(0, 0) + chrono::Duration::days(1),
                ),
            ],
        }
    }

    /// A test state on a fake GOWA whose prayer times for the default
    /// place (Milton Keynes, with coordinates) are already in memory for
    /// 14 and 15 June, so nothing reaches AlAdhan.
    async fn state_with_june(pool: sqlx::SqlitePool) -> (Arc<AppState>, FakeGowa) {
        let gowa = fake_gowa().await;
        let mut state = test_state(pool, &gowa.url);
        // `Arc::get_mut` gives a `&mut` only while nothing else shares the
        // `Arc`, which is still true here.
        Arc::get_mut(&mut state)
            .expect("not shared yet")
            .apis
            .prayer_times = PrayerTimes::with_cached_days(defaults(), vec![june(14), june(15)]);
        (state, gowa)
    }

    #[tokio::test]
    async fn isha_ending_reminder_after_midnight_is_sent() {
        const DM: &str = "447700900001@s.whatsapp.net";
        let pool = test_pool().await;
        let (state, gowa) = state_with_june(pool.clone()).await;
        let chat = {
            let mut conn = pool.acquire().await.unwrap();
            let user = register(&mut conn, "Bilal", "447700900001", Utc::now())
                .await
                .id;
            let id = chats::find_or_create(&mut conn, DM, Some(user), Utc::now())
                .await
                .unwrap();
            ReminderChat {
                id,
                wa_chat_id: DM.to_string(),
                dm_user_id: Some(user),
                delivery: Delivery::default(),
                // `/settings ending 15`.
                overrides: Overrides {
                    prayer_ending_minutes: Some(15),
                    ..Overrides::default()
                },
            }
        };

        // 00:45 BST on 15 June: 14 June's Isha ends in 15 minutes.
        let now = "2026-06-14T23:45:00Z".parse().unwrap();
        check_chat(&state, &chat, now).await.unwrap();

        let sent = gowa.sent.lock().unwrap();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, DM);
        assert!(sent[0].1.contains("*Isha* ends in 15 min"), "{}", sent[0].1);
    }

    #[tokio::test]
    async fn one_failing_place_does_not_stop_the_others() {
        const GROUP: &str = "120363000000000001@g.us";
        let pool = test_pool().await;
        let (mut state, gowa) = state_with_june(pool.clone()).await;
        // A geocoder that only knows Leicester, so "Nowhereville" fails.
        Arc::get_mut(&mut state).expect("not shared yet").apis.geo =
            crate::geo::Geo::with_geocoding_url(&crate::testkit::fake_geocoder().await);
        let chat = {
            let mut conn = pool.acquire().await.unwrap();
            let now = Utc::now();
            let group = chats::find_or_create(&mut conn, GROUP, None, now)
                .await
                .unwrap();
            // Aisha has no settings of her own, so she's on the group's
            // (Nowhereville, no coordinates): her place's lookup fails.
            // Members come A to Z, so her place is the first one.
            let aisha = register(&mut conn, "Aisha", "447700900001", now).await.id;
            // Bilal set his own town in his DM: Milton Keynes, the place
            // whose times are cached.
            let bilal = register(&mut conn, "Bilal", "447700900002", now).await.id;
            for user in [aisha, bilal] {
                chats::add_member(&mut conn, group, user, now)
                    .await
                    .unwrap();
            }
            let dm =
                chats::find_or_create(&mut conn, "447700900002@s.whatsapp.net", Some(bilal), now)
                    .await
                    .unwrap();
            let home = defaults();
            stored::set_location(
                &mut conn,
                dm,
                &home.city,
                &home.country,
                home.coordinates.unwrap(),
                home.timezone.name(),
            )
            .await
            .unwrap();
            ReminderChat {
                id: group,
                wa_chat_id: GROUP.to_string(),
                dm_user_id: None,
                delivery: Delivery::default(),
                overrides: Overrides {
                    city: Some("Nowhereville".to_string()),
                    country: Some("GB".to_string()),
                    ..Overrides::default()
                },
            }
        };

        // 17:25 BST on 15 June: Asr started five minutes ago.
        let now = "2026-06-15T16:25:00Z".parse().unwrap();
        // The failing place is only logged, so the chat as a whole is fine;
        // what matters is that Bilal's reminder still went out.
        check_chat(&state, &chat, now).await.unwrap();

        let sent = gowa.sent.lock().unwrap();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, GROUP);
        assert!(sent[0].1.contains("@447700900002"), "{}", sent[0].1);
        assert!(
            sent[0].1.contains("*Asr* has started in Milton Keynes"),
            "{}",
            sent[0].1
        );
    }
}

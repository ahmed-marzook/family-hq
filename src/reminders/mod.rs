//! The prayer reminder loop: a background task that wakes every minute and
//! sends the reminders that are due.
//!
//! * In a DM: one reminder, on that person's own settings.
//! * In a group: each member's times come from *their own* settings (from
//!   their DM with the bot; members who never set any use the group's).
//!   Members with the same settings share one message that @mentions them,
//!   so a family split between the UK and Sri Lanka gets one reminder per
//!   country, each at the right local time.
//!
//! How every job on the loop sends what it planned is `engine`'s.

pub mod engine;

use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, NaiveDate, Utc};
use tracing::{error, warn};

use crate::AppState;
use crate::error::AppError;
use crate::prayer::tracking;
use crate::prayer::{self, ReminderKind, Window};
use crate::push::Feature;
use crate::repo::prayers::{self, ReminderChat};
use crate::repo::settings::{self as stored, MemberSettings};
use crate::settings::{self, Overrides, Settings};
use crate::todo::reminders as todo_reminders;
use engine::{Claim, OnSent, Outgoing, Planned, Push};

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

/// Runs forever, alongside the web server (`main` spawns it).
pub async fn run(state: Arc<AppState>) {
    let mut ticks = tokio::time::interval(CHECK_EVERY);
    loop {
        // The first tick completes immediately, so a check runs at startup.
        ticks.tick().await;
        // The clock is read here, at the edge, and passed down.
        if let Err(err) = check(&state, Utc::now()).await {
            error!("check failed: {err}");
        }
        // Before the todo reminders, so the morning one sees today's charges.
        if let Err(err) = crate::points::overdue::check(&state, Utc::now()).await {
            error!("overdue todos: check failed: {err}");
        }
        if let Err(err) = todo_reminders::check(&state, Utc::now()).await {
            error!("todo reminders: check failed: {err}");
        }
        // Sunday evening: each group's week of points.
        if let Err(err) = crate::points::reports::check(&state, Utc::now()).await {
            error!("points summary: check failed: {err}");
        }
        // Not a reminder, but it runs on the same clock: warns, and clears
        // the data of, people who stopped using the bot. Not on the
        // engine: its warnings are marked only once really sent (no claim
        // first), and they're woven in with leaving groups and deleting.
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
    let defaults = &state.apis.prayer_times.defaults;

    let places = if let Some(owner) = chat.dm_user_id {
        // A DM: just its owner, on the DM's settings (theirs), no mention.
        vec![Place {
            settings: settings::layered(defaults, &chat.overrides, None),
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
        places_for_group(defaults, &chat.overrides, &members)
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
    let due = plan_due(state, chat, place, name_place, now).await?;
    let push = state.apis.push.as_ref();
    engine::deliver(&state.gowa, &state.db, push, due, now).await;
    // After the reminders: prayers whose time has ended are marked
    // missed, then the day's, week's and month's reports go out.
    tracking::check(state, chat, place, now).await
}

/// Splits a group's members by their own settings. Members who never set
/// their own use the group's. A group whose members aren't known yet (the
/// bot has just been added) gets one reminder on the group's settings with
/// nobody mentioned.
///
/// `group` is what the group itself changed from `defaults`; each member's
/// own settings go on top (`settings::layered`, the same rule as `/todo`
/// and `/prayers`).
///
/// Pure (no database, no network), so it's tested directly.
pub fn places_for_group(
    defaults: &Settings,
    group: &Overrides,
    members: &[MemberSettings],
) -> Vec<Place> {
    let mut places: Vec<Place> = Vec::new();
    for member in members {
        let own = member.settings.as_ref().map(|own| &own.overrides);
        let settings = settings::layered(defaults, group, own);
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
            settings: settings::layered(defaults, group, None),
            recipients: Vec::new(),
        });
    }
    places
}

/// One reminder that's due: the day it's for, which kind, which prayer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DueReminder {
    pub date: NaiveDate,
    pub kind: ReminderKind,
    pub window: Window,
}

/// Works out the reminders due for one place in one chat, ready for
/// `engine::deliver`. Reads prayer times and the prayer log; sends and
/// claims nothing.
async fn plan_due(
    state: &AppState,
    chat: &ReminderChat,
    place: &Place,
    name_place: bool,
    now: DateTime<Utc>,
) -> Result<Vec<Planned>, AppError> {
    let settings = &place.settings;
    let location = state.apis.prayer_location(settings).await?;
    let today = settings.today(now);
    // Isha ends at Islamic midnight, often after midnight on the clock
    // (about 01:00 in a UK summer), so its "ending soon" can fall on the
    // next day: yesterday's times are checked too, as in
    // `prayer::tracking::check`. Each reminder keeps the date it's *for*,
    // which is part of its claim, so yesterday's Isha and today's are
    // never mixed up.
    let mut due = Vec::new();
    for date in [today.pred_opt(), Some(today)].into_iter().flatten() {
        let day = state.apis.prayer_times.for_date(&location, date).await?;
        for (kind, window) in prayer::due_reminders(&day, now, settings.prayer_ending_minutes) {
            due.push(DueReminder {
                date: day.date,
                kind,
                window,
            });
        }
    }
    if due.is_empty() {
        return Ok(Vec::new());
    }

    let is_group = chat.dm_user_id.is_none();
    // A DM's reminders don't need telling apart; a group's are per place.
    let place_key = if is_group {
        location.key()
    } else {
        String::new()
    };
    let place_name = name_place.then_some(settings.city.as_str());
    // Only for reading the log; back to the pool (at the end of this
    // function) before anything is sent.
    let mut conn = state.db.acquire().await?;

    let mut planned = Vec::new();
    for reminder in due {
        // "Ending soon" only goes to people who haven't prayed it yet.
        let mut recipients = place.recipients.clone();
        if reminder.kind == ReminderKind::Ending {
            let mut still_to_pray = Vec::new();
            for r in recipients {
                let prayed = prayers::prayed_on(&mut conn, r.user_id, reminder.date).await?;
                if !prayed.contains(&reminder.window.prayer) {
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
        planned.push(reminder_plan(
            chat,
            settings,
            place_name,
            &place_key,
            &reminder,
            &recipients,
            now,
        ));
    }
    Ok(planned)
}

/// One reminder as the engine sends it: claimed per chat, day, prayer,
/// kind and place (`place_key`); the message, @mentioning `recipients` in
/// a group; its ID kept so reactions find the prayer; and the same as a
/// push to each recipient. `place_name` is said in the text when a group
/// has members in more than one place.
///
/// Pure, so tested directly.
pub fn reminder_plan(
    chat: &ReminderChat,
    settings: &Settings,
    place_name: Option<&str>,
    place_key: &str,
    reminder: &DueReminder,
    recipients: &[Recipient],
    now: DateTime<Utc>,
) -> Planned {
    let DueReminder { date, kind, window } = *reminder;
    let is_group = chat.dm_user_id.is_none();
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

    // The same reminder as a push, for anyone it's for who has push on.
    let notice = prayer::reminder_notice(kind, &window, date, now, settings, place_name);
    let key = prayer::push_key(date, window.prayer, kind);
    let push = recipients
        .iter()
        .map(|r| Push {
            user_id: r.user_id,
            feature: Feature::Prayers,
            key: key.clone(),
            notice: notice.clone(),
        })
        .collect();

    Planned {
        // Claim first, send second: if the row already exists, this
        // reminder went out on an earlier tick (or before a restart).
        claim: Claim::PrayerReminder {
            chat_id: chat.id,
            date,
            prayer: window.prayer,
            kind,
            place: place_key.to_string(),
            prayer_time: window.start,
        },
        messages: vec![Outgoing {
            to: chat.wa_chat_id.clone(),
            text,
            mentions,
            quote: None,
            on_sent: OnSent::KeepReminderMessage,
            push,
            what: format!("{} {kind:?} ({})", window.prayer, settings.city),
        }],
    }
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
        let places = places_for_group(&defaults(), &Overrides::default(), &members);

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
        assert_eq!(
            places_for_group(&defaults(), &Overrides::default(), &members).len(),
            2
        );
    }

    #[test]
    fn a_group_with_no_known_members_still_gets_one_reminder() {
        let places = places_for_group(&defaults(), &Overrides::default(), &[]);
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
    use crate::testkit::{FakeGowa, fake_gowa, register, test_state, utc};

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

    /// Colombo, with coordinates, as someone's own settings would make it.
    fn colombo_settings() -> Settings {
        Settings {
            city: "Colombo".to_string(),
            country: "LK".to_string(),
            timezone: chrono_tz::Asia::Colombo,
            coordinates: Some(crate::geo::Coordinates {
                latitude: 6.9271,
                longitude: 79.8612,
            }),
            ..defaults()
        }
    }

    /// A test state on a fake GOWA whose prayer times for the default
    /// place (Milton Keynes, with coordinates) are already in memory for
    /// 14 to 16 June, so nothing reaches AlAdhan. Colombo's are too, with
    /// the same UTC times to keep the tests short (not Colombo's real ones).
    async fn state_with_june(pool: sqlx::SqlitePool) -> (Arc<AppState>, FakeGowa) {
        let gowa = fake_gowa().await;
        let mut state = test_state(pool, &gowa.url);
        let colombo = colombo_settings();
        let colombo = colombo.prayer_location(colombo.coordinates.unwrap());
        Arc::get_mut(&mut state)
            .expect("not shared yet")
            .apis
            .prayer_times =
            PrayerTimes::with_cached_days(defaults(), vec![june(14), june(15), june(16)])
                .also_cached(&colombo, vec![june(14), june(15), june(16)]);
        (state, gowa)
    }

    const GROUP: &str = "120363000000000001@g.us";

    /// A group with Aisha (`447700900001`) and Bilal (`447700900002`) in
    /// it, on the default settings (Milton Keynes). Their ids, and the
    /// group as the loop sees it.
    async fn group_of_two(pool: &sqlx::SqlitePool) -> (i64, i64, ReminderChat) {
        let mut conn = pool.acquire().await.unwrap();
        let now = utc("2026-06-01T12:00:00Z");
        let group = chats::find_or_create(&mut conn, GROUP, None, now)
            .await
            .unwrap();
        let aisha = register(&mut conn, "Aisha", "447700900001", now).await.id;
        let bilal = register(&mut conn, "Bilal", "447700900002", now).await.id;
        for user in [aisha, bilal] {
            chats::add_member(&mut conn, group, user, now)
                .await
                .unwrap();
        }
        let chat = ReminderChat {
            id: group,
            wa_chat_id: GROUP.to_string(),
            dm_user_id: None,
            delivery: Delivery::default(),
            overrides: Overrides::default(),
        };
        (aisha, bilal, chat)
    }

    /// Ticks `prayer` on 15 June off for `user_id`.
    async fn prayed(pool: &sqlx::SqlitePool, user_id: i64, prayer: Prayer) {
        let mut conn = pool.acquire().await.unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
        crate::prayer::checkin::tick(
            &mut conn,
            user_id,
            date,
            prayer,
            None,
            utc("2026-06-15T17:00:00Z"),
        )
        .await
        .unwrap();
    }

    /// What the fake GOWA was asked to send, so far.
    fn sent(gowa: &FakeGowa) -> Vec<(String, String)> {
        gowa.sent.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn start_and_ending_go_once_and_ending_skips_who_prayed() {
        let pool = test_pool().await;
        let (state, gowa) = state_with_june(pool.clone()).await;
        let (_aisha, bilal, chat) = group_of_two(&pool).await;

        // 17:25 BST on 15 June: Asr started five minutes ago. Then the
        // next tick: already claimed.
        check_chat(&state, &chat, utc("2026-06-15T16:25:00Z"))
            .await
            .unwrap();
        check_chat(&state, &chat, utc("2026-06-15T16:26:00Z"))
            .await
            .unwrap();
        let start = sent(&gowa);
        assert_eq!(start.len(), 1, "{start:?}");
        assert_eq!(start[0].0, GROUP);
        assert!(
            start[0].1.starts_with(
                "@447700900001 @447700900002\n🕌 *Asr* has started (17:20). It ends at 21:20."
            ),
            "{}",
            start[0].1
        );
        // Its message ID was kept, so a reaction to it finds the prayer.
        {
            let mut conn = pool.acquire().await.unwrap();
            let found = prayers::find_reminder(&mut conn, "FAKE1", GROUP)
                .await
                .unwrap();
            let june_15 = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
            assert_eq!(found, Some((june_15, Prayer::Asr)));
        }

        // Bilal prays; 20 minutes before Asr ends, only Aisha is reminded.
        prayed(&pool, bilal, Prayer::Asr).await;
        check_chat(&state, &chat, utc("2026-06-15T20:01:00Z"))
            .await
            .unwrap();
        check_chat(&state, &chat, utc("2026-06-15T20:02:00Z"))
            .await
            .unwrap();
        let all = sent(&gowa);
        assert_eq!(all.len(), 2, "{all:?}");
        assert!(
            all[1]
                .1
                .starts_with("@447700900001\n⏳ *Asr* ends in 19 min (21:20)."),
            "{}",
            all[1].1
        );
    }

    #[tokio::test]
    async fn no_ending_reminder_once_everyone_has_prayed() {
        let pool = test_pool().await;
        let (state, gowa) = state_with_june(pool.clone()).await;
        let (aisha, bilal, chat) = group_of_two(&pool).await;
        prayed(&pool, aisha, Prayer::Asr).await;
        prayed(&pool, bilal, Prayer::Asr).await;

        check_chat(&state, &chat, utc("2026-06-15T20:01:00Z"))
            .await
            .unwrap();
        assert_eq!(sent(&gowa), []);
    }

    #[tokio::test]
    async fn a_group_in_two_places_names_each_place() {
        let pool = test_pool().await;
        let (state, gowa) = state_with_june(pool.clone()).await;
        let (_aisha, bilal, chat) = group_of_two(&pool).await;
        {
            // Bilal set Colombo in his DM.
            let mut conn = pool.acquire().await.unwrap();
            let dm = chats::find_or_create(
                &mut conn,
                "447700900002@s.whatsapp.net",
                Some(bilal),
                utc("2026-06-01T12:00:00Z"),
            )
            .await
            .unwrap();
            let colombo = colombo_settings();
            stored::set_location(
                &mut conn,
                dm,
                &colombo.city,
                &colombo.country,
                colombo.coordinates.unwrap(),
                colombo.timezone.name(),
            )
            .await
            .unwrap();
        }

        check_chat(&state, &chat, utc("2026-06-15T16:25:00Z"))
            .await
            .unwrap();
        let all = sent(&gowa);
        assert_eq!(all.len(), 2, "{all:?}");
        // Members come A to Z, so Aisha's place is first.
        assert!(
            all[0]
                .1
                .starts_with("@447700900001\n🕌 *Asr* has started in Milton Keynes (17:20)."),
            "{}",
            all[0].1
        );
        assert!(
            all[1]
                .1
                .starts_with("@447700900002\n🕌 *Asr* has started in Colombo (21:50)."),
            "{}",
            all[1].1
        );
    }

    #[tokio::test]
    async fn the_days_prayer_report_goes_once() {
        const DM: &str = "447700900001@s.whatsapp.net";
        let pool = test_pool().await;
        let (state, gowa) = state_with_june(pool.clone()).await;
        let chat = {
            let mut conn = pool.acquire().await.unwrap();
            let now = utc("2026-06-01T12:00:00Z");
            let user = register(&mut conn, "Bilal", "447700900001", now).await.id;
            let id = chats::find_or_create(&mut conn, DM, Some(user), now)
                .await
                .unwrap();
            drop(conn);
            for prayer in Prayer::ALL {
                prayed(&pool, user, prayer).await;
            }
            ReminderChat {
                id,
                wa_chat_id: DM.to_string(),
                dm_user_id: Some(user),
                delivery: Delivery::default(),
                overrides: Overrides::default(),
            }
        };

        // 15 June's Isha ended at midnight UTC (01:00 BST); then the next
        // tick, already claimed.
        check_chat(&state, &chat, utc("2026-06-16T00:05:00Z"))
            .await
            .unwrap();
        check_chat(&state, &chat, utc("2026-06-16T00:06:00Z"))
            .await
            .unwrap();
        let all = sent(&gowa);
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!(all[0].0, DM);
        assert_eq!(
            all[0].1,
            "🎉 You prayed *all 5 prayers* today (Mon 15 Jun). Masha'Allah, keep it up!"
        );
    }

    #[tokio::test]
    async fn a_groups_reports_sent_privately_skip_those_without_a_dm() {
        let pool = test_pool().await;
        let (state, gowa) = state_with_june(pool.clone()).await;
        let (aisha, bilal, mut chat) = group_of_two(&pool).await;
        chat.delivery = Delivery::Dm;
        for prayer in Prayer::ALL {
            prayed(&pool, aisha, prayer).await;
            prayed(&pool, bilal, prayer).await;
        }
        // Aisha has a DM with the bot (prayer reminders off there); Bilal
        // never messaged it privately, so the bot doesn't start one.
        const AISHA_DM: &str = "447700900001@s.whatsapp.net";
        {
            let mut conn = pool.acquire().await.unwrap();
            chats::find_or_create(
                &mut conn,
                AISHA_DM,
                Some(aisha),
                utc("2026-06-01T12:00:00Z"),
            )
            .await
            .unwrap();
        }

        check_chat(&state, &chat, utc("2026-06-16T00:05:00Z"))
            .await
            .unwrap();
        check_chat(&state, &chat, utc("2026-06-16T00:06:00Z"))
            .await
            .unwrap();
        let all = sent(&gowa);
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!(all[0].0, AISHA_DM);
        assert!(all[0].1.starts_with("🎉 You prayed"), "{}", all[0].1);
    }

    // ---- the plans, looked at directly: nothing is sent or claimed ----

    fn june_15() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 6, 15).unwrap()
    }

    /// The place key Milton Keynes' reminders are claimed under in a group.
    fn milton_keynes_key() -> String {
        let home = defaults();
        home.prayer_location(home.coordinates.unwrap()).key()
    }

    /// The group's one place (everyone on its settings), as `check_chat`
    /// works it out.
    async fn group_place(pool: &sqlx::SqlitePool, chat: &ReminderChat) -> Place {
        let mut conn = pool.acquire().await.unwrap();
        let members = stored::members(&mut conn, chat.id).await.unwrap();
        let mut places = places_for_group(&defaults(), &chat.overrides, &members);
        assert_eq!(places.len(), 1);
        places.remove(0)
    }

    /// The one message a reminder plan sends.
    fn only_message(plan: &Planned) -> &Outgoing {
        assert_eq!(plan.messages.len(), 1, "{plan:?}");
        &plan.messages[0]
    }

    #[tokio::test]
    async fn planning_a_start_reminder_sends_and_claims_nothing() {
        let pool = test_pool().await;
        let (state, gowa) = state_with_june(pool.clone()).await;
        let (aisha, bilal, chat) = group_of_two(&pool).await;
        let place = group_place(&pool, &chat).await;
        let now = utc("2026-06-15T16:25:00Z");

        let planned = plan_due(&state, &chat, &place, false, now).await.unwrap();
        assert_eq!(planned.len(), 1, "{planned:?}");
        assert_eq!(
            planned[0].claim,
            Claim::PrayerReminder {
                chat_id: chat.id,
                date: june_15(),
                prayer: Prayer::Asr,
                kind: ReminderKind::Start,
                place: milton_keynes_key(),
                prayer_time: utc("2026-06-15T16:20:00Z"),
            }
        );
        let message = only_message(&planned[0]);
        assert_eq!(message.to, GROUP);
        assert_eq!(message.mentions, ["447700900001", "447700900002"]);
        assert_eq!(message.on_sent, OnSent::KeepReminderMessage);
        let pushed: Vec<(i64, &str)> = message
            .push
            .iter()
            .map(|p| (p.user_id, p.key.as_str()))
            .collect();
        assert_eq!(
            pushed,
            [
                (aisha, "2026-06-15 asr start"),
                (bilal, "2026-06-15 asr start")
            ]
        );

        // Planning sent nothing and claimed nothing: planning again gives
        // the same.
        assert_eq!(sent(&gowa), []);
        assert_eq!(
            plan_due(&state, &chat, &place, false, now).await.unwrap(),
            planned
        );
    }

    #[tokio::test]
    async fn an_ending_plan_leaves_out_who_prayed() {
        let pool = test_pool().await;
        let (state, _gowa) = state_with_june(pool.clone()).await;
        let (aisha, bilal, chat) = group_of_two(&pool).await;
        let place = &group_place(&pool, &chat).await;
        prayed(&pool, bilal, Prayer::Asr).await;

        let planned = plan_due(&state, &chat, place, false, utc("2026-06-15T20:01:00Z"))
            .await
            .unwrap();
        assert_eq!(planned.len(), 1, "{planned:?}");
        let message = only_message(&planned[0]);
        assert_eq!(message.mentions, ["447700900001"]);
        assert!(
            message.text.contains("⏳ *Asr* ends in 19 min"),
            "{}",
            message.text
        );
        let pushed: Vec<i64> = message.push.iter().map(|p| p.user_id).collect();
        assert_eq!(pushed, [aisha]);

        // Once Aisha has prayed too, there's no plan at all.
        prayed(&pool, aisha, Prayer::Asr).await;
        let planned = plan_due(&state, &chat, place, false, utc("2026-06-15T20:01:00Z"))
            .await
            .unwrap();
        assert_eq!(planned, []);
    }

    #[test]
    fn a_dm_reminder_mentions_nobody_and_names_no_place() {
        let chat = ReminderChat {
            id: 7,
            wa_chat_id: "447700900001@s.whatsapp.net".to_string(),
            dm_user_id: Some(1),
            delivery: Delivery::default(),
            overrides: Overrides::default(),
        };
        let owner = Recipient {
            user_id: 1,
            phone: String::new(),
            name: String::new(),
        };
        let window = june(15).windows[2];
        let reminder = DueReminder {
            date: june_15(),
            kind: ReminderKind::Start,
            window,
        };
        let plan = reminder_plan(
            &chat,
            &defaults(),
            None,
            "",
            &reminder,
            &[owner],
            utc("2026-06-15T16:25:00Z"),
        );

        let Claim::PrayerReminder { place, .. } = &plan.claim else {
            panic!("a prayer reminder's claim: {:?}", plan.claim);
        };
        assert_eq!(place, "", "a DM's reminders aren't per place");
        let message = only_message(&plan);
        assert_eq!(message.to, chat.wa_chat_id);
        assert!(message.mentions.is_empty());
        assert!(
            message
                .text
                .starts_with("🕌 *Asr* has started (17:20). It ends at 21:20."),
            "{}",
            message.text
        );
        assert_eq!(message.push.len(), 1);
        assert_eq!(message.push[0].notice.title, "Asr has started");
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

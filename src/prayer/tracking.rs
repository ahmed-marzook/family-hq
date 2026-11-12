//! Missed prayers and prayer reports, on the reminder loop's clock.
//!
//! Each minute, for each place in each chat with reminders on:
//!
//! * A prayer whose time has ended, that the chat was reminded of, is
//!   marked missed for everyone it was for who hasn't checked in.
//! * Once a day's Isha has ended, that day's wrap-up goes out (if anyone
//!   prayed all five), and on a Sunday the week's summary, on a month's
//!   last day the month's.
//!
//! When reports are due, and turning them into a claim and messages, is the
//! shared `reports` module's; the reminder engine sends them. What
//! prayers' say is decided here by `messages`, a pure function.

use chrono::{DateTime, NaiveDate, Utc};
use tracing::info;

use crate::AppState;
use crate::error::AppError;
use crate::prayer::stats::{self, Entry, Summary};
use crate::prayer::{DayTimes, checkin};
use crate::reminders::engine::{self, Planned};
use crate::reminders::{Place, Recipient};
use crate::repo::prayers::{self, ReminderChat};
use crate::reports::{self, Delivery, Draft, Feature, Period, Report, ReportKind, To};

/// Marks missed prayers and sends due reports for one place in one chat.
pub async fn check(
    state: &AppState,
    chat: &ReminderChat,
    place: &Place,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    let settings = &place.settings;
    let location = state.apis.prayer_location(settings).await?;
    let is_group = chat.dm_user_id.is_none();
    // The same place key the reminders were claimed under.
    let place_key = if is_group {
        location.key()
    } else {
        String::new()
    };
    let today = settings.today(now);
    // Isha ends at Islamic midnight, often after midnight on the clock, so
    // yesterday can still have a prayer ending or a report due.
    for date in [today.pred_opt(), Some(today)].into_iter().flatten() {
        let day = state.apis.prayer_times.for_date(&location, date).await?;
        mark_missed(state, chat, place, &place_key, &day, now).await?;
        for kind in reports::due(date, stats::day_over(&day), now) {
            // Planned, then sent straight away, so each report goes out in
            // the same order as before (this day's before the next's).
            let Some(report) = plan_report(state, chat, place, &place_key, date, kind).await?
            else {
                continue;
            };
            let push = state.apis.push.as_ref();
            engine::deliver(&state.gowa, &state.db, push, vec![report], now).await;
        }
    }
    Ok(())
}

async fn mark_missed(
    state: &AppState,
    chat: &ReminderChat,
    place: &Place,
    place_key: &str,
    day: &DayTimes,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    let ended = stats::ended(day, now);
    if ended.is_empty() {
        return Ok(());
    }
    // `checkin::mark_missed` reads the log, then writes. A write
    // transaction (`BEGIN IMMEDIATE`) keeps anyone else (a reaction coming
    // in at the same moment) from changing it in between. Nothing in here
    // waits on the network, so it's held only for a few quick queries.
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    for prayer in ended {
        if !prayers::was_reminded(&mut tx, chat.id, day.date, prayer, place_key).await? {
            continue;
        }
        for r in &place.recipients {
            if checkin::mark_missed(&mut tx, r.user_id, day.date, prayer, now).await? {
                info!("{prayer} on {} missed by user {}", day.date, r.user_id);
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

/// What to send for one report. `people` are the place's recipients, each
/// with their log for the report's period. Pure, so tested directly.
///
/// * A DM: one message about its owner, saying "you".
/// * A group, `Here`: one message about everyone.
/// * A group, `Dm`: one message per person, to their DM.
/// * `Off`, or nothing to say (nobody prayed all five, or nobody has any
///   record for the period): nothing.
pub fn messages(
    kind: ReportKind,
    date: NaiveDate,
    delivery: Delivery,
    is_group: bool,
    people: &[(Recipient, Vec<Entry>)],
) -> Vec<Draft> {
    if delivery == Delivery::Off {
        return Vec::new();
    }
    let privately = is_group && delivery == Delivery::Dm;
    let to = |r: &Recipient| {
        if privately {
            To::Dm(r.user_id)
        } else {
            To::Chat
        }
    };

    match kind {
        ReportKind::Day => {
            let done: Vec<&Recipient> = people
                .iter()
                .filter(|(_, log)| stats::prayed_all(log, date))
                .map(|(r, _)| r)
                .collect();
            if is_group && !privately {
                if done.is_empty() {
                    return Vec::new();
                }
                let mentions: Vec<String> = done.iter().map(|r| r.phone.clone()).collect();
                return vec![Draft {
                    to: To::Chat,
                    text: stats::day_text(date, &mentions),
                    mentions,
                }];
            }
            done.iter()
                .map(|r| Draft {
                    to: to(r),
                    text: stats::day_text(date, &[]),
                    mentions: Vec::new(),
                })
                .collect()
        }
        ReportKind::Summary(span) => {
            let period = Period::of(span, date);
            // People with nothing in the log for the period are left out.
            let summaries: Vec<(&Recipient, Summary)> = people
                .iter()
                .map(|(r, log)| (r, Summary::new(period, log)))
                .filter(|(_, summary)| !summary.is_empty())
                .collect();
            if summaries.is_empty() {
                return Vec::new();
            }
            if is_group && !privately {
                let named: Vec<(Option<&str>, Summary)> = summaries
                    .into_iter()
                    .map(|(r, summary)| (Some(r.name.as_str()), summary))
                    .collect();
                return vec![Draft {
                    to: To::Chat,
                    text: stats::summary_text(span, period, &named),
                    mentions: Vec::new(),
                }];
            }
            summaries
                .into_iter()
                .map(|(r, summary)| Draft {
                    to: to(r),
                    text: stats::summary_text(span, period, &[(None, summary)]),
                    mentions: Vec::new(),
                })
                .collect()
        }
    }
}

/// One prayer report, ready for the engine to claim and send; `None` if
/// the chat turned its reports off (then not even claimed). Reads the
/// prayer log; sends and claims nothing.
async fn plan_report(
    state: &AppState,
    chat: &ReminderChat,
    place: &Place,
    place_key: &str,
    date: NaiveDate,
    kind: ReportKind,
) -> Result<Option<Planned>, AppError> {
    if chat.delivery == Delivery::Off {
        return Ok(None);
    }
    // Only for reading; back to the pool when this returns, before the
    // report is sent.
    let mut conn = state.db.acquire().await?;
    let period = Period::for_report(kind, date);
    let mut people = Vec::new();
    for r in &place.recipients {
        let log = prayers::log_between(&mut conn, r.user_id, period.first, period.last).await?;
        people.push((r.clone(), log));
    }
    let is_group = chat.dm_user_id.is_none();
    // Someone whose own DM has prayer reminders on gets that DM's reports;
    // a group's sent privately as well would be a copy.
    let mut outgoing = Vec::new();
    for message in messages(kind, date, chat.delivery, is_group, &people) {
        if let To::Dm(user_id) = message.to
            && prayers::dm_reminders_on(&mut conn, user_id).await?
        {
            continue;
        }
        outgoing.push(message);
    }
    let report = Report {
        feature: Feature::Prayers,
        kind,
        date,
        chat_id: chat.id,
        wa_chat_id: &chat.wa_chat_id,
        place: place_key,
    };
    Ok(Some(reports::plan(&mut conn, &report, outgoing).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prayer::Prayer;
    use crate::prayer::stats::Status;
    use crate::reports::Span;
    use crate::testkit::date;

    fn person(id: i64, name: &str) -> Recipient {
        Recipient {
            user_id: id,
            phone: format!("44770090000{id}"),
            name: name.to_string(),
        }
    }

    fn day_log(day: &str, prayed: usize) -> Vec<Entry> {
        Prayer::ALL
            .into_iter()
            .enumerate()
            .map(|(i, prayer)| Entry {
                date: date(day),
                prayer,
                status: if i < prayed {
                    Status::Prayed
                } else {
                    Status::Missed
                },
            })
            .collect()
    }

    /// Bilal prayed all five on Sunday 1 Nov, Khadija four.
    fn family() -> Vec<(Recipient, Vec<Entry>)> {
        vec![
            (person(1, "Bilal"), day_log("2026-11-01", 5)),
            (person(2, "Khadija"), day_log("2026-11-01", 4)),
        ]
    }

    const SUNDAY: &str = "2026-11-01";
    const WEEK: ReportKind = ReportKind::Summary(Span::Week);

    #[test]
    fn a_group_congratulates_only_those_who_prayed_all_five() {
        let sent = messages(
            ReportKind::Day,
            date(SUNDAY),
            Delivery::Here,
            true,
            &family(),
        );
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].to, To::Chat);
        assert_eq!(sent[0].mentions, ["447700900001"]);
        assert!(sent[0].text.contains("@447700900001."), "{}", sent[0].text);
    }

    #[test]
    fn nobody_praying_all_five_sends_nothing() {
        let people = vec![(person(2, "Khadija"), day_log(SUNDAY, 4))];
        assert!(messages(ReportKind::Day, date(SUNDAY), Delivery::Here, true, &people).is_empty());
        assert!(
            messages(
                ReportKind::Day,
                date(SUNDAY),
                Delivery::Here,
                false,
                &people
            )
            .is_empty()
        );
    }

    #[test]
    fn a_groups_week_names_everyone_in_one_message() {
        let sent = messages(WEEK, date(SUNDAY), Delivery::Here, true, &family());
        assert_eq!(sent.len(), 1);
        let text = &sent[0].text;
        assert!(text.contains("*Bilal*\n⬜⬜⬜⬜⬜⬜🟩\n"), "{text}");
        assert!(text.contains("*Khadija*\n⬜⬜⬜⬜⬜⬜🟨\n"), "{text}");
        assert!(text.contains("❌ Missed: Isha (Sun)"), "{text}");
        assert!(sent[0].mentions.is_empty(), "a summary doesn't ping anyone");
    }

    #[test]
    fn sent_privately_each_person_gets_their_own() {
        let sent = messages(WEEK, date(SUNDAY), Delivery::Dm, true, &family());
        let to: Vec<&To> = sent.iter().map(|m| &m.to).collect();
        assert_eq!(to, [&To::Dm(1), &To::Dm(2)]);
        assert!(!sent[0].text.contains("Bilal"), "it's about 'you'");
        assert!(!sent[1].text.contains("Bilal"), "nobody sees anyone else's");

        let day = messages(ReportKind::Day, date(SUNDAY), Delivery::Dm, true, &family());
        assert_eq!(day.len(), 1);
        assert_eq!(day[0].to, To::Dm(1));
        assert!(day[0].text.starts_with("🎉 You prayed"), "{}", day[0].text);
    }

    #[test]
    fn a_dm_report_goes_to_the_dm_itself() {
        let owner = vec![(person(1, ""), day_log(SUNDAY, 5))];
        let sent = messages(WEEK, date(SUNDAY), Delivery::Here, false, &owner);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].to, To::Chat);
        // In a DM "dm" means the same as "here".
        let same = messages(WEEK, date(SUNDAY), Delivery::Dm, false, &owner);
        assert_eq!(same, sent);
    }

    #[test]
    fn off_or_no_record_sends_nothing() {
        assert!(messages(WEEK, date(SUNDAY), Delivery::Off, true, &family()).is_empty());
        let unrecorded = vec![(person(1, "Bilal"), Vec::new())];
        assert!(messages(WEEK, date(SUNDAY), Delivery::Here, true, &unrecorded).is_empty());
    }

    #[test]
    fn people_with_no_record_are_left_out_of_a_group_summary() {
        let mut people = family();
        people.push((person(3, "Yusuf"), Vec::new()));
        let sent = messages(WEEK, date(SUNDAY), Delivery::Here, true, &people);
        assert!(!sent[0].text.contains("Yusuf"), "{}", sent[0].text);
    }

    // ---- `plan_report`: what goes to the engine ----

    use crate::db::test_pool;
    use crate::reminders::engine::Claim;
    use crate::repo::settings as stored;
    use crate::settings::Overrides;
    use crate::settings::tests::defaults;
    use crate::testkit::{Family, GROUP, seed_family, test_state, utc};

    /// The family's group as the loop sees it, reports going `delivery`,
    /// with Bilal and Khadija as its one place.
    fn group(family: &Family, delivery: Delivery) -> (ReminderChat, Place) {
        let chat = ReminderChat {
            id: family.group,
            wa_chat_id: GROUP.to_string(),
            dm_user_id: None,
            delivery,
            overrides: Overrides::default(),
        };
        let recipient = |user: &crate::repo::users::User| Recipient {
            user_id: user.id,
            phone: user.wa_jid.split_once('@').unwrap().0.to_string(),
            name: user.display_name.clone(),
        };
        let place = Place {
            settings: defaults(),
            recipients: vec![recipient(&family.bilal), recipient(&family.khadija)],
        };
        (chat, place)
    }

    /// Both prayed all five on Sunday 1 November.
    async fn both_prayed_all_five(state: &AppState, family: &Family) {
        let mut conn = state.db.acquire().await.unwrap();
        for user in [family.bilal.id, family.khadija.id] {
            for prayer in Prayer::ALL {
                checkin::tick(
                    &mut conn,
                    user,
                    date(SUNDAY),
                    prayer,
                    None,
                    utc("2026-11-01T22:00:00Z"),
                )
                .await
                .unwrap();
            }
        }
    }

    #[tokio::test]
    async fn reports_off_plan_nothing_not_even_a_claim() {
        let state = test_state(test_pool().await, "http://localhost:1");
        let family = {
            let mut conn = state.db.acquire().await.unwrap();
            seed_family(&mut conn, utc("2026-10-01T12:00:00Z")).await
        };
        both_prayed_all_five(&state, &family).await;
        let (chat, place) = group(&family, Delivery::Off);
        let planned = plan_report(&state, &chat, &place, "mk", date(SUNDAY), ReportKind::Day)
            .await
            .unwrap();
        assert_eq!(planned, None);
    }

    #[tokio::test]
    async fn sent_privately_a_dm_with_its_own_reminders_gets_no_copy() {
        let state = test_state(test_pool().await, "http://localhost:1");
        let family = {
            let mut conn = state.db.acquire().await.unwrap();
            seed_family(&mut conn, utc("2026-10-01T12:00:00Z")).await
        };
        both_prayed_all_five(&state, &family).await;
        let (chat, place) = group(&family, Delivery::Dm);

        // Bilal's DM is his only private chat; Khadija has none, so with
        // his DM's reminders off, only he gets the group's day report.
        let plan = plan_report(&state, &chat, &place, "mk", date(SUNDAY), ReportKind::Day)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            plan.claim,
            Claim::Report {
                chat_id: family.group,
                feature: Feature::Prayers,
                kind: ReportKind::Day,
                date: date(SUNDAY),
                place: "mk".to_string(),
            }
        );
        let to: Vec<&str> = plan.messages.iter().map(|m| m.to.as_str()).collect();
        assert_eq!(to, [family.bilal.wa_jid.as_str()]);

        // His DM has its own prayer reminders (and so its own reports): no
        // copy of the group's, but still a claim.
        {
            let mut conn = state.db.acquire().await.unwrap();
            stored::set_prayer_reminders(&mut conn, family.dm, true)
                .await
                .unwrap();
        }
        let plan = plan_report(&state, &chat, &place, "mk", date(SUNDAY), ReportKind::Day)
            .await
            .unwrap()
            .unwrap();
        assert!(plan.messages.is_empty(), "{plan:?}");
    }
}

//! The prayer commands: `/prayers` (today's times, with the sender's
//! ticks), `+prayed`, `/prayers on|off`, `/prayers week|month` and
//! `/prayers reports`. `bot` passes each one here.
//!
//! As for every command, the lookups that need the network (prayer times)
//! happen first, in `look_up_today` / `look_up_checkin`, called from
//! `bot::prepare`; the database work happens in `execute`, inside the
//! webhook's write transaction.

use chrono::NaiveDate;
use sqlx::SqliteConnection;
use tracing::warn;

use crate::Apis;
use crate::bot::{Context, chat_settings, my_settings};
use crate::error::AppError;
use crate::moon::Moon;
use crate::parser::{Checkin, Timing};
use crate::prayer::checkin::{self, Correction};
use crate::prayer::stats::{self, Status, Summary};
use crate::prayer::{self, DayTimes, Prayer};
use crate::repo::prayers;
use crate::repo::reports as stored_reports;
use crate::repo::settings as stored;
use crate::reports::{Delivery, Feature, Period, Span};
use crate::settings::Settings;

const PROBLEM: &str = "❓";
const MOSQUE: &str = "🕌";
const NOT_YET: &str = "⬜";
const BELL: &str = "🔔";
const BELL_OFF: &str = "🔕";
const MOON: &str = "🌙";
const CHART: &str = "📊";

/// What `bot::prepare` looked up for a prayer command, ready for `execute`.
pub enum Looked {
    /// `/prayers`: the sender's settings and today's times, or why there
    /// are none.
    Today {
        settings: Settings,
        today: Result<DayTimes, AppError>,
    },
    /// `+prayed`: the sender's settings, today's times and (when they can
    /// matter) yesterday's, or why there are none.
    Prayed {
        checkin: Checkin,
        settings: Settings,
        times: Result<(DayTimes, Option<DayTimes>), AppError>,
    },
}

/// `/prayers`, step one (in `bot::prepare`): today's times for the sender.
pub async fn look_up_today(
    conn: &mut SqliteConnection,
    ctx: &Context,
    apis: &Apis,
) -> Result<Looked, AppError> {
    let settings = my_settings(conn, ctx, &apis.prayer_times.defaults).await?;
    let today = lookup_day(apis, &settings, settings.today(ctx.now)).await;
    Ok(Looked::Today { settings, today })
}

/// `+prayed`, step one (in `bot::prepare`): the days' times it needs.
pub async fn look_up_checkin(
    conn: &mut SqliteConnection,
    ctx: &Context,
    apis: &Apis,
    checkin: Checkin,
) -> Result<Looked, AppError> {
    // For people who didn't catch the reminder: with no timing, the
    // same as reacting to it. Today (or yesterday) is in the
    // sender's own timezone.
    let settings = my_settings(conn, ctx, &apis.prayer_times.defaults).await?;
    let today = settings.today(ctx.now);
    let yesterday = today.pred_opt().unwrap_or(today);
    // Every lookup that needs the network happens here, before
    // anything is saved, so if a service is down nothing is ticked
    // and the reply says so.
    let lookups = async {
        let location = apis.prayer_location(&settings).await?;
        let today_times = apis.prayer_times.for_date(&location, today).await?;
        // Only looked up between midnight and Fajr, when last night's
        // Isha may still be going (see `prayer::checkin_date`), or
        // for `yesterday`.
        let yesterday_times = if checkin.yesterday || prayer::before_fajr(&today_times, ctx.now) {
            Some(apis.prayer_times.for_date(&location, yesterday).await?)
        } else {
            None
        };
        Ok::<_, AppError>((today_times, yesterday_times))
    };
    let times = lookups.await;
    Ok(Looked::Prayed {
        checkin,
        settings,
        times,
    })
}

/// Step two (in `bot::execute`): the database work and the reply, using
/// what step one looked up. No network here.
pub async fn execute(
    conn: &mut SqliteConnection,
    ctx: &Context,
    looked: Looked,
) -> Result<String, AppError> {
    match looked {
        Looked::Today { settings, today } => match today {
            Ok(day) => format_day(conn, ctx, &settings, &day).await,
            Err(err) => prayer_service_down(err),
        },
        Looked::Prayed {
            checkin,
            settings,
            times,
        } => prayed(conn, ctx, checkin, &settings, times).await,
    }
}

/// `/prayers on` / `off`: prayer reminders in this chat.
pub async fn reminders(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    on: bool,
) -> Result<String, AppError> {
    stored::set_prayer_reminders(conn, ctx.chat_id, on).await?;
    let (settings, _) = chat_settings(conn, ctx, defaults).await?;
    Ok(if on {
        if ctx.is_group {
            format!(
                "{BELL} Prayer reminders are on in this group: when each prayer starts, \
                 and before it ends. Each member is @mentioned on *their own* times \
                 (set yours by messaging me `/settings city …` privately; otherwise \
                 this group's {} times are used). React to a reminder once you've prayed.",
                settings.city
            )
        } else {
            format!(
                "{BELL} Prayer reminders are on ({}): when each prayer starts, and {} min \
                 before it ends. React to a reminder once you've prayed.",
                settings.city, settings.prayer_ending_minutes
            )
        }
    } else {
        format!("{BELL_OFF} Prayer reminders are off in this chat.")
    })
}

/// `/prayers week` / `month`: the sender's own summary.
pub async fn summary(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    span: Span,
) -> Result<String, AppError> {
    let settings = my_settings(conn, ctx, defaults).await?;
    let period = Period::of(span, settings.today(ctx.now));
    let log = prayers::log_between(conn, ctx.sender.id, period.first, period.last).await?;
    let summary = Summary::new(period, &log);
    Ok(if summary.is_empty() {
        let span = match span {
            Span::Week => "week",
            Span::Month => "month",
            Span::Year => "year",
        };
        format!(
            "{MOSQUE} Nothing recorded yet this {span}. Turn on reminders with \
             `/prayers on`, then react to each one once you've prayed."
        )
    } else {
        stats::summary_text(span, period, &[(None, summary)])
    })
}

/// `/prayers reports`, and (`Some`) `/prayers reports here|dm|off`.
pub async fn reports(
    conn: &mut SqliteConnection,
    ctx: &Context,
    change: Option<Delivery>,
) -> Result<String, AppError> {
    let delivery = match change {
        None => stored_reports::delivery(conn, ctx.chat_id, Feature::Prayers).await?,
        Some(delivery) => {
            // In a DM, "privately" is the same as "here".
            let delivery = if ctx.is_group || delivery != Delivery::Dm {
                delivery
            } else {
                Delivery::Here
            };
            stored_reports::set_delivery(conn, ctx.chat_id, Feature::Prayers, delivery).await?;
            delivery
        }
    };
    Ok(format_reports(delivery, ctx.is_group))
}

/// The prayer times for these settings on `date`: from the cache, or
/// AlAdhan (and, for a town without coordinates, the geocoder). Network,
/// so only ever called from `prepare`.
async fn lookup_day(
    apis: &Apis,
    settings: &Settings,
    date: NaiveDate,
) -> Result<DayTimes, AppError> {
    let location = apis.prayer_location(settings).await?;
    apis.prayer_times.for_date(&location, date).await
}

/// A day's prayer times, with the sender's ticks.
async fn format_day(
    conn: &mut SqliteConnection,
    ctx: &Context,
    settings: &Settings,
    day: &DayTimes,
) -> Result<String, AppError> {
    let date = day.date;
    let log: Vec<(Prayer, Status)> = prayers::log_between(conn, ctx.sender.id, date, date)
        .await?
        .into_iter()
        .map(|e| (e.prayer, e.status))
        .collect();
    Ok(format_prayer_day(day, settings, &log, Moon::at(ctx.now)))
}

/// `+prayed`, with the times `prepare` looked up: ticks each prayer, then
/// shows each day that changed.
async fn prayed(
    conn: &mut SqliteConnection,
    ctx: &Context,
    checkin: Checkin,
    settings: &Settings,
    times: Result<(DayTimes, Option<DayTimes>), AppError>,
) -> Result<String, AppError> {
    let (today_times, yesterday_times) = match times {
        Ok(times) => times,
        Err(err) => return prayer_service_down(err),
    };
    let today = settings.today(ctx.now);
    let yesterday = today.pred_opt().unwrap_or(today);
    let mut dates = Vec::new();
    for prayer in checkin.prayers {
        let date = if checkin.yesterday {
            yesterday
        } else {
            prayer::checkin_date(prayer, &today_times, yesterday_times.as_ref(), ctx.now)
        };
        if !dates.contains(&date) {
            dates.push(date);
        }
        let user = ctx.sender.id;
        match checkin.timing {
            Timing::Auto => {
                checkin::tick(conn, user, date, prayer, Some(ctx.chat_id), ctx.now).await?;
            }
            Timing::OnTime => {
                let said = Correction::OnTime;
                checkin::correct(conn, user, date, prayer, said, Some(ctx.chat_id), ctx.now)
                    .await?;
            }
            Timing::Late => {
                let said = Correction::Late;
                checkin::correct(conn, user, date, prayer, said, Some(ctx.chat_id), ctx.now)
                    .await?;
            }
        }
    }
    // Each day that changed, in order: usually one, but `+prayed
    // isha fajr` just after midnight ticks last night's Isha and
    // today's Fajr.
    // Each date is today's or yesterday's, and both days' times came
    // from `prepare` (`checkin_date` only picks yesterday when they're
    // there), so nothing is looked up here.
    dates.sort();
    let mut days = Vec::new();
    for date in dates {
        let day = [Some(&today_times), yesterday_times.as_ref()]
            .into_iter()
            .flatten()
            .find(|day| day.date == date)
            .ok_or_else(|| AppError::PrayerTimes(format!("no times looked up for {date}")))?;
        days.push(format_day(conn, ctx, settings, day).await?);
    }
    let level_ups = crate::points::level_ups::for_sender(conn, ctx, today).await?;
    if !level_ups.is_empty() {
        days.push(level_ups.join("\n"));
    }
    Ok(days.join("\n\n"))
}

/// What to say when a prayer-times lookup fails because a service is down
/// or answered with something unusable: AlAdhan (`Http`, `PrayerTimes`)
/// or the town lookup (`Geo`). The real error goes to the log.
///
/// Any other error, in practice the database, is given back as it is:
/// the webhook then answers 500 and GOWA retries, which can help there.
fn prayer_service_down(err: AppError) -> Result<String, AppError> {
    match err {
        err @ (AppError::Http(_) | AppError::PrayerTimes(_) | AppError::Geo(_)) => {
            warn!("prayer times lookup failed: {err}");
            Ok(format!(
                "{PROBLEM} The prayer-times service isn't answering right now. Try again in a bit."
            ))
        }
        other => Err(other),
    }
}

/// The `/prayers` reply: each prayer's time on the chat's own clock, marked
/// with how it went for the sender: ✅ prayed, 🕐 made up, ❌ missed.
fn format_prayer_day(
    day: &DayTimes,
    settings: &Settings,
    log: &[(Prayer, Status)],
    moon: Moon,
) -> String {
    let mut text = format!(
        "{MOSQUE} *Prayer times* ({}, {})\n",
        settings.city,
        day.date.format("%a %-d %b")
    );
    // "🌙 16 Jumada al-Ula 1448 AH · 🌖 Waning gibbous", or just the phase
    // if AlAdhan didn't send the date.
    let phase = moon.phase();
    let phase = format!("{} {phase}", phase.emoji());
    match day.hijri {
        Some(hijri) => text.push_str(&format!("{MOON} {hijri} · {phase}\n")),
        None => text.push_str(&format!("{phase}\n")),
    }
    // Numbered 1-5 in the day's order, which is what `+p 3` refers to.
    for (number, window) in (1..).zip(&day.windows) {
        let mark = log
            .iter()
            .find(|(prayer, _)| *prayer == window.prayer)
            .map_or(NOT_YET, |(_, status)| status.mark());
        text.push_str(&format!(
            "{mark} {number}. {} {}–{}\n",
            window.prayer,
            settings.clock(window.start),
            settings.clock(window.end)
        ));
    }
    text.push_str("\nTick one off with `+prayed asr` (or `+p 3`), or react to its reminder.");
    // Only worth saying once something is ❌ or 🕐.
    if log.iter().any(|(_, status)| *status != Status::Prayed) {
        text.push_str(
            "\nForgot to tick one in time? `+prayed asr on time` (or `late` if it was made up).",
        );
    }
    text
}

/// The `/prayers reports` reply: where they go now, and how to change it.
fn format_reports(delivery: Delivery, is_group: bool) -> String {
    let what = "each day's 🎉 for praying all 5, and on Sundays and at each month's end a \
                summary with the streak, misses and a heatmap";
    let now = match (delivery, is_group) {
        (Delivery::Off, _) => format!("{BELL_OFF} Prayer reports are off in this chat."),
        (Delivery::Here, true) => {
            format!("{CHART} Prayer reports go to this group: {what}, for everyone.")
        }
        (Delivery::Dm, true) => format!(
            "{CHART} Prayer reports go to each member privately: {what}. \
             (Only to people who have messaged me privately.)"
        ),
        (_, false) => format!("{CHART} Prayer reports come here: {what}."),
    };
    let change = if is_group {
        "Change with `/prayers reports here`, `dm` or `off`."
    } else {
        "Change with `/prayers reports here` or `off`."
    };
    format!("{now}\nThey need prayer reminders on (`/prayers on`). {change}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aladhan::PrayerTimes;
    use crate::bot::tests::{day, reply_to, setup, times};
    use crate::bot::{Outcome, run_command};
    use crate::db::test_pool;
    use crate::parser::BotCommand;
    use crate::repo::chats;
    use crate::settings::tests::defaults;

    #[test]
    fn prayer_day_ticks_what_was_prayed() {
        let text = format_prayer_day(
            &day(),
            &defaults(),
            &[
                (Prayer::Fajr, Status::Prayed),
                (Prayer::Asr, Status::Late),
                (Prayer::Dhuhr, Status::Missed),
            ],
            Moon { elongation: 205.0 },
        );
        assert!(
            text.starts_with(
                "🕌 *Prayer times* (Milton Keynes, Tue 27 Oct)\n\
                 🌙 16 Jumada al-Ula 1448 AH · 🌖 Waning gibbous\n"
            ),
            "{text}"
        );
        assert!(text.contains("✅ 1. Fajr 04:55–06:49\n"), "{text}");
        assert!(text.contains("❌ 2. Dhuhr 11:47–14:16\n"), "{text}");
        assert!(text.contains("🕐 3. Asr 14:16–16:44\n"), "{text}");
        assert!(text.contains("⬜ 5. Isha 18:31–23:46\n"), "{text}");
        assert!(text.contains("`+prayed asr on time`"), "{text}");
    }

    #[test]
    fn prayer_day_without_a_hijri_date_still_shows_the_phase() {
        let day = DayTimes {
            hijri: None,
            ..day()
        };
        let text = format_prayer_day(&day, &defaults(), &[], Moon { elongation: 0.0 });
        assert!(!text.contains("AH"), "{text}");
        assert!(text.contains(")\n🌑 New moon\n⬜ 1. Fajr"), "{text}");
    }

    #[tokio::test]
    async fn prayers_command_shows_the_senders_own_ticks() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        ctx.now = "2026-10-27T15:00:00Z".parse().unwrap();
        checkin::tick(
            &mut conn,
            ctx.sender.id,
            day().date,
            Prayer::Dhuhr,
            None,
            ctx.now,
        )
        .await
        .unwrap();

        let Outcome::Reply(text) =
            run_command(&mut conn, &ctx, &Apis::new(times()), BotCommand::Prayers)
                .await
                .unwrap()
        else {
            panic!("expected a reply");
        };
        assert!(text.contains("✅ 2. Dhuhr 11:47"), "{text}");
        assert!(text.contains("⬜ 3. Asr 14:16"), "{text}");
    }

    #[tokio::test]
    async fn prayer_reminders_on_and_off() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;

        let on = run_command(
            &mut conn,
            &ctx,
            &Apis::new(times()),
            BotCommand::PrayerReminders(true),
        )
        .await
        .unwrap();
        assert!(matches!(on, Outcome::Reply(t) if t.contains("on")));
        assert_eq!(prayers::reminder_chats(&mut conn).await.unwrap().len(), 1);

        run_command(
            &mut conn,
            &ctx,
            &Apis::new(times()),
            BotCommand::PrayerReminders(false),
        )
        .await
        .unwrap();
        assert!(prayers::reminder_chats(&mut conn).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn prayed_command_marks_today_and_shows_the_day() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        ctx.now = "2026-10-27T19:00:00Z".parse().unwrap();

        let command = BotCommand::Prayed(Checkin::today(vec![Prayer::Fajr, Prayer::Isha]));
        let Outcome::Reply(text) = run_command(&mut conn, &ctx, &Apis::new(times()), command)
            .await
            .unwrap()
        else {
            panic!("expected a reply");
        };
        assert!(text.contains("✅ 1. Fajr 04:55"), "{text}");
        assert!(text.contains("⬜ 3. Asr 14:16"), "{text}");
        assert!(text.contains("✅ 5. Isha 18:31"), "{text}");

        let mut prayed = prayers::prayed_on(&mut conn, ctx.sender.id, day().date)
            .await
            .unwrap();
        prayed.sort_by_key(|p| p.as_db_str());
        assert_eq!(prayed, [Prayer::Fajr, Prayer::Isha]);
    }

    #[tokio::test]
    async fn isha_ticked_after_midnight_counts_for_last_night() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        // 27 Oct's Isha runs to 00:30 on the 28th; it's 00:10.
        let mut last_night = day();
        last_night.windows[4].end = "2026-10-28T00:30:00Z".parse().unwrap();
        let mut today = day();
        today.date = NaiveDate::from_ymd_opt(2026, 10, 28).unwrap();
        today.windows[0].start = "2026-10-28T04:56:00Z".parse().unwrap();
        let apis = Apis::new(PrayerTimes::with_cached_days(
            defaults(),
            vec![last_night.clone(), today.clone()],
        ));
        ctx.now = "2026-10-28T00:10:00Z".parse().unwrap();

        let command = BotCommand::Prayed(Checkin::today(vec![Prayer::Isha]));
        let text = run_command(&mut conn, &ctx, &apis, command)
            .await
            .unwrap()
            .into_text();
        assert!(
            text.contains("Tue 27 Oct"),
            "shows last night's day: {text}"
        );
        assert!(text.contains("✅ 5. Isha"), "{text}");
        let user = ctx.sender.id;
        let last = prayers::prayed_on(&mut conn, user, last_night.date).await;
        assert_eq!(last.unwrap(), [Prayer::Isha]);
        let now = prayers::prayed_on(&mut conn, user, today.date).await;
        assert!(now.unwrap().is_empty());

        // Fajr named at the same time is today's: both days are shown.
        let command = BotCommand::Prayed(Checkin::today(vec![Prayer::Fajr]));
        let text = run_command(&mut conn, &ctx, &apis, command)
            .await
            .unwrap()
            .into_text();
        assert!(text.contains("Wed 28 Oct"), "{text}");
        assert_eq!(
            prayers::prayed_on(&mut conn, ctx.sender.id, today.date)
                .await
                .unwrap(),
            [Prayer::Fajr]
        );
    }

    #[tokio::test]
    async fn prayer_times_follow_the_chats_timezone() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        ctx.now = "2026-10-27T15:00:00Z".parse().unwrap();
        // Same city's times, read on a Karachi clock (UTC+5).
        stored::set_timezone(&mut conn, ctx.chat_id, "Asia/Karachi")
            .await
            .unwrap();
        let karachi = crate::settings::Settings {
            timezone: chrono_tz::Asia::Karachi,
            ..defaults()
        };
        let source = PrayerTimes::with_cached(karachi, day());

        let Outcome::Reply(text) =
            run_command(&mut conn, &ctx, &Apis::new(source), BotCommand::Prayers)
                .await
                .unwrap()
        else {
            panic!("expected a reply");
        };
        assert!(text.contains("3. Asr 19:16–21:44"), "{text}");
    }

    #[tokio::test]
    async fn prayers_in_a_group_use_the_senders_own_settings() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;
        ctx.now = "2026-10-27T15:00:00Z".parse().unwrap();

        // The sender's DM says Karachi time; the group has no settings.
        let dm = chats::find_or_create(
            &mut conn,
            "447700900001@s.whatsapp.net",
            Some(ctx.sender.id),
            ctx.now,
        )
        .await
        .unwrap();
        stored::set_timezone(&mut conn, dm, "Asia/Karachi")
            .await
            .unwrap();

        // Times for "Milton Keynes in Karachi time" come from the cache.
        let karachi = Settings {
            timezone: chrono_tz::Asia::Karachi,
            ..defaults()
        };
        let source = PrayerTimes::with_cached(karachi, day());
        let Outcome::Reply(text) =
            run_command(&mut conn, &ctx, &Apis::new(source), BotCommand::Prayers)
                .await
                .unwrap()
        else {
            panic!("expected a reply");
        };
        assert!(text.contains("3. Asr 19:16–21:44"), "{text}");
    }

    #[tokio::test]
    async fn prayers_reply_when_the_prayer_times_service_is_down() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        // Nothing cached, and AlAdhan answers 500 to everything.
        let down = crate::testkit::failing_service().await;
        let apis = Apis::new(PrayerTimes::new(defaults()).with_base_url(&down));

        let text = reply_to(&mut conn, &ctx, &apis, BotCommand::Prayers).await;
        assert!(
            text.contains("prayer-times service isn't answering"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn prayed_ticks_nothing_when_the_prayer_times_service_is_down() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        let down = crate::testkit::failing_service().await;
        let apis = Apis::new(PrayerTimes::new(defaults()).with_base_url(&down));

        let command = BotCommand::Prayed(Checkin::today(vec![Prayer::Fajr]));
        let text = reply_to(&mut conn, &ctx, &apis, command).await;
        assert!(
            text.contains("prayer-times service isn't answering"),
            "{text}"
        );
        let today = defaults().today(ctx.now);
        let prayed = prayers::prayed_on(&mut conn, ctx.sender.id, today)
            .await
            .unwrap();
        assert!(prayed.is_empty(), "{prayed:?}");
    }
}

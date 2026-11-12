//! The small commands with no feature folder of their own: `/weather` and
//! `/moon` (answered in `bot::prepare`, since they need the network) and
//! `/push` (the sender's ntfy notifications). Their services' own code,
//! and the text of the forecast and moon replies, are in `crate::weather`,
//! `crate::moon` and `crate::push`; what's here is the command around it.

use sqlx::SqliteConnection;
use tracing::warn;

use super::emoji::{BELL, PROBLEM};
use super::{Context, Offline, Outcome, Ran, my_settings};
use crate::Apis;
use crate::error::AppError;
use crate::moon;
use crate::parser::PushCommand;
use crate::push;
use crate::repo::push::{self as push_repo, Subscription};
use crate::weather::{self, When};

const MUTED: &str = "🔕";

/// `/weather`: the forecast for the sender's own city (their DM settings,
/// or this chat's), in their timezone.
pub(super) async fn weather(
    conn: &mut SqliteConnection,
    ctx: &Context,
    apis: &Apis,
    when: When,
) -> Result<String, AppError> {
    let settings = my_settings(conn, ctx, &apis.prayer_times.defaults).await?;
    // As in `shared_location`: `Geo` is "no such town", anything else is
    // the town lookup not answering.
    let at = match apis.coordinates(&settings).await {
        Ok(at) => at,
        Err(AppError::Geo(_)) => {
            return Ok(format!(
                "{PROBLEM} I couldn't find {}, {} for the weather. Check `/settings`.",
                settings.city, settings.country
            ));
        }
        Err(err) => {
            warn!("town lookup for the weather failed: {err}");
            return Ok(format!(
                "{PROBLEM} The town lookup isn't answering right now. Try again in a bit."
            ));
        }
    };
    match apis.weather.forecast(at, settings.timezone).await {
        Ok(forecast) => Ok(weather::format(&forecast, when, &settings.city)),
        Err(err) => {
            // Not "weather: {err}": `AppError::Weather`'s text already
            // starts with "weather:".
            warn!("forecast failed: {err}");
            Ok(format!(
                "{PROBLEM} The weather service isn't answering. Try again in a bit."
            ))
        }
    }
}

/// `/moon`: the phase now, today's moonrise and moonset in the sender's
/// town, and the next new and full moons. The phase is worked out here;
/// only the rise and set times need the network, and if that fails (or
/// the town can't be found) the rest still shows.
pub(super) async fn moon(
    conn: &mut SqliteConnection,
    ctx: &Context,
    apis: &Apis,
) -> Result<String, AppError> {
    let settings = my_settings(conn, ctx, &apis.prayer_times.defaults).await?;
    let today = settings.today(ctx.now);

    // A failed lookup is "not shown", not an error for the whole reply.
    let at = apis
        .coordinates(&settings)
        .await
        .inspect_err(|err| warn!("town lookup for the moon failed: {err}"))
        .ok();
    let rise_set = match at {
        Some(at) => apis
            .weather
            .moon_times(at, settings.timezone, today)
            .await
            .inspect_err(|err| warn!("moon times: {err}"))
            .ok(),
        None => None,
    };
    // The Hijri date comes with the prayer times (usually already cached).
    let hijri = match apis.prayer_location(&settings).await {
        Ok(location) => apis
            .prayer_times
            .for_date(&location, today)
            .await
            .inspect_err(|err| warn!("Hijri date for the moon failed: {err}"))
            .ok()
            .and_then(|day| day.hijri),
        Err(err) => {
            warn!("prayer location for the moon failed: {err}");
            None
        }
    };
    Ok(moon::format(ctx.now, &settings, rise_set, hijri))
}

/// `/push ...`: the sender's own push notifications (`crate::push`).
/// Personal, so only in their DM: in a group the reply (with the topic)
/// would be seen by everyone.
pub(super) async fn push(
    conn: &mut SqliteConnection,
    ctx: &Context,
    offline: &Offline<'_>,
    command: PushCommand,
) -> Result<Ran, AppError> {
    let reply = |text: String| Ok(Ran::Done(Outcome::Reply(text)));
    let Some(config) = offline.push else {
        return reply(not_set_up());
    };
    if ctx.is_group {
        return reply(format!(
            "{BELL} Push notifications are personal: send `/push on` in your own chat with me."
        ));
    }
    let user_id = ctx.sender.id;
    let text = match command {
        PushCommand::Status => match push_repo::subscription(conn, user_id).await? {
            Some(sub) => format!(
                "{BELL} Push notifications are on, on the topic `{}`. `/push test` sends one, \
                 `/push on` shows the setup steps again (for a new phone) and `/push off` stops them.",
                sub.topic
            ),
            None => format!(
                "{BELL} Push notifications are off. `/push on` sets them up: prayer \
                 reminders then come to the ntfy app as well, with a button to tick them off."
            ),
        },
        PushCommand::On => {
            let sub =
                push_repo::subscribe(conn, user_id, &push::new_subscription(), ctx.now).await?;
            // The steps, then the server and the topic on their own, so
            // each can be copied into the app as it is.
            return Ok(Ran::Done(Outcome::Several(push::subscribe_messages(
                config, &sub.topic,
            ))));
        }
        PushCommand::Off => {
            if push_repo::unsubscribe(conn, user_id).await? {
                format!(
                    "{MUTED} Push notifications are off. You can delete the subscription in the ntfy app."
                )
            } else {
                format!("{MUTED} Push notifications were already off.")
            }
        }
        PushCommand::Test => match push_repo::subscription(conn, user_id).await? {
            None => format!("{BELL} Push is off. Send `/push on` first."),
            // Sent by `Ran::finish`, once the transaction is saved.
            Some(sub) => return Ok(Ran::TestPush(sub)),
        },
    };
    reply(text)
}

fn not_set_up() -> String {
    format!("{BELL} Push notifications aren't set up on this server yet.")
}

/// `/push test`'s push, and the reply saying whether it went.
pub(super) async fn test_push(apis: &Apis, subscription: &Subscription) -> String {
    // `push` only asks for this when push is set up.
    let Some(ntfy) = &apis.push else {
        return not_set_up();
    };
    match ntfy
        .publish(&ntfy.message(subscription, &push::test_notice()))
        .await
    {
        Ok(()) => format!(
            "{BELL} Sent a test push. If it hasn't arrived, check the server and topic \
             in the ntfy app (`/push on` shows them again). Tap its button to check \
             it can reach me too."
        ),
        Err(err) => {
            warn!("test push failed: {err}");
            format!(
                "{PROBLEM} Couldn't send it: the push server didn't answer. Try again in a bit."
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aladhan::PrayerTimes;
    use crate::bot::tests::{reply_to, setup};
    use crate::db::test_pool;
    use crate::parser::BotCommand;
    use crate::settings::Settings;
    use crate::settings::tests::defaults;

    // ---- outside services that are down ----

    /// Settings with no saved coordinates, so `/weather` has to look the
    /// town up by name.
    fn by_name() -> Settings {
        Settings {
            coordinates: None,
            ..defaults()
        }
    }

    #[tokio::test]
    async fn weather_says_service_down_when_the_geocoder_fails() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        let mut apis = Apis::new(PrayerTimes::new(by_name()));
        apis.geo = crate::geo::Geo::with_geocoding_url(&crate::testkit::failing_service().await);

        let text = reply_to(&mut conn, &ctx, &apis, BotCommand::Weather(When::Now)).await;
        assert!(text.contains("isn't answering"), "{text}");
        assert!(!text.contains("couldn't find"), "{text}");
    }

    #[tokio::test]
    async fn weather_says_not_found_for_an_unknown_town() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;
        // The fake geocoder only knows Leicester, not Milton Keynes.
        let mut apis = Apis::new(PrayerTimes::new(by_name()));
        apis.geo = crate::geo::Geo::with_geocoding_url(&crate::testkit::fake_geocoder().await);

        let text = reply_to(&mut conn, &ctx, &apis, BotCommand::Weather(When::Now)).await;
        assert!(text.contains("couldn't find Milton Keynes, GB"), "{text}");
    }
}

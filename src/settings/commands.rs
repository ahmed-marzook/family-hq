//! The settings commands: `/settings` (show, change, `everyone`,
//! `methods`) and a location shared in a DM. `bot` passes each one here.
//!
//! As for every command, the lookups that need the network (a new town,
//! the nearest town to a shared point) happen first, in `look_up` /
//! `find_place`, called from `bot::prepare`; the saving happens in
//! `execute` / `save_place`, inside the webhook's write transaction.

use chrono_tz::Tz;
use sqlx::SqliteConnection;
use tracing::warn;

use crate::Apis;
use crate::bot::emoji::{BELL, PROBLEM};
use crate::bot::{Context, chat_settings};
use crate::error::AppError;
use crate::geo::{Coordinates, NearestPlace};
use crate::parser::SettingsCommand;
use crate::prayer;
use crate::repo::settings::{self as stored, MemberSettings};
use crate::settings::Settings;
use crate::settings::edit::{self as settings_edit, Change, Checked, Refused, Saved};

const MOSQUE: &str = "🕌";
const GEAR: &str = "⚙️";
const PIN: &str = "📍";
const CLOCK: &str = "🕰️";
const PEOPLE: &str = "👥";

/// What a `/settings` command does.
pub enum SettingsStep {
    Show,
    Everyone,
    Methods,
    /// A change, checked by `settings_edit::check`: what to save, or why
    /// it's refused.
    Change(Result<Checked, Refused>),
}

/// `/settings ...`, step one (in `prepare`): which kind it is, and for a
/// change, its checks, including looking a new town up.
pub async fn look_up(
    conn: &mut SqliteConnection,
    ctx: &Context,
    apis: &Apis,
    command: SettingsCommand,
) -> Result<SettingsStep, AppError> {
    // The changes are checked and saved by `settings_edit`, under the same
    // rules the admin dashboard uses; only the wording here is the bot's.
    let change = match command {
        SettingsCommand::Show => return Ok(SettingsStep::Show),
        SettingsCommand::Everyone => return Ok(SettingsStep::Everyone),
        SettingsCommand::Methods => return Ok(SettingsStep::Methods),
        SettingsCommand::Reset => Change::Reset,
        SettingsCommand::City { city, country } => {
            // No country typed: keep the one already in use.
            let country = match country {
                Some(country) => country,
                None => {
                    let defaults = &apis.prayer_times.defaults;
                    chat_settings(conn, ctx, defaults).await?.0.country
                }
            };
            Change::City { city, country }
        }
        SettingsCommand::Timezone(name) => Change::Timezone(name),
        SettingsCommand::PrayerMethod(method) => Change::PrayerMethod(method),
        SettingsCommand::PrayerSchool(school) => Change::PrayerSchool(school),
        SettingsCommand::PrayerEnding(minutes) => Change::PrayerEnding(minutes),
    };
    Ok(SettingsStep::Change(
        settings_edit::check(&apis.geo, &change).await,
    ))
}

/// Step two (in `execute`): saves a checked change, then the reply shows
/// the chat's full settings so people see the result.
pub async fn execute(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    step: SettingsStep,
) -> Result<String, AppError> {
    let id = ctx.chat_id;
    // `note` is an extra first line for the reply; most changes have none.
    let note = match step {
        SettingsStep::Show => None,
        SettingsStep::Everyone => {
            if !ctx.is_group {
                return Ok(format!("{PEOPLE} In a DM there's only you: see /settings."));
            }
            let members = stored::members(conn, id).await?;
            return Ok(format_everyone(&members, defaults));
        }
        SettingsStep::Methods => return Ok(format_methods()),
        // Refused: the explanation is the whole reply.
        SettingsStep::Change(Err(refused)) => return Ok(format_refused(refused)),
        SettingsStep::Change(Ok(checked)) => {
            let reset = checked == Checked::Reset;
            let (current, _) = chat_settings(conn, ctx, defaults).await?;
            match settings_edit::save(conn, id, &current, checked).await? {
                Saved::Town { name, timezone } => Some(format!(
                    "Found {name}. Timezone set to {timezone} to match."
                )),
                Saved::Done if reset => Some("Back to the defaults.".to_string()),
                Saved::Done => None,
            }
        }
    };

    // Read back what's now stored, so the reply shows exactly what's saved.
    let (settings, reminders) = chat_settings(conn, ctx, defaults).await?;
    let mut text = note.map(|n| format!("{n}\n\n")).unwrap_or_default();
    text.push_str(&format_settings(&settings, reminders, ctx.is_group));
    Ok(text)
}

/// Why a `/settings` change wasn't made, as the bot says it.
fn format_refused(refused: Refused) -> String {
    match refused {
        Refused::UnknownTown { city, country } => format!(
            "{PROBLEM} I couldn't find {city}, {country}. \
             Try e.g. `/settings city Leicester, GB`."
        ),
        Refused::UnknownTimezone(name) => format!(
            "{PROBLEM} I don't know the timezone {name}. Use a name like \
             Europe/London or Asia/Karachi."
        ),
        Refused::UnknownMethod(method) => {
            format!("{PROBLEM} There's no method {method}. /settings methods lists them.")
        }
        // The parser only ever gives Shafi'i or Hanafi, but just in case.
        Refused::UnknownSchool(_) => format!("{PROBLEM} Use `/settings asr hanafi` or `shafi`."),
        Refused::EndingOutOfRange(_) => format!(
            "{PROBLEM} Pick between {} and {} minutes.",
            settings_edit::ENDING_MINUTES.start(),
            settings_edit::ENDING_MINUTES.end()
        ),
    }
}

/// A location shared in a DM, step one (in `prepare`): the nearest town
/// (OpenStreetMap), and the timezone AlAdhan gives for that point. `Err`
/// is the reply when the town can't be worked out.
pub async fn find_place(
    conn: &mut SqliteConnection,
    ctx: &Context,
    apis: &Apis,
    at: Coordinates,
) -> Result<Result<(NearestPlace, Tz), String>, AppError> {
    let (current, _) = chat_settings(conn, ctx, &apis.prayer_times.defaults).await?;
    // `Geo` means OpenStreetMap answered but found no town there; any
    // other error means it didn't answer properly, which is worth a log
    // line and a different reply ("try again", not "try something else").
    let near = match apis.geo.nearest_place(at).await {
        Ok(near) => near,
        Err(AppError::Geo(_)) => {
            return Ok(Err(format!(
                "{PROBLEM} I couldn't work out which town that is. \
                 Try `/settings city Leicester, GB` instead."
            )));
        }
        Err(err) => {
            warn!("nearest town lookup failed: {err}");
            return Ok(Err(format!(
                "{PROBLEM} The map service isn't answering right now, so I couldn't \
                 work out which town that is. Try again in a bit."
            )));
        }
    };
    let timezone = apis
        .prayer_times
        .timezone_at(at, current.today(ctx.now))
        .await
        .unwrap_or(current.timezone);
    Ok(Ok((near, timezone)))
}

/// Step two (in `execute`): saves the exact point, named after the nearest
/// town, with that timezone. Prayer times are then fetched for the point
/// itself, so no name ever has to be looked up.
pub async fn save_place(
    conn: &mut SqliteConnection,
    ctx: &Context,
    defaults: &Settings,
    at: Coordinates,
    found: Result<(NearestPlace, Tz), String>,
) -> Result<String, AppError> {
    let (near, timezone) = match found {
        Ok(found) => found,
        Err(reply) => return Ok(reply),
    };
    // `names[0]` is the most specific: the town or village itself.
    let name = &near.names[0];
    stored::set_location(conn, ctx.chat_id, name, &near.country, at, timezone.name()).await?;

    let (settings, reminders) = chat_settings(conn, ctx, defaults).await?;
    Ok(format!(
        "{PIN} Got it: you're near *{name}, {}* ({timezone}). Prayer times, \
         reminders and weather now use your exact location.\n\n{}",
        near.country,
        format_settings(&settings, reminders, ctx.is_group)
    ))
}

/// The `/settings` reply.
fn format_settings(settings: &Settings, reminders: bool, is_group: bool) -> String {
    let whose = if is_group { "this group" } else { "your own" };
    let method = prayer::method_name(settings.prayer_method).unwrap_or("unknown");
    let reminders = if reminders {
        format!(
            "on (start, and {} min before the end)",
            settings.prayer_ending_minutes
        )
    } else {
        "off (`/prayers on`)".to_string()
    };
    let mut text = format!(
        "{GEAR} *Settings* ({whose})\n\
         {PIN} {}, {}\n\
         {CLOCK} {}\n\
         {MOSQUE} Method {}: {method}; Asr {}\n\
         {BELL} Prayer reminders: {reminders}\n\n\
         Change with `/settings city …`, `timezone …`, `method …`, \
         `school hanafi|shafi`, `ending 20` or `reset`.",
        settings.city,
        settings.country,
        settings.timezone,
        settings.prayer_method,
        prayer::school_name(settings.prayer_school),
    );
    if is_group {
        text.push_str(
            "\nYour own settings: message me privately. `/settings everyone` shows each member's.",
        );
    }
    text
}

/// The `/settings everyone` reply: each member's personal settings.
fn format_everyone(members: &[MemberSettings], defaults: &Settings) -> String {
    let mut text = format!("{PEOPLE} *Members' own settings* (from their chats with me)\n");
    for member in members {
        let line = match &member.settings {
            None => "defaults".to_string(),
            Some(own) => {
                let s = own.overrides.apply(defaults);
                let reminders = if own.prayer_reminders {
                    "reminders on"
                } else {
                    "reminders off"
                };
                format!(
                    "{}, {} · {} · method {} · {} · {reminders}",
                    s.city,
                    s.country,
                    s.timezone,
                    s.prayer_method,
                    prayer::school_name(s.prayer_school)
                )
            }
        };
        text.push_str(&format!("• {}: {line}\n", member.display_name));
    }
    text
}

/// The `/settings methods` reply.
fn format_methods() -> String {
    let mut text = format!("{MOSQUE} *Prayer calculation methods*\n");
    for (id, name) in prayer::METHODS {
        text.push_str(&format!("{id}. {name}\n"));
    }
    text.push_str("\nChoose with `/settings method 3`.");
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::tests::{setup, times};
    use crate::bot::{Outcome, run_command};
    use crate::db::test_pool;
    use crate::parser::BotCommand;

    async fn settings_reply(
        conn: &mut SqliteConnection,
        ctx: &Context,
        command: SettingsCommand,
    ) -> String {
        match run_command(
            conn,
            ctx,
            &Apis::new(times()),
            BotCommand::Settings(command),
        )
        .await
        .unwrap()
        {
            Outcome::Reply(text) => text,
            other => panic!("expected a reply, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn settings_show_the_defaults_then_changes() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;

        let text = settings_reply(&mut conn, &ctx, SettingsCommand::Show).await;
        assert!(text.contains("*Settings* (this group)"), "{text}");
        assert!(text.contains("📍 Milton Keynes, GB"), "{text}");
        assert!(text.contains("Europe/London"), "{text}");
        assert!(
            text.contains("Method 3: Muslim World League; Asr Shafi'i"),
            "{text}"
        );
        assert!(text.contains("reminders: off"), "{text}");

        settings_reply(
            &mut conn,
            &ctx,
            SettingsCommand::PrayerSchool(prayer::HANAFI),
        )
        .await;
        settings_reply(&mut conn, &ctx, SettingsCommand::PrayerMethod(2)).await;
        let text = settings_reply(
            &mut conn,
            &ctx,
            SettingsCommand::Timezone("Asia/Karachi".into()),
        )
        .await;
        assert!(text.contains("Asia/Karachi"), "{text}");
        assert!(
            text.contains("Method 2: Islamic Society of North America (ISNA); Asr Hanafi"),
            "{text}"
        );

        let text = settings_reply(&mut conn, &ctx, SettingsCommand::Reset).await;
        assert!(text.starts_with("Back to the defaults."), "{text}");
        assert!(text.contains("Method 3"), "{text}");
    }

    #[tokio::test]
    async fn bad_settings_are_explained_and_not_saved() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let ctx = setup(&mut conn).await;

        let text = settings_reply(&mut conn, &ctx, SettingsCommand::PrayerMethod(6)).await;
        assert!(text.contains("no method 6"), "{text}");
        let text = settings_reply(
            &mut conn,
            &ctx,
            SettingsCommand::Timezone("Mars/Olympus".into()),
        )
        .await;
        assert!(text.contains("don't know the timezone"), "{text}");
        let text = settings_reply(&mut conn, &ctx, SettingsCommand::PrayerEnding(0)).await;
        assert!(text.contains("between 1 and 120"), "{text}");

        let stored = stored::get(&mut conn, ctx.chat_id).await.unwrap();
        assert_eq!(stored.overrides, crate::settings::Overrides::default());
    }

    #[tokio::test]
    async fn everyone_lists_members_and_is_group_only() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let mut ctx = setup(&mut conn).await;

        let text = settings_reply(&mut conn, &ctx, SettingsCommand::Everyone).await;
        assert!(text.contains("• Bilal: defaults"), "{text}");
        assert!(text.contains("• Khadija: defaults"), "{text}");

        ctx.is_group = false;
        let text = settings_reply(&mut conn, &ctx, SettingsCommand::Everyone).await;
        assert!(text.contains("only you"), "{text}");
    }

    #[tokio::test]
    async fn methods_lists_every_method() {
        let text = format_methods();
        assert!(text.contains("3. Muslim World League\n"));
        assert!(text.contains("23. Ministry of Awqaf, Jordan\n"));
        assert!(!text.contains("\n6. "));
    }
}

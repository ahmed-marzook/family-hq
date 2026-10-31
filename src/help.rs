//! The `/help` pages: an overview, and one page per topic with examples.
//!
//! Plain text only, so it's easy to keep up to date: when a command
//! changes, change its page here. Pure functions, no database.

/// The topics `/help <topic>` knows, and the words that lead to each.
const TOPICS: [(&str, &[&str]); 6] = [
    ("shop", &["shop", "shopping", "buy", "got", "list"]),
    ("todo", &["todo", "todos", "task", "tasks"]),
    (
        "prayers",
        &["prayers", "prayer", "prayed", "salah", "salat", "namaz"],
    ),
    (
        "settings",
        &["settings", "setting", "city", "timezone", "location"],
    ),
    ("weather", &["weather", "forecast", "rain"]),
    ("moon", &["moon", "hijri", "islamic", "month", "crescent"]),
];

const OVERVIEW: &str = "🤖 *Family HQ help*
I keep shared lists for each group I'm in, and private ones when you message me directly. Commands start with + (add) or / (show or change).

🛒 *Shopping*: `+shop milk, eggs` · `/shop` · `+got milk`
📝 *Todos*: `+todo fix the tap` · `/todo` · `+done 2` or `+done gym`
🕌 *Prayers*: `/prayers` · `/prayers on` · `+prayed asr`
⚙️ *Settings*: `/settings` · `/settings city Leicester, GB` · or share your location with me
🌤️ *Weather*: `/weather` · `/weather hourly` · `/weather tomorrow` · `/weather week`
🌙 *Moon*: `/moon`

Short forms: `+s` shop · `+g` got · `+t` todo · `+d` done · `+p` prayed · `/s` · `/t` · `/p` · `/w` · `/m` · `/h`

For the full page on a topic, with examples:
`/help shop` · `/help todo` · `/help prayers` · `/help settings` · `/help weather` · `/help moon`

I react 👍 to every command I understand. No reaction means I didn't recognise it.";

const SHOP: &str = "🛒 *Help: shopping list*
Each group has its own list; in a DM with me it's your private list.

*Add items*
`+shop milk` · `+shop milk, eggs, bread` (commas or new lines between items)
`+buy` works too. Items already on the list are skipped.
\"and\" doesn't split items: `+shop mac and cheese` is one item.

*See the list*
`/shop`: numbered list of what's left to buy, plus how many are bought.

*Tick items off*
• Reply (swipe) to a `/shop` list with the numbers: `1 3`
• Or without replying: `+got milk, eggs` (by name) or `+got 1 3` (numbers from the latest `/shop` list here). `+bought` works too.
• Reply `remove 2` to a list to delete an item instead.
In a group, I @mention whoever bought it (\"✅ @Bilal bought *eggs*\"), so everyone knows it's done.

*Tidy up*
`/shop clear`: empties the whole list.
`/shop clear bought`: removes only the ticked-off items.";

const TODO: &str = "📝 *Help: todos*
Each group has its own todos; in a DM with me they're your private ones.

*Add*
`+todo fix the tap`: assigned to you.
`+todo fix the tap, call the school, bins`: several at once (commas or new lines).
`+todo @Khadija book dentist`: assigned to someone in this group. Names are what I show in `/settings everyone`; people appear once they've sent a message here.

*See them*
`/todo`: a progress bar (and how much of your day is left), then everyone's todos under their name: ⬜ still to do, ✅ done (with when, and who if someone else did it).
`/todo all`: the same todos as one list, in order, with who each is for.
`/todo Khadija` (or `/todo @Khadija`): just one person's, with their own progress. `/todo mine` for yours.
Every view uses the same numbers, so `+done 5` is the same todo whichever you looked at.

*Reminders*
`/todo reminders on`: if you have open todos here, I'll remind you at 08:00, 13:00, 18:00 and 22:00 on your own clock. Off unless you turn it on, and each person chooses for themselves. `/todo reminders off` stops them.

*Finish one*
• Reply to a `/todo` list: `done 2`, or `1 3` for several.
• Or any time, without replying: `+done 2` (`+d 2`) using the numbers from the latest `/todo`, or `+done gym` using part of the title.
The reply shows the updated progress bar.

*Delete*
Reply `remove 2` to a `/todo` list.
`/todo clear done`: removes the ✅ completed todos.
`/todo clear`: empties the whole list.

The numbers always mean the list you replied to, even if it's old.";

const PRAYERS: &str = "🕌 *Help: prayers*
*Today's times*
`/prayers`: today's Islamic date and moon phase, then each prayer's start and end time, ✅ for the ones you've prayed.

*Reminders*
`/prayers on`: in this chat, a reminder when each prayer starts, and another shortly before it ends (20 min by default). `/prayers off` stops them.
In a group, everyone is @mentioned on *their own* times: someone abroad gets their own reminder at their local time. Set yours by messaging me privately (`/settings city …`, or just share your location). People who haven't use the group's settings.
The \"ending soon\" reminder only goes to people who haven't prayed yet.

*Mark a prayer as prayed*
• React to its reminder with any emoji (remove the reaction to undo). I'll reply to confirm, with how many you've prayed that day.
• Or type `+prayed asr`. Several: `+prayed fajr, dhuhr`. All five: `+prayed all`. Spellings like zuhr, asar, esha work too.
• Or by number, 1 (Fajr) to 5 (Isha), as shown in `/prayers`: `+p 3` is Asr, `+p 1 3` is Fajr and Asr.

*When prayers end*
Fajr ends at sunrise, Dhuhr at Asr, Asr at Maghrib, Maghrib at Isha, and Isha at Islamic midnight.

City, calculation method, Asr school and reminder timing: see `/help settings`.";

const SETTINGS: &str = "⚙️ *Help: settings*
Settings belong to a chat: in a DM with me they're *your own*; in a group they're shared by the group. Anything you don't set uses the defaults.

`/settings`: show this chat's settings.
`/settings everyone`: in a group, each member's own settings.

*Location and time*
📎 *Share your location* with me (in a private chat): I'll use the nearest town and its timezone. The easiest way.
`/settings city Leicester, GB`: sets the city and its timezone to match. Leave out the country to keep the current one.
`/settings timezone Europe/London`: only if the city's timezone is wrong for you.

*Prayers*
`/settings method 3`: calculation method. `/settings methods` lists them (3 = Muslim World League).
`/settings school hanafi` or `/settings school shafi`: how Asr is calculated.
`/settings ending 20`: how many minutes before a prayer ends the last reminder comes.

`/settings reset`: back to the defaults (reminders stay on or off).";

const WEATHER: &str = "🌤️ *Help: weather*
`/weather`: the weather now and the rest of today, how far through the season and the year we are, and sunrise, sunset and daylight.
`/weather hourly`: the next 12 hours, with the chance of rain.
`/weather tomorrow` (or `tmr`): tomorrow's forecast.
`/weather week`: the next seven days.

It's for *your* town: the one in your own settings (message me `/settings city …` or share your location privately), or this group's if you haven't set one. Seasons are your town's: the other way round south of the equator. In the tropics they're its usual wet and dry seasons (hot and cooler in a desert), from the last 10 years of weather there.";

const MOON: &str = "🌙 *Help: moon*
`/moon` (or `/m`): the moon's phase and how much of it is lit, today's Islamic date, moonrise and moonset in your town, and when the next new and full moons are.

`/prayers` shows the Islamic date and phase too.

The Islamic date comes from AlAdhan's calculated calendar, so it can be a day off from the moon sighting your mosque follows. A new Islamic month starts once the new crescent is seen, usually a day or two after the new moon.";

/// The topic a typed word leads to ("namaz" → "prayers"), if any.
pub fn topic(word: &str) -> Option<&'static str> {
    let word = word.trim().to_lowercase();
    // Find the topic whose list of words contains what was typed.
    TOPICS
        .iter()
        .find(|(_, words)| words.contains(&word.as_str()))
        .map(|(name, _)| *name)
}

/// The help text for `/help` (no topic) or `/help <topic>`.
pub fn page(topic: Option<&str>) -> String {
    let Some(topic) = topic else {
        return OVERVIEW.to_string();
    };
    match self::topic(topic) {
        Some("shop") => SHOP.to_string(),
        Some("todo") => TODO.to_string(),
        Some("prayers") => PRAYERS.to_string(),
        Some("settings") => SETTINGS.to_string(),
        Some("weather") => WEATHER.to_string(),
        Some("moon") => MOON.to_string(),
        _ => format!(
            "❓ There's no help page for \"{topic}\". Try `/help shop`, `/help todo`, \
             `/help prayers`, `/help settings`, `/help weather` or `/help moon`."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_lists_every_topic() {
        let text = page(None);
        for (name, _) in TOPICS {
            assert!(
                text.contains(&format!("`/help {name}`")),
                "overview misses {name}"
            );
        }
    }

    #[test]
    fn each_topic_and_its_aliases_find_their_page() {
        assert!(page(Some("shop")).starts_with("🛒 *Help: shopping list*"));
        assert!(page(Some("Shopping")).starts_with("🛒 *Help: shopping list*"));
        assert!(page(Some("todos")).starts_with("📝 *Help: todos*"));
        assert!(page(Some("namaz")).starts_with("🕌 *Help: prayers*"));
        assert!(page(Some("timezone")).starts_with("⚙️ *Help: settings*"));
        assert!(page(Some("hijri")).starts_with("🌙 *Help: moon*"));
    }

    #[test]
    fn unknown_topic_points_to_the_real_ones() {
        let text = page(Some("cooking"));
        assert!(text.contains("no help page for \"cooking\""), "{text}");
        assert!(text.contains("`/help shop`"));
    }

    #[test]
    fn pages_mention_the_commands_they_explain() {
        assert!(page(Some("shop")).contains("+got"));
        assert!(page(Some("todo")).contains("remove 2"));
        assert!(page(Some("prayers")).contains("+prayed all"));
        assert!(page(Some("settings")).contains("/settings everyone"));
        assert!(page(Some("moon")).contains("`/moon` (or `/m`)"));
    }
}

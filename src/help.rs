//! The `/help` pages: an overview, and one page per topic with examples.
//!
//! Plain text only, so it's easy to keep up to date: when a command
//! changes, change its page here. Pure functions, no database.

/// The topics `/help <topic>` knows, and the words that lead to each.
const TOPICS: [(&str, &[&str]); 5] = [
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
];

const OVERVIEW: &str = "🤖 *Family HQ help*
I keep shared lists for each group I'm in, and private ones when you message me directly. Commands start with + (add) or / (show or change).

🛒 *Shopping*: `+shop milk, eggs` · `/shop` · `+got milk`
📝 *Todos*: `+todo fix the tap` · `/todo` · reply `done 2`
🕌 *Prayers*: `/prayers` · `/prayers on` · `+prayed asr`
⚙️ *Settings*: `/settings` · `/settings city Leicester, GB` · or share your location with me
🌤️ *Weather*: `/weather` · `/weather tomorrow` · `/weather week`

Short forms: `+s` shop · `+g` got · `+t` todo · `+p` prayed · `/s` · `/t` · `/p` · `/w` · `/h`

For the full page on a topic, with examples:
`/help shop` · `/help todo` · `/help prayers` · `/help settings` · `/help weather`

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
`+todo @Khadija book dentist`: assigned to someone in this group. Names are what I show in `/settings everyone`; people appear once they've sent a message here.

*See them*
`/todo`: numbered list of open todos, with who each is for.

*Finish or delete* (reply to a `/todo` list)
`done 2`: marks todo 2 as done. `1 3` marks several.
`remove 2`: deletes todo 2.
The numbers always mean the list you replied to, even if it's old.";

const PRAYERS: &str = "🕌 *Help: prayers*
*Today's times*
`/prayers`: each prayer's start and end time, ✅ for the ones you've prayed.

*Reminders*
`/prayers on`: in this chat, a reminder when each prayer starts, and another shortly before it ends (20 min by default). `/prayers off` stops them.
In a group, everyone is @mentioned on *their own* times: someone abroad gets their own reminder at their local time. Set yours by messaging me privately (`/settings city …`, or just share your location). People who haven't use the group's settings.
The \"ending soon\" reminder only goes to people who haven't prayed yet.

*Mark a prayer as prayed*
• React to its reminder with any emoji (remove the reaction to undo).
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
`/weather`: the weather now, and the rest of today.
`/weather tomorrow` (or `tmr`): tomorrow's forecast.
`/weather week`: the next seven days.

It's for *your* town: the one in your own settings (message me `/settings city …` or share your location privately), or this group's if you haven't set one.";

/// The help text for `/help` (no topic) or `/help <topic>`.
pub fn page(topic: Option<&str>) -> String {
    let Some(topic) = topic else {
        return OVERVIEW.to_string();
    };
    let word = topic.trim().to_lowercase();

    // Find the topic whose list of words contains what was typed.
    let found = TOPICS
        .iter()
        .find(|(_, words)| words.contains(&word.as_str()))
        .map(|(name, _)| *name);

    match found {
        Some("shop") => SHOP.to_string(),
        Some("todo") => TODO.to_string(),
        Some("prayers") => PRAYERS.to_string(),
        Some("settings") => SETTINGS.to_string(),
        Some("weather") => WEATHER.to_string(),
        _ => format!(
            "❓ There's no help page for \"{topic}\". Try `/help shop`, `/help todo`, \
             `/help prayers`, `/help settings` or `/help weather`."
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
    }
}

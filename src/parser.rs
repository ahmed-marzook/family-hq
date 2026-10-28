//! Turns the text of a WhatsApp message into a command for the bot.
//!
//! A pure function: text in, `Option` out. No database, no network, no
//! clock, so every case can be tested in a millisecond.
//!
//! Commands start with `+` (add something) or `/` (show or change a list).
//! Anything else is ordinary chat and parses to `None`, so the bot stays
//! quiet in the family group unless someone is talking to it.

use crate::person::Assignee;
use crate::prayer::{HANAFI, Prayer, SHAFII};
use crate::weather::When;

/// A command typed as a normal message.
#[derive(Debug, PartialEq)]
pub enum BotCommand {
    /// `+shop milk, eggs` or `+buy milk`
    AddShop(Vec<String>),
    /// `/shop`
    ListShop,
    /// `/shop clear` (the whole list) or `/shop clear bought` (only the
    /// ticked-off items)
    ClearShop { bought_only: bool },
    /// `+got milk, eggs` or `+got 1 3` (also `+bought`): tick items off
    /// without replying to a list
    GotShop(Got),
    /// `+todo fix the tap`, `+todo @Khadija book dentist`, or several at
    /// once: `+todo fix the tap, call the school` (one `@name` is for all)
    AddTodo {
        titles: Vec<String>,
        /// The parser only ever sets `Assignee::Mentioned` (from `@name`),
        /// or `None` when no name was typed. It can't know who sent the
        /// message, so the webhook replaces `None` with `Assignee::Sender`.
        assignee: Option<Assignee>,
    },
    /// `/todo`
    ListTodos(TodoView),
    /// `+done 2`, `+done 1 3` (numbers from the latest `/todo` list) or
    /// `+done gym` (by title): complete todos without replying
    DoneTodo(Got),
    /// `/todo clear` (everything) or `/todo clear done` (only completed)
    ClearTodos { done_only: bool },
    /// `/prayers`: today's times, and which you've prayed
    Prayers,
    /// `/prayers on` (true) / `/prayers off` (false): reminders in this chat
    PrayerReminders(bool),
    /// `+prayed asr` or `+prayed fajr, dhuhr` or `+prayed all`: mark
    /// prayers as prayed today without reacting to a reminder
    Prayed(Vec<Prayer>),
    /// `/settings ...`: show or change this chat's settings
    Settings(SettingsCommand),
    /// `/weather`, `/weather hourly`, `/weather tomorrow`, `/weather week`
    Weather(When),
    /// `/moon`: today's phase, moonrise and moonset, the next new and full
    Moon,
    /// A location shared in WhatsApp (📎 → Location). Not typed: the
    /// webhook builds this from a location message in a DM.
    SharedLocation { latitude: f64, longitude: f64 },
    /// `/help` (overview) or `/help shop` (one topic's page)
    Help(Option<String>),
}

/// Which todos `/todo` shows, and how. The numbers are the same in every
/// view, so `+done 5` means the same todo whichever one was sent last.
#[derive(Debug, Clone, PartialEq)]
pub enum TodoView {
    /// `/todo`: grouped under each person's name.
    ByPerson,
    /// `/todo all`: one list, in order.
    All,
    /// `/todo mine`: the sender's own.
    Mine,
    /// `/todo Bilal` or `/todo @Bilal`: one person's, by the name as typed.
    Person(String),
}

/// What a `/settings` message asks for. Values are only parsed here; the
/// handler checks them (a real method number, a known city) so it can
/// reply with a helpful message rather than staying silent.
#[derive(Debug, PartialEq)]
pub enum SettingsCommand {
    /// `/settings`
    Show,
    /// `/settings everyone`: each member's personal settings
    Everyone,
    /// `/settings reset`
    Reset,
    /// `/settings methods`: the prayer calculation methods to choose from
    Methods,
    /// `/settings city Leicester, GB` (country optional)
    City {
        city: String,
        country: Option<String>,
    },
    /// `/settings timezone Europe/London`
    Timezone(String),
    /// `/settings method 2`
    PrayerMethod(u8),
    /// `/settings school hanafi` / `shafi`
    PrayerSchool(u8),
    /// `/settings ending 20`
    PrayerEnding(i64),
}

/// What `+got` refers to.
#[derive(Debug, PartialEq)]
pub enum Got {
    /// `+got milk, eggs`: items on the list, by name.
    Names(Vec<String>),
    /// `+got 1 3`: numbers from the most recent `/shop` list in the chat.
    Numbers(Vec<usize>),
}

/// A reply (swipe-reply) to one of the bot's numbered lists. The numbers
/// refer to that list message, so which list it is (shop or todo) comes
/// from the message being replied to, not from this text.
#[derive(Debug, PartialEq)]
pub enum ReplyCommand {
    /// `1 3` or `1, 3`: tick off those items
    Tick(Vec<usize>),
    /// `done 2`
    Done(usize),
    /// `remove 2`
    Remove(usize),
}

/// Parses a normal message. `None` means "not for the bot".
pub fn parse(text: &str) -> Option<BotCommand> {
    // Same idea as `split_first` in the CLI, but on one string:
    // "+shop milk, eggs" → ("+shop", "milk, eggs").
    // `split_once` splits at the first whitespace (space or newline) and
    // returns `None` if there is none, i.e. the message is a single word.
    let text = text.trim();
    let (first, rest) = match text.split_once(char::is_whitespace) {
        Some((first, rest)) => (first, rest.trim()),
        None => (text, ""),
    };

    let first = first.to_lowercase();
    match expand_shorthand(&first) {
        "+shop" | "+buy" => parse_items(rest).map(BotCommand::AddShop),
        "+got" | "+bought" => parse_got(rest).map(BotCommand::GotShop),
        "+done" => parse_got(rest).map(BotCommand::DoneTodo),
        "+todo" => parse_add_todo(rest),
        "+prayed" => parse_prayed(rest).map(BotCommand::Prayed),

        // Match guards (`if ...`) pick the arm by what follows the command.
        // `/shop something-else` falls through to `None`.
        "/shop" if rest.is_empty() => Some(BotCommand::ListShop),
        "/shop" => match rest.to_lowercase().as_str() {
            "clear" => Some(BotCommand::ClearShop { bought_only: false }),
            "clear bought" => Some(BotCommand::ClearShop { bought_only: true }),
            _ => None,
        },
        "/todo" => match rest.to_lowercase().as_str() {
            "" => Some(BotCommand::ListTodos(TodoView::ByPerson)),
            "all" => Some(BotCommand::ListTodos(TodoView::All)),
            "mine" | "me" => Some(BotCommand::ListTodos(TodoView::Mine)),
            "clear" => Some(BotCommand::ClearTodos { done_only: false }),
            "clear done" => Some(BotCommand::ClearTodos { done_only: true }),
            // Anything else is a name. Display names are one word (see
            // `users::base_display_name`), so `/todo fix the tap` isn't one.
            _ => {
                let name = rest.strip_prefix('@').unwrap_or(rest);
                let one_word = !name.is_empty() && !name.contains(char::is_whitespace);
                one_word.then(|| BotCommand::ListTodos(TodoView::Person(name.to_string())))
            }
        },
        "/prayers" | "/prayer" => match rest.to_lowercase().as_str() {
            "" => Some(BotCommand::Prayers),
            "on" => Some(BotCommand::PrayerReminders(true)),
            "off" => Some(BotCommand::PrayerReminders(false)),
            _ => None,
        },
        "/settings" | "/setting" => parse_settings(rest).map(BotCommand::Settings),
        "/weather" => match rest.to_lowercase().as_str() {
            "" | "now" | "today" => Some(BotCommand::Weather(When::Now)),
            "tomorrow" | "tmr" | "tmrw" => Some(BotCommand::Weather(When::Tomorrow)),
            "week" | "7 days" => Some(BotCommand::Weather(When::Week)),
            "hourly" | "hours" | "hour" | "today hourly" => Some(BotCommand::Weather(When::Hourly)),
            _ => None,
        },
        "/moon" if rest.is_empty() => Some(BotCommand::Moon),
        // Any word after /help is a topic; `help::page` handles unknown ones.
        "/help" => Some(BotCommand::Help(
            (!rest.is_empty()).then(|| rest.to_string()),
        )),

        _ => None,
    }
}

/// Short forms of the common commands: `+s milk` is `+shop milk`, `/w tmr`
/// is `/weather tmr`, and so on. Anything else comes back unchanged.
/// `/settings` has no short form, so it can't be changed by accident.
///
/// It returns either `word` itself or a fixed string. No lifetime needs
/// writing: with exactly one reference in, Rust assumes the reference out
/// borrows from it ("lifetime elision"), and a `&'static str` fits that too.
fn expand_shorthand(word: &str) -> &str {
    match word {
        "+s" => "+shop",
        "+g" => "+got",
        "+d" => "+done",
        "+t" => "+todo",
        "+p" => "+prayed",
        "/s" => "/shop",
        "/t" => "/todo",
        "/p" => "/prayers",
        "/w" => "/weather",
        "/m" => "/moon",
        "/h" => "/help",
        other => other,
    }
}

/// The part after `/settings`: a key word, then (for some) a value.
fn parse_settings(rest: &str) -> Option<SettingsCommand> {
    let (key, value) = match rest.split_once(char::is_whitespace) {
        Some((key, value)) => (key, value.trim()),
        None => (rest, ""),
    };

    // A tuple match: both the key and whether there's a value decide.
    match (key.to_lowercase().as_str(), value) {
        ("", _) => Some(SettingsCommand::Show),
        ("everyone" | "all", "") => Some(SettingsCommand::Everyone),
        ("reset", "") => Some(SettingsCommand::Reset),
        ("methods", "") => Some(SettingsCommand::Methods),
        ("city" | "location", value) => parse_city(value),
        // IANA names have no spaces ("America/New_York").
        ("timezone" | "tz", value) if !value.is_empty() && !value.contains(char::is_whitespace) => {
            Some(SettingsCommand::Timezone(value.to_string()))
        }
        ("method", value) => value.parse().ok().map(SettingsCommand::PrayerMethod),
        ("school" | "asr", value) => match value.to_lowercase().as_str() {
            "hanafi" => Some(SettingsCommand::PrayerSchool(HANAFI)),
            "shafi" | "shafii" | "shafi'i" | "shafi’i" | "standard" => {
                Some(SettingsCommand::PrayerSchool(SHAFII))
            }
            _ => None,
        },
        ("ending", value) => value.parse().ok().map(SettingsCommand::PrayerEnding),
        _ => None,
    }
}

/// "Leicester, GB" → city "Leicester", country "GB". "Leicester" → no
/// country (the default is used). The *last* comma splits, so a city with
/// a comma in its name still works.
fn parse_city(value: &str) -> Option<SettingsCommand> {
    let (city, country) = match value.rsplit_once(',') {
        Some((city, country)) => (city.trim(), Some(country.trim())),
        None => (value.trim(), None),
    };
    if city.is_empty() || country == Some("") {
        return None;
    }
    Some(SettingsCommand::City {
        city: city.to_string(),
        country: country.map(str::to_string),
    })
}

/// Parses the text of a reply to one of the bot's lists.
/// `None` means it isn't a list action (just someone replying normally).
pub fn parse_reply(text: &str) -> Option<ReplyCommand> {
    // Commas count as spaces, so "1,3", "1, 3" and "1 3" are all the same.
    let normalised = text.replace(',', " ");
    let words: Vec<&str> = normalised.split_whitespace().collect();

    // Slice patterns: match on the *shape* of the list of words.
    // `[action, number]` only matches a list of exactly two words.
    match words.as_slice() {
        [action, number] if action.eq_ignore_ascii_case("done") => {
            parse_item_number(number).map(ReplyCommand::Done)
        }
        [action, number] if action.eq_ignore_ascii_case("remove") => {
            parse_item_number(number).map(ReplyCommand::Remove)
        }
        [] => None,
        numbers => {
            // Every word must be a number. Collecting an iterator of
            // `Option<usize>` into `Option<Vec<usize>>` gives `None` as soon
            // as one of them is `None`, so "1 apple" is rejected as a whole.
            let mut numbers: Vec<usize> = numbers
                .iter()
                .map(|n| parse_item_number(n))
                .collect::<Option<_>>()?;
            // "3 1 3" → [1, 3]. `dedup` only removes neighbours, hence the sort.
            numbers.sort_unstable();
            numbers.dedup();
            Some(ReplyCommand::Tick(numbers))
        }
    }
}

/// "1 3" → numbers; "milk, eggs" → names. Numbers only when *every* word
/// is a list number, so "2 pints milk" is still an item name.
fn parse_got(rest: &str) -> Option<Got> {
    // Reuses the reply parser: "1 3", "1,3" → Tick([1, 3]).
    if let Some(ReplyCommand::Tick(numbers)) = parse_reply(rest) {
        return Some(Got::Numbers(numbers));
    }
    parse_items(rest).map(Got::Names)
}

/// "milk, eggs\nbread" → ["milk", "eggs", "bread"]. `None` if no items.
///
/// Items are separated by commas or new lines. "and" is NOT a separator,
/// because "mac and cheese" is one item. A leading "and " is dropped, so
/// "milk, eggs, and bread" still works.
fn parse_items(rest: &str) -> Option<Vec<String>> {
    let items: Vec<String> = rest
        .split([',', '\n'])
        .map(|item| {
            let item = item.trim();
            strip_prefix_ignore_case(item, "and ")
                .unwrap_or(item)
                .trim()
                .to_string()
        })
        // ",," or a trailing comma would give empty items.
        .filter(|item| !item.is_empty())
        .collect();

    if items.is_empty() { None } else { Some(items) }
}

/// "@Khadija book dentist" → assignee `Mentioned("Khadija")`, title "book dentist".
/// "fix the tap" → no assignee. "fix the tap, call the school" → two todos
/// (split like `+shop`'s items; an `@name` first is for all of them).
/// `None` if there's no title or no name.
fn parse_add_todo(rest: &str) -> Option<BotCommand> {
    let (assignee, title) = match rest.strip_prefix('@') {
        Some(after_at) => {
            let (name, title) = after_at.split_once(char::is_whitespace)?;
            // "+todo @ book dentist": an `@` with no name is a mistake,
            // not a todo titled "book dentist".
            if name.is_empty() {
                return None;
            }
            (Some(Assignee::Mentioned(name.to_string())), title.trim())
        }
        None => (None, rest),
    };

    // `?`: no titles at all (empty, or only commas) is `None`.
    let titles = parse_items(title)?;
    Some(BotCommand::AddTodo { titles, assignee })
}

/// "asr" → [Asr], "fajr, dhuhr" → [Fajr, Dhuhr], "all" → all five.
/// `None` if nothing is named or any word isn't a prayer, so a typo gets
/// no silent half-result.
fn parse_prayed(rest: &str) -> Option<Vec<Prayer>> {
    let words: Vec<String> = rest
        .split([',', ' ', '\n'])
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();

    if words == ["all"] {
        return Some(Prayer::ALL.to_vec());
    }

    // Same trick as `parse_reply`: collecting `Option`s into
    // `Option<Vec<_>>` is `None` if any word didn't match.
    let mut prayers: Vec<Prayer> = words
        .iter()
        .map(|w| prayer_name(w))
        .collect::<Option<_>>()?;
    if prayers.is_empty() {
        return None;
    }
    // In day order, each once: "asr fajr asr" → [Fajr, Asr].
    prayers.sort_by_key(|p| Prayer::ALL.iter().position(|q| q == p));
    prayers.dedup();
    Some(prayers)
}

/// A prayer as people type it: its name (including common spellings) or
/// its number in the day, 1 (Fajr) to 5 (Isha).
fn prayer_name(word: &str) -> Option<Prayer> {
    match word {
        "fajr" | "fajar" | "subh" | "fajir" | "1" => Some(Prayer::Fajr),
        "dhuhr" | "zuhr" | "zohr" | "zuhur" | "dhuhur" | "duhr" | "zhuhr" | "2" => {
            Some(Prayer::Dhuhr)
        }
        "asr" | "asar" | "3" => Some(Prayer::Asr),
        "maghrib" | "magrib" | "maghreb" | "4" => Some(Prayer::Maghrib),
        "isha" | "esha" | "ishaa" | "isya" | "5" => Some(Prayer::Isha),
        _ => None,
    }
}

/// A list number as typed: "1" or more. 0, negatives and words are `None`.
fn parse_item_number(text: &str) -> Option<usize> {
    // `.ok()` turns the parse `Result` into an `Option`; `.filter` then
    // turns `Some(0)` into `None`, because lists are numbered from 1.
    text.parse::<usize>().ok().filter(|&n| n >= 1)
}

/// Like `str::strip_prefix`, but "And milk" matches "and ".
fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    // `get` instead of `[..]`: slicing in the middle of a multi-byte
    // character (an emoji, say) would panic, `get` returns `None`.
    let head = text.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        text.get(prefix.len()..)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Builds the Vec<String> that AddShop holds from string literals.
    fn shop(items: &[&str]) -> Option<BotCommand> {
        Some(BotCommand::AddShop(
            items.iter().map(|i| i.to_string()).collect(),
        ))
    }

    // `mentioned` is the name after `@`, if any.
    fn todo(title: &str, mentioned: Option<&str>) -> Option<BotCommand> {
        todos(&[title], mentioned)
    }

    fn todos(titles: &[&str], mentioned: Option<&str>) -> Option<BotCommand> {
        Some(BotCommand::AddTodo {
            titles: titles.iter().map(|t| t.to_string()).collect(),
            assignee: mentioned.map(|name| Assignee::Mentioned(name.to_string())),
        })
    }

    #[test]
    fn several_todos_at_once() {
        assert_eq!(
            parse("+todo fix the tap, call the school, and bins"),
            todos(&["fix the tap", "call the school", "bins"], None)
        );
        // One @name is for all of them.
        assert_eq!(
            parse("+t @Khadija book dentist, renew passport"),
            todos(&["book dentist", "renew passport"], Some("Khadija"))
        );
        // A new line works like a comma; empty ones are dropped.
        assert_eq!(
            parse("+todo hoover\nmop the floor,,\n"),
            todos(&["hoover", "mop the floor"], None)
        );
    }

    // ---- +shop ----

    #[test]
    fn shop_single_item() {
        assert_eq!(parse("+shop milk"), shop(&["milk"]));
    }

    #[test]
    fn shop_comma_separated_items_are_trimmed() {
        assert_eq!(
            parse("+shop milk, eggs ,bread"),
            shop(&["milk", "eggs", "bread"])
        );
    }

    #[test]
    fn shop_items_on_separate_lines() {
        assert_eq!(parse("+shop\nmilk\neggs"), shop(&["milk", "eggs"]));
    }

    #[test]
    fn shop_and_is_not_a_separator() {
        assert_eq!(parse("+shop mac and cheese"), shop(&["mac and cheese"]));
        assert_eq!(
            parse("+shop milk, eggs, and bread"),
            shop(&["milk", "eggs", "bread"])
        );
    }

    #[test]
    fn shop_ignores_empty_items() {
        assert_eq!(parse("+shop milk,, eggs,"), shop(&["milk", "eggs"]));
    }

    #[test]
    fn shop_keeps_item_capitals() {
        assert_eq!(parse("+shop Weetabix"), shop(&["Weetabix"]));
    }

    #[test]
    fn shop_command_is_case_insensitive_and_buy_is_an_alias() {
        assert_eq!(parse("+Shop milk"), shop(&["milk"]));
        assert_eq!(parse("+SHOP milk"), shop(&["milk"]));
        assert_eq!(parse("+buy milk"), shop(&["milk"]));
    }

    #[test]
    fn shop_with_extra_spaces() {
        assert_eq!(parse("   +shop    milk   "), shop(&["milk"]));
    }

    #[test]
    fn shop_with_nothing_to_add_is_none() {
        assert_eq!(parse("+shop"), None);
        assert_eq!(parse("+shop   "), None);
        assert_eq!(parse("+shop , ,"), None);
    }

    // ---- +got ----

    #[test]
    fn got_by_name() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            parse("+got milk, Eggs"),
            Some(BotCommand::GotShop(Got::Names(names(&["milk", "Eggs"]))))
        );
        assert_eq!(
            parse("+Bought 2 pints milk"),
            Some(BotCommand::GotShop(Got::Names(names(&["2 pints milk"]))))
        );
    }

    #[test]
    fn got_by_number() {
        assert_eq!(
            parse("+got 1 3"),
            Some(BotCommand::GotShop(Got::Numbers(vec![1, 3])))
        );
        assert_eq!(
            parse("+got 3,1"),
            Some(BotCommand::GotShop(Got::Numbers(vec![1, 3])))
        );
        assert_eq!(
            parse("+bought 2"),
            Some(BotCommand::GotShop(Got::Numbers(vec![2])))
        );
    }

    #[test]
    fn got_with_nothing_is_none() {
        assert_eq!(parse("+got"), None);
        assert_eq!(parse("+got ,"), None);
    }

    // ---- +todo ----

    #[test]
    fn todo_without_assignee() {
        assert_eq!(parse("+todo fix the tap"), todo("fix the tap", None));
    }

    #[test]
    fn todo_with_assignee() {
        assert_eq!(
            parse("+todo @Khadija book dentist"),
            todo("book dentist", Some("Khadija"))
        );
    }

    #[test]
    fn todo_at_sign_later_in_the_title_is_just_text() {
        assert_eq!(
            parse("+todo email bob@example.com"),
            todo("email bob@example.com", None)
        );
    }

    #[test]
    fn todo_without_a_title_is_none() {
        assert_eq!(parse("+todo"), None);
        assert_eq!(parse("+todo , ,"), None, "only commas");
        assert_eq!(parse("+todo @Khadija"), None);
        assert_eq!(parse("+todo @Khadija   "), None);
    }

    #[test]
    fn todo_with_empty_assignee_is_none() {
        assert_eq!(parse("+todo @"), None);
        assert_eq!(parse("+todo @ book dentist"), None);
    }

    // ---- / commands ----

    #[test]
    fn list_commands() {
        assert_eq!(parse("/shop"), Some(BotCommand::ListShop));
        assert_eq!(
            parse("/todo"),
            Some(BotCommand::ListTodos(TodoView::ByPerson))
        );
        assert_eq!(parse("/help"), Some(BotCommand::Help(None)));
        assert_eq!(
            parse("/help todo"),
            Some(BotCommand::Help(Some("todo".to_string())))
        );
        assert_eq!(parse("/Shop"), Some(BotCommand::ListShop));
    }

    #[test]
    fn shop_clear() {
        let all = Some(BotCommand::ClearShop { bought_only: false });
        let bought = Some(BotCommand::ClearShop { bought_only: true });
        assert_eq!(parse("/shop clear"), all);
        assert_eq!(parse("/shop CLEAR"), all);
        assert_eq!(parse("/shop clear bought"), bought);
        assert_eq!(parse("/shop clear everything"), None);
    }

    #[test]
    fn prayer_commands() {
        assert_eq!(parse("/prayers"), Some(BotCommand::Prayers));
        assert_eq!(parse("/Prayer"), Some(BotCommand::Prayers));
        assert_eq!(
            parse("/prayers on"),
            Some(BotCommand::PrayerReminders(true))
        );
        assert_eq!(
            parse("/prayers OFF"),
            Some(BotCommand::PrayerReminders(false))
        );
        assert_eq!(
            parse("  /prayer   on "),
            Some(BotCommand::PrayerReminders(true))
        );
    }

    #[test]
    fn prayed_names_one_or_more_prayers() {
        assert_eq!(
            parse("+prayed asr"),
            Some(BotCommand::Prayed(vec![Prayer::Asr]))
        );
        assert_eq!(
            parse("+Prayed Isha, fajr"),
            Some(BotCommand::Prayed(vec![Prayer::Fajr, Prayer::Isha]))
        );
        assert_eq!(
            parse("+prayed zuhr asr zohr"),
            Some(BotCommand::Prayed(vec![Prayer::Dhuhr, Prayer::Asr]))
        );
        assert_eq!(
            parse("+prayed all"),
            Some(BotCommand::Prayed(Prayer::ALL.to_vec()))
        );
    }

    #[test]
    fn prayed_with_nothing_or_a_typo_is_none() {
        assert_eq!(parse("+prayed"), None);
        assert_eq!(parse("+prayed ,"), None);
        assert_eq!(parse("+prayed asr and fajr"), None);
        assert_eq!(parse("+prayed sunrise"), None);
        assert_eq!(parse("+prayed all fajr"), None);
    }

    fn settings(text: &str) -> Option<SettingsCommand> {
        match parse(text) {
            Some(BotCommand::Settings(command)) => Some(command),
            Some(other) => panic!("{text:?} parsed as {other:?}"),
            None => None,
        }
    }

    #[test]
    fn settings_show_everyone_reset_methods() {
        assert_eq!(settings("/settings"), Some(SettingsCommand::Show));
        assert_eq!(
            settings("/Settings everyone"),
            Some(SettingsCommand::Everyone)
        );
        assert_eq!(settings("/settings all"), Some(SettingsCommand::Everyone));
        assert_eq!(settings("/settings reset"), Some(SettingsCommand::Reset));
        assert_eq!(
            settings("/settings methods"),
            Some(SettingsCommand::Methods)
        );
    }

    #[test]
    fn settings_city_with_and_without_country() {
        assert_eq!(
            settings("/settings city Milton Keynes, GB"),
            Some(SettingsCommand::City {
                city: "Milton Keynes".to_string(),
                country: Some("GB".to_string()),
            })
        );
        assert_eq!(
            settings("/settings location Leicester"),
            Some(SettingsCommand::City {
                city: "Leicester".to_string(),
                country: None,
            })
        );
        assert_eq!(settings("/settings city"), None);
        assert_eq!(settings("/settings city , GB"), None);
        assert_eq!(settings("/settings city Leicester,"), None);
    }

    #[test]
    fn settings_timezone_method_school_ending() {
        assert_eq!(
            settings("/settings timezone Europe/Paris"),
            Some(SettingsCommand::Timezone("Europe/Paris".to_string()))
        );
        assert_eq!(settings("/settings timezone Europe Paris"), None);
        assert_eq!(
            settings("/settings method 2"),
            Some(SettingsCommand::PrayerMethod(2))
        );
        assert_eq!(settings("/settings method two"), None);
        assert_eq!(
            settings("/settings school Hanafi"),
            Some(SettingsCommand::PrayerSchool(HANAFI))
        );
        assert_eq!(
            settings("/settings school shafi"),
            Some(SettingsCommand::PrayerSchool(SHAFII))
        );
        assert_eq!(
            settings("/settings school shafi'i"),
            Some(SettingsCommand::PrayerSchool(SHAFII))
        );
        assert_eq!(settings("/settings school maliki"), None);
        assert_eq!(
            settings("/settings ending 15"),
            Some(SettingsCommand::PrayerEnding(15))
        );
        assert_eq!(settings("/settings ending soon"), None);
        assert_eq!(settings("/settings colour blue"), None);
    }

    #[test]
    fn todo_views() {
        let view = |text| match parse(text) {
            Some(BotCommand::ListTodos(view)) => Some(view),
            _ => None,
        };
        assert_eq!(view("/todo"), Some(TodoView::ByPerson));
        assert_eq!(view("/todo all"), Some(TodoView::All));
        assert_eq!(view("/todo ALL"), Some(TodoView::All));
        assert_eq!(view("/todo mine"), Some(TodoView::Mine));
        assert_eq!(view("/t me"), Some(TodoView::Mine));
        let bilal = Some(TodoView::Person("Bilal".to_string()));
        assert_eq!(view("/todo Bilal"), bilal);
        assert_eq!(view("/todo @Bilal"), bilal);
        assert_eq!(view("/t @Bilal"), bilal);
        assert_eq!(view("/todo @"), None, "an @ without a name");
        assert_eq!(view("/todo fix the tap"), None, "not a name");
        // The keywords still win over a name.
        assert_eq!(
            parse("/todo clear"),
            Some(BotCommand::ClearTodos { done_only: false })
        );
    }

    #[test]
    fn moon_command() {
        assert_eq!(parse("/moon"), Some(BotCommand::Moon));
        assert_eq!(parse("/Moon"), Some(BotCommand::Moon));
        assert_eq!(parse("/moon tomorrow"), None);
    }

    #[test]
    fn weather_commands() {
        assert_eq!(parse("/weather"), Some(BotCommand::Weather(When::Now)));
        assert_eq!(
            parse("/Weather today"),
            Some(BotCommand::Weather(When::Now))
        );
        assert_eq!(
            parse("/weather tmr"),
            Some(BotCommand::Weather(When::Tomorrow))
        );
        assert_eq!(
            parse("/weather Tomorrow"),
            Some(BotCommand::Weather(When::Tomorrow))
        );
        assert_eq!(
            parse("/weather week"),
            Some(BotCommand::Weather(When::Week))
        );
        assert_eq!(
            parse("/weather hourly"),
            Some(BotCommand::Weather(When::Hourly))
        );
        assert_eq!(parse("/w hours"), Some(BotCommand::Weather(When::Hourly)));
        assert_eq!(parse("/weather in paris"), None);
    }

    #[test]
    fn slash_commands_with_unexpected_words_are_none() {
        assert_eq!(parse("/shop milk"), None);
        // One word after /todo is a name (see `todo_views`); more isn't.
        assert_eq!(parse("/todo right now"), None);
        assert_eq!(parse("/prayers maybe"), None);
        assert_eq!(parse("/prayers on off"), None);
    }

    // ---- ordinary chat ----

    #[test]
    fn shorthands_mean_the_same_as_the_full_commands() {
        let same = [
            ("+s milk, eggs", "+shop milk, eggs"),
            ("+S milk", "+shop milk"),
            ("+g milk", "+got milk"),
            ("+g 1 3", "+got 1 3"),
            ("+t fix the tap", "+todo fix the tap"),
            ("+t @Khadija book dentist", "+todo @Khadija book dentist"),
            ("+p asr", "+prayed asr"),
            ("/s", "/shop"),
            ("/s clear", "/shop clear"),
            ("/t", "/todo"),
            ("/p", "/prayers"),
            ("/p on", "/prayers on"),
            ("/w", "/weather"),
            ("/w tmr", "/weather tmr"),
            ("/m", "/moon"),
            ("/h", "/help"),
            ("/h shop", "/help shop"),
        ];
        for (short, full) in same {
            let parsed = parse(short);
            assert!(parsed.is_some(), "{short:?} should be a command");
            assert_eq!(parsed, parse(full), "{short:?} vs {full:?}");
        }
    }

    #[test]
    fn prayers_by_number_one_to_five() {
        assert_eq!(
            parse("+prayed 1"),
            Some(BotCommand::Prayed(vec![Prayer::Fajr]))
        );
        assert_eq!(
            parse("+p 3 1"),
            Some(BotCommand::Prayed(vec![Prayer::Fajr, Prayer::Asr]))
        );
        assert_eq!(
            parse("+p 1, 2, 3, 4, 5"),
            Some(BotCommand::Prayed(Prayer::ALL.to_vec()))
        );
        // Names and numbers can be mixed.
        assert_eq!(
            parse("+p isha 4"),
            Some(BotCommand::Prayed(vec![Prayer::Maghrib, Prayer::Isha]))
        );
        assert_eq!(parse("+p 0"), None);
        assert_eq!(parse("+p 6"), None);
    }

    #[test]
    fn done_and_clear_todos() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            parse("+done 2"),
            Some(BotCommand::DoneTodo(Got::Numbers(vec![2])))
        );
        assert_eq!(
            parse("+d 3 1"),
            Some(BotCommand::DoneTodo(Got::Numbers(vec![1, 3])))
        );
        assert_eq!(
            parse("+done go gym"),
            Some(BotCommand::DoneTodo(Got::Names(names(&["go gym"]))))
        );
        assert_eq!(parse("+done"), None);
        assert_eq!(
            parse("/todo clear"),
            Some(BotCommand::ClearTodos { done_only: false })
        );
        assert_eq!(
            parse("/t clear done"),
            Some(BotCommand::ClearTodos { done_only: true })
        );
        assert_eq!(parse("/todo clear everything"), None);
    }

    #[test]
    fn shorthands_still_need_their_arguments() {
        assert_eq!(parse("+s"), None);
        assert_eq!(parse("+t"), None);
        assert_eq!(parse("/s nonsense"), None);
        // No short form for settings.
        assert_eq!(parse("/set"), None);
    }

    #[test]
    fn ordinary_chat_is_none() {
        for text in [
            "",
            "   ",
            "see you at 6",
            "shop milk",
            "Todo add go gym",
            "the shop is closed",
            "Poll vote: yes",
            "+1",
            "+ shop milk",
            "/",
            "👍",
        ] {
            assert_eq!(parse(text), None, "{text:?} should not be a command");
        }
    }

    // ---- replies ----

    #[test]
    fn reply_numbers_tick_items() {
        assert_eq!(parse_reply("1 3"), Some(ReplyCommand::Tick(vec![1, 3])));
        assert_eq!(parse_reply("1,3"), Some(ReplyCommand::Tick(vec![1, 3])));
        assert_eq!(parse_reply(" 1, 3 "), Some(ReplyCommand::Tick(vec![1, 3])));
        assert_eq!(parse_reply("2"), Some(ReplyCommand::Tick(vec![2])));
    }

    #[test]
    fn reply_numbers_are_sorted_and_deduplicated() {
        assert_eq!(parse_reply("3 1 3"), Some(ReplyCommand::Tick(vec![1, 3])));
    }

    #[test]
    fn reply_done_and_remove() {
        assert_eq!(parse_reply("done 2"), Some(ReplyCommand::Done(2)));
        assert_eq!(parse_reply("Done 2"), Some(ReplyCommand::Done(2)));
        assert_eq!(parse_reply("remove 2"), Some(ReplyCommand::Remove(2)));
    }

    #[test]
    fn reply_that_is_not_a_list_action_is_none() {
        for text in [
            "", "thanks", "1 apple", "0", "-1", "done", "done 0", "done two", "done 2 3", "remove",
        ] {
            assert_eq!(
                parse_reply(text),
                None,
                "{text:?} should not be a list action"
            );
        }
    }
}

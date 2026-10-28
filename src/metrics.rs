//! What goes in the usage log (`command_log`, see migration 0004): a name
//! for each command, never its contents.
//!
//! Pure functions, like the parser: a command in, a name and detail out.
//! The webhook records them with `repo::metrics::record`.

use crate::help;
use crate::parser::{BotCommand, Got, ReplyCommand, SettingsCommand, TodoView};
use crate::person::Assignee;
use crate::weather::When;

/// The `kind` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Command,
    ListReply,
    Reaction,
    Unknown,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Command => "command",
            Kind::ListReply => "list_reply",
            Kind::Reaction => "reaction",
            Kind::Unknown => "unknown",
        }
    }
}

/// One thing someone did, as it's logged.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub kind: Kind,
    /// "todo.add". A `String` because an unknown command's name is the word
    /// typed; known ones are fixed names.
    pub name: String,
    /// A fixed word about it, e.g. "week" for `/weather week`.
    pub detail: Option<String>,
    /// The first word as typed, if it's a `+` or `/` word ("/t", "+d").
    pub typed: Option<String>,
}

impl Event {
    fn new(kind: Kind, name: &str, detail: Option<&str>) -> Self {
        Event {
            kind,
            name: name.to_string(),
            detail: detail.map(str::to_string),
            typed: None,
        }
    }

    /// The same event, with the first word of `text` as `typed`.
    pub fn typed_as(self, text: &str) -> Self {
        Event {
            typed: command_word(text),
            ..self
        }
    }
}

/// The most characters kept of a typed word (the schema allows 40).
const MAX_WORD: usize = 30;

/// The first word of `text`, lower case, if it starts with `+` or `/`
/// (so it's meant as a command), cut to 30 characters. `None` for normal
/// chat, which is never logged.
pub fn command_word(text: &str) -> Option<String> {
    let word = text.split_whitespace().next()?;
    if !word.starts_with(['+', '/']) || word.len() < 2 {
        return None;
    }
    // `.chars().take(n)` rather than slicing bytes (`&word[..30]`), which
    // would panic in the middle of an emoji or accented letter.
    Some(word.to_lowercase().chars().take(MAX_WORD).collect())
}

/// A message starting with `+` or `/` that the parser didn't understand.
pub fn unknown(text: &str) -> Option<Event> {
    let word = command_word(text)?;
    Some(Event {
        kind: Kind::Unknown,
        name: word.clone(),
        detail: None,
        typed: Some(word),
    })
}

/// Which list a `Got` used: numbers from a list, or names.
fn got(got: &Got) -> &'static str {
    match got {
        Got::Numbers(_) => "numbers",
        Got::Names(_) => "names",
    }
}

/// The name and detail for a typed command.
pub fn command(command: &BotCommand) -> Event {
    let event = |name, detail| Event::new(Kind::Command, name, detail);
    match command {
        BotCommand::AddShop(_) => event("shop.add", None),
        BotCommand::ListShop => event("shop.list", None),
        BotCommand::ClearShop { bought_only } => event(
            "shop.clear",
            Some(if *bought_only { "bought" } else { "all" }),
        ),
        BotCommand::GotShop(g) => event("shop.got", Some(got(g))),
        BotCommand::AddTodo { assignee, .. } => {
            let whose = match assignee {
                Some(Assignee::Mentioned(_)) => "someone",
                None | Some(Assignee::Sender(_)) => "self",
            };
            event("todo.add", Some(whose))
        }
        BotCommand::ListTodos(view) => event(
            "todo.list",
            Some(match view {
                TodoView::ByPerson => "by_person",
                TodoView::All => "all",
                TodoView::Mine => "mine",
                TodoView::Person(_) => "person",
            }),
        ),
        BotCommand::DoneTodo(g) => event("todo.done", Some(got(g))),
        BotCommand::ClearTodos { done_only } => {
            event("todo.clear", Some(if *done_only { "done" } else { "all" }))
        }
        BotCommand::Prayers => event("prayers.list", None),
        BotCommand::PrayerReminders(on) => {
            event("prayers.reminders", Some(if *on { "on" } else { "off" }))
        }
        BotCommand::Prayed(prayers) => {
            // "asr" or "fajr,dhuhr": which prayers get ticked off by hand
            // rather than by reacting. At most 27 characters.
            let names: Vec<String> = prayers
                .iter()
                .map(|p| p.to_string().to_lowercase())
                .collect();
            Event {
                detail: Some(names.join(",")),
                ..event("prayers.prayed", None)
            }
        }
        BotCommand::Settings(command) => event(
            "settings",
            Some(match command {
                SettingsCommand::Show => "show",
                SettingsCommand::Everyone => "everyone",
                SettingsCommand::Reset => "reset",
                SettingsCommand::Methods => "methods",
                SettingsCommand::City { .. } => "city",
                SettingsCommand::Timezone(_) => "timezone",
                SettingsCommand::PrayerMethod(_) => "method",
                SettingsCommand::PrayerSchool(_) => "school",
                SettingsCommand::PrayerEnding(_) => "ending",
            }),
        ),
        BotCommand::Weather(when) => event(
            "weather",
            Some(match when {
                When::Now => "now",
                When::Tomorrow => "tomorrow",
                When::Week => "week",
                When::Hourly => "hourly",
            }),
        ),
        BotCommand::Moon => event("moon", None),
        BotCommand::SharedLocation { .. } => event("settings.location", None),
        // The topic as `/help` understood it, so a typo isn't stored.
        BotCommand::Help(topic) => event(
            "help",
            Some(match topic {
                None => "overview",
                Some(word) => help::topic(word).unwrap_or("not_found"),
            }),
        ),
    }
}

/// The name for a reply to a list: `item_type` is the list's ("todo" or
/// "shop"), so `done 2` on a todo list is "todo.reply_done".
pub fn list_reply(item_type: &str, reply: &ReplyCommand) -> Event {
    let action = match reply {
        ReplyCommand::Tick(_) => "tick",
        ReplyCommand::Done(_) => "done",
        ReplyCommand::Remove(_) => "remove",
    };
    let list = if item_type == "shop" { "shop" } else { "todo" };
    Event::new(Kind::ListReply, &format!("{list}.reply_{action}"), None)
}

/// A reaction on a prayer reminder: added (prayed) or removed (undone).
pub fn reaction(removed: bool) -> Event {
    let detail = if removed { "removed" } else { "added" };
    Event::new(Kind::Reaction, "prayers.react", Some(detail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    /// `(name, detail)` for a typed message, via the real parser.
    fn logged(text: &str) -> (String, Option<String>) {
        let event = command(&parse(text).expect(text));
        assert_eq!(event.kind, Kind::Command);
        (event.name, event.detail)
    }

    fn pair(name: &str, detail: Option<&str>) -> (String, Option<String>) {
        (name.to_string(), detail.map(str::to_string))
    }

    #[test]
    fn commands_are_logged_by_name_without_their_contents() {
        let cases = [
            ("+shop milk, eggs", pair("shop.add", None)),
            ("+got 1 3", pair("shop.got", Some("numbers"))),
            ("+g milk", pair("shop.got", Some("names"))),
            ("/shop clear bought", pair("shop.clear", Some("bought"))),
            (
                "+todo @Khadija book dentist",
                pair("todo.add", Some("someone")),
            ),
            ("+todo fix the tap", pair("todo.add", Some("self"))),
            ("/t", pair("todo.list", Some("by_person"))),
            ("/todo Bilal", pair("todo.list", Some("person"))),
            ("+d gym", pair("todo.done", Some("names"))),
            (
                "+prayed fajr, asr",
                pair("prayers.prayed", Some("fajr,asr")),
            ),
            ("/prayers on", pair("prayers.reminders", Some("on"))),
            ("/settings city Leeds, GB", pair("settings", Some("city"))),
            ("/w week", pair("weather", Some("week"))),
            ("/m", pair("moon", None)),
            ("/help", pair("help", Some("overview"))),
            ("/help Namaz", pair("help", Some("prayers"))),
            ("/help cooking", pair("help", Some("not_found"))),
        ];
        for (text, expected) in cases {
            assert_eq!(logged(text), expected, "{text}");
        }
        // Nothing the person wrote ends up in the log.
        let (name, detail) = logged("+todo @Khadija book dentist");
        assert!(!format!("{name}{detail:?}").contains("dentist"));
    }

    #[test]
    fn every_prayer_fits_in_the_detail_column() {
        let event = command(&parse("+prayed all").unwrap());
        let detail = event.detail.unwrap();
        assert_eq!(detail, "fajr,dhuhr,asr,maghrib,isha");
        assert!(detail.len() <= 40);
    }

    #[test]
    fn the_typed_word_shows_short_forms() {
        let typed = |text| command(&parse(text).unwrap()).typed_as(text).typed;
        assert_eq!(typed("/t"), Some("/t".to_string()));
        assert_eq!(typed("+Todo fix the tap"), Some("+todo".to_string()));
    }

    #[test]
    fn only_plus_and_slash_words_are_kept() {
        assert_eq!(command_word("/remind me"), Some("/remind".to_string()));
        assert_eq!(command_word("  +Remind 5pm"), Some("+remind".to_string()));
        assert_eq!(command_word("hello there"), None, "normal chat");
        assert_eq!(command_word("+"), None, "a lone +");
        assert_eq!(command_word("/"), None);
        assert_eq!(command_word(""), None);
        // Long words are cut by characters, so emoji can't split.
        let long = format!("/{}", "é".repeat(50));
        assert_eq!(command_word(&long).unwrap().chars().count(), 30);
    }

    #[test]
    fn unknown_commands_are_logged_by_their_first_word() {
        let event = unknown("+remind call mum at 5").unwrap();
        assert_eq!(event.kind, Kind::Unknown);
        assert_eq!(event.name, "+remind");
        assert_eq!(unknown("just chatting"), None);
    }

    #[test]
    fn list_replies_and_reactions() {
        let tick = list_reply("shop", &ReplyCommand::Tick(vec![1, 3]));
        assert_eq!(
            (tick.kind, tick.name.as_str()),
            (Kind::ListReply, "shop.reply_tick")
        );
        let done = list_reply("todo", &ReplyCommand::Done(2));
        assert_eq!(done.name, "todo.reply_done");
        let undo = reaction(true);
        assert_eq!(
            (undo.name.as_str(), undo.detail.as_deref()),
            ("prayers.react", Some("removed"))
        );
    }
}

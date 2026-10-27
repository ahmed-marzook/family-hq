//! Turns the text of a WhatsApp message into a command for the bot.
//!
//! A pure function: text in, `Option` out. No database, no network, no
//! clock, so every case can be tested in a millisecond.
//!
//! Commands start with `+` (add something) or `/` (show or change a list).
//! Anything else is ordinary chat and parses to `None`, so the bot stays
//! quiet in the family group unless someone is talking to it.

use crate::person::Assignee;

/// A command typed as a normal message.
#[derive(Debug, PartialEq)]
pub enum BotCommand {
    /// `+shop milk, eggs` or `+buy milk`
    AddShop(Vec<String>),
    /// `/shop`
    ListShop,
    /// `/shop clear`
    ClearShop,
    /// `+todo fix the tap` or `+todo @Khadija book dentist`
    AddTodo {
        title: String,
        /// The parser only ever sets `Assignee::Mentioned` (from `@name`),
        /// or `None` when no name was typed. It can't know who sent the
        /// message, so the webhook replaces `None` with `Assignee::Sender`.
        assignee: Option<Assignee>,
    },
    /// `/todo`
    ListTodos,
    /// `/help`
    Help,
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

    match first.to_lowercase().as_str() {
        "+shop" | "+buy" => parse_shop_items(rest).map(BotCommand::AddShop),
        "+todo" => parse_add_todo(rest),

        // Match guards (`if ...`) pick the arm by what follows the command.
        // `/shop something-else` falls through to `None`.
        "/shop" if rest.is_empty() => Some(BotCommand::ListShop),
        "/shop" if rest.eq_ignore_ascii_case("clear") => Some(BotCommand::ClearShop),
        "/todo" if rest.is_empty() => Some(BotCommand::ListTodos),
        "/help" if rest.is_empty() => Some(BotCommand::Help),

        _ => None,
    }
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

/// "milk, eggs\nbread" → ["milk", "eggs", "bread"]. `None` if no items.
///
/// Items are separated by commas or new lines. "and" is NOT a separator,
/// because "mac and cheese" is one item. A leading "and " is dropped, so
/// "milk, eggs, and bread" still works.
fn parse_shop_items(rest: &str) -> Option<Vec<String>> {
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
/// "fix the tap" → no assignee. `None` if there's no title or no name.
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

    if title.is_empty() {
        return None;
    }

    Some(BotCommand::AddTodo {
        title: title.to_string(),
        assignee,
    })
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
        Some(BotCommand::AddTodo {
            title: title.to_string(),
            assignee: mentioned.map(|name| Assignee::Mentioned(name.to_string())),
        })
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
        assert_eq!(parse("/todo"), Some(BotCommand::ListTodos));
        assert_eq!(parse("/help"), Some(BotCommand::Help));
        assert_eq!(parse("/Shop"), Some(BotCommand::ListShop));
    }

    #[test]
    fn shop_clear() {
        assert_eq!(parse("/shop clear"), Some(BotCommand::ClearShop));
        assert_eq!(parse("/shop CLEAR"), Some(BotCommand::ClearShop));
    }

    #[test]
    fn slash_commands_with_unexpected_words_are_none() {
        assert_eq!(parse("/shop milk"), None);
        assert_eq!(parse("/todo now"), None);
        assert_eq!(parse("/help me"), None);
    }

    // ---- ordinary chat ----

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

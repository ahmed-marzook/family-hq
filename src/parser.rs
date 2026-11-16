//! Turns the text of a WhatsApp message into a command for the bot.
//!
//! A pure function: text in, `Option` out. No database, no network, no
//! clock, so every case can be tested in a millisecond.
//!
//! Commands start with `+` (add something) or `/` (show or change a list).
//! Anything else is ordinary chat and parses to `None`, so the bot stays
//! quiet in the family group unless someone is talking to it.

use chrono::{DateTime, NaiveTime, Utc};

use crate::person::Assignee;
use crate::prayer::{HANAFI, Prayer, SHAFII};
use crate::reports::{Delivery, Span};
use crate::todo::flags::{Due, Flag, FlagError, Priority};
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
    /// once: `+todo fix the tap, call the school` (one `@name` is for all).
    /// Flags at the end are for all of them too: `-d fri -p high`.
    AddTodo {
        titles: Vec<String>,
        /// The parser only ever sets `Assignee::Mentioned` (from `@name`),
        /// or `None` when no name was typed. It can't know who sent the
        /// message, so the webhook replaces `None` with `Assignee::Sender`.
        assignee: Option<Assignee>,
        /// `-d fri`: as typed. The handler turns it into a date, because
        /// only it knows what "today" is for the sender.
        due: Option<Due>,
        /// `-p high`
        priority: Option<Priority>,
    },
    /// A `+todo` whose flags are wrong (`-p urgent`, `-d` with no date).
    /// Not `None`: the sender clearly meant the bot, so it says what's wrong.
    BadTodo(FlagError),
    /// `/todo`
    ListTodos(TodoView),
    /// `+done 2`, `+done 1 3` (numbers from the latest `/todo` list) or
    /// `+done gym` (by title): complete todos without replying
    DoneTodo(Got),
    /// `+skip 2` or `+skip gym`: close todos as "won't be done", like
    /// `+done` but kept on record as not done
    SkipTodo(Got),
    /// `/todo clear` (everything) or `/todo clear done` (only done and skipped)
    ClearTodos { done_only: bool },
    /// `/todo reminders on`, `off`, or just `/todo reminders` (`Show`, to
    /// see which it is): the sender's own, in this chat
    TodoReminders(Toggle),
    /// `/prayers`: today's times, and which you've prayed
    Prayers,
    /// `/prayers on` (true) / `/prayers off` (false): reminders in this chat
    PrayerReminders(bool),
    /// `/prayers week` / `/prayers month`: the sender's own summary so far,
    /// with its heatmap
    PrayerSummary(Span),
    /// `/prayers reports here|dm|off`, or just `/prayers reports` (`None`,
    /// to see which it is): where this chat's prayer reports go
    PrayerReports(Option<Delivery>),
    /// `+prayed asr` or `+prayed fajr, dhuhr` or `+prayed all`: mark
    /// prayers as prayed without reacting to a reminder. Can end with
    /// `on time` or `late`, and say `yesterday`.
    Prayed(Checkin),
    /// `/settings ...`: show or change this chat's settings
    Settings(SettingsCommand),
    /// `/weather`, `/weather hourly`, `/weather tomorrow`, `/weather week`
    Weather(When),
    /// `/moon`: today's phase, moonrise and moonset, the next new and full
    Moon,
    /// A location shared in WhatsApp (📎 → Location). Not typed: the
    /// webhook builds this from a location message in a DM.
    SharedLocation { latitude: f64, longitude: f64 },
    /// `/push`, `/push on`, `/push off`, `/push test`: the sender's own push
    /// notifications (ntfy)
    Push(PushCommand),
    /// `/points`: in a group, this week's leaderboard; in a DM, your own
    /// points from every chat
    Points,
    /// `/points reports here|dm|off`, or just `/points reports` (`None`, to
    /// see which it is): where a group's Sunday summary goes
    PointsReports(Option<Delivery>),
    /// `/points weeks`, `/points months`, `/points year` or `/points 2025`:
    /// your own history, in a DM only
    PointsHistory(PointsHistory),
    /// `/jot ...` or `+jot text`: the opt-in log of what you've done
    Jot(JotCommand),
    /// `/help` (overview) or `/help shop` (one topic's page)
    Help(Option<String>),
}

/// What `/jot` and `+jot` ask for.
#[derive(Debug, Clone, PartialEq)]
pub enum JotCommand {
    /// `/jot`: is it on, and when is the next prompt?
    Status,
    /// `/jot on`, `/jot on 2h`, `/jot on 2h 09:00-18:00`: what isn't said
    /// stays as it was (or is the default).
    On {
        interval_minutes: Option<u32>,
        hours: Option<(NaiveTime, NaiveTime)>,
        /// `-days mon-fri`
        days: Option<JotDays>,
    },
    /// `/jot off`
    Off,
    /// `/jot pause 3h`, `/jot pause today`: no prompts for a while, without
    /// turning Jot off
    Pause(JotPause),
    /// `/jot today`, `/jot yesterday`, `/jot mon`: that day as a timeline
    Day(JotDay),
    /// `/jot week`: one line per day of the last 7
    Week,
    /// `+jot text` or `+j text`: an entry with no prompt
    Log(String),
    /// Not typed: the webhook builds this from a reply to a prompt (or the
    /// next plain message after one), which answers the prompt at `slot_at`.
    Reply {
        text: String,
        slot_at: DateTime<Utc>,
    },
    /// Not typed either: a reply of `skip` (or 😴) to the prompt at
    /// `slot_at`.
    Skip { slot_at: DateTime<Utc> },
    /// `/jot on 5h`, `/jot on 18:00-09:00`: clearly meant, but wrong, so the
    /// bot says what's wrong.
    Invalid(JotError),
}

/// Which day `/jot` shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JotDay {
    Today,
    Yesterday,
    /// The latest such weekday within the last 7 days (what `/jot week`
    /// lists), so a reply of `mon` after the week view means that Monday.
    Weekday(chrono::Weekday),
}

/// How long `/jot pause` stops the prompts for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JotPause {
    /// `3h`, `90m`.
    For { minutes: u32 },
    /// Until the person's local midnight.
    Today,
}

/// The weekdays Jot prompts on: a bit each, Monday = 1 to Sunday = 64.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JotDays(u8);

impl JotDays {
    pub const EVERY_DAY: JotDays = JotDays(0b111_1111);

    /// `None` for an empty set or a bit that isn't a day (what the schema's
    /// `CHECK` refuses too).
    pub fn from_bits(bits: i64) -> Option<JotDays> {
        u8::try_from(bits)
            .ok()
            .filter(|b| (1..=0b111_1111).contains(b))
            .map(JotDays)
    }

    pub fn bits(self) -> u8 {
        self.0
    }

    pub fn contains(self, day: chrono::Weekday) -> bool {
        self.0 & (1 << day.num_days_from_monday()) != 0
    }

    fn of(days: impl IntoIterator<Item = chrono::Weekday>) -> JotDays {
        JotDays(
            days.into_iter()
                .fold(0, |bits, day| bits | (1 << day.num_days_from_monday())),
        )
    }

    /// `mon-fri` (which can wrap: `fri-mon`), `mon,wed,fri`, `weekdays`,
    /// `weekends`, `daily`. `None` if it isn't one of those.
    pub fn parse(text: &str) -> Option<JotDays> {
        use chrono::Weekday::{Fri, Mon, Sat, Sun};
        match text {
            "daily" | "everyday" | "all" => return Some(Self::EVERY_DAY),
            "weekdays" => return Some(Self::of(Mon.cycle_to(Fri))),
            "weekends" => return Some(Self::of([Sat, Sun])),
            _ => {}
        }
        let day = |word: &str| word.parse::<chrono::Weekday>().ok();
        if let Some((from, to)) = text.split_once(['-', '–']) {
            return Some(Self::of(day(from)?.cycle_to(day(to)?)));
        }
        let days = text.split(',').map(day).collect::<Option<Vec<_>>>()?;
        Some(Self::of(days))
    }

    /// `every day`, `Mon-Fri`, `Mon, Wed, Fri`: how it's said back.
    pub fn label(self) -> String {
        use chrono::Weekday::{Fri, Mon, Sat, Sun};
        if self == Self::EVERY_DAY {
            return "every day".to_string();
        }
        if self == Self::of(Mon.cycle_to(Fri)) {
            return "Mon–Fri".to_string();
        }
        if self == Self::of([Sat, Sun]) {
            return "Sat–Sun".to_string();
        }
        (0..7)
            .map(|n| chrono::Weekday::try_from(n).unwrap_or(Mon))
            .filter(|d| self.contains(*d))
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// `Weekday::cycle_to`: the days from one to another, wrapping past Sunday.
trait CycleTo {
    fn cycle_to(self, to: chrono::Weekday) -> Vec<chrono::Weekday>;
}

impl CycleTo for chrono::Weekday {
    fn cycle_to(self, to: chrono::Weekday) -> Vec<chrono::Weekday> {
        let mut days = vec![self];
        let mut day = self;
        while day != to {
            day = day.succ();
            days.push(day);
        }
        days
    }
}

/// What's wrong with a `/jot on` the sender typed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JotError {
    #[error(
        "I can't use `{0}` as an interval. Try `1h`, `90m`, `2h` or `3h`: anything from 30m to 4h."
    )]
    BadInterval(String),
    #[error("I can't read `{0}` as hours. Write them like `09:00-18:00`.")]
    BadHours(String),
    #[error("The hours must end after they start: `09:00-18:00`, not `18:00-09:00`.")]
    EndBeforeStart,
    #[error(
        "I can't read `{0}` as days. Try `-days mon-fri`, `-days mon,wed,fri`, `-days weekdays` or `-days weekends`."
    )]
    BadDays(String),
    #[error(
        "I can't pause for `{0}`. Try `/jot pause 3h`, `/jot pause 90m` or `/jot pause today`."
    )]
    BadPause(String),
}

/// Which part of your own points history to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointsHistory {
    /// The last 12 weeks.
    Weeks,
    /// The last 12 months.
    Months,
    /// A calendar year: this one (`None`) or the one named.
    Year(Option<i32>),
}

/// What a setting that is on or off is asked for: to show it, or to turn
/// it on or off. Three named cases read better than `Option<bool>`, where
/// it's easy to forget that `None` means "just show me".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    /// No `on` or `off`: say which it is.
    Show,
    On,
    Off,
}

/// What `/push` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushCommand {
    /// `/push`: is it on?
    Status,
    /// `/push on`: turn it on, and say how to subscribe.
    On,
    /// `/push off`
    Off,
    /// `/push test`: send a test push.
    Test,
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
    /// `/todo overdue`, `/todo due`, `/todo high`: only the open todos that
    /// match, everyone's.
    Filtered(TodoFilter),
}

/// Which open todos a filtered `/todo` shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoFilter {
    /// `/todo overdue` (or `late`): due before today.
    Overdue,
    /// `/todo due`: any with a due date.
    Due,
    /// `/todo high`: high priority.
    High,
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

/// A `+prayed` message: which prayers, which day, and whether on time.
#[derive(Debug, Clone, PartialEq)]
pub struct Checkin {
    /// In day order, each once.
    pub prayers: Vec<Prayer>,
    pub timing: Timing,
    /// `+prayed yesterday isha`: for yesterday rather than today.
    pub yesterday: bool,
}

impl Checkin {
    /// Just these prayers, today, timing worked out as usual.
    // Only the tests use this.
    #[cfg(test)]
    pub fn today(prayers: Vec<Prayer>) -> Self {
        Checkin {
            prayers,
            timing: Timing::Auto,
            yesterday: false,
        }
    }
}

/// Whether a check-in was on time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timing {
    /// Nothing said: on time, unless the prayer was already marked ❌
    /// (then it's made up). The same as reacting to a reminder.
    Auto,
    /// `on time`: prayed in its time, just ticked off late. Replaces a ❌.
    OnTime,
    /// `late` (or `qada`): prayed after its time.
    Late,
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
    /// `skip 2`: a todo that won't be done
    Skip(usize),
    /// `due 2 fri` (`Some`), or `due 2 none` (`None`) to take the date off
    SetDue(usize, Option<Due>),
    /// `priority 2 high` (`Some`), or `priority 2 none` (`None`)
    SetPriority(usize, Option<Priority>),
    /// `due 2 friyay`, `priority 2`: clearly a change, but the value is
    /// wrong or missing, so the bot says what's wrong
    BadChange(FlagError),
}

/// Parses a normal message. `None` means "not for the bot".
pub fn parse(text: &str) -> Option<BotCommand> {
    // "+shop milk, eggs" → ("+shop", "milk, eggs"). A single word has no rest.
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
        "+skip" => parse_got(rest).map(BotCommand::SkipTodo),
        "+todo" => parse_add_todo(rest),
        "+prayed" => parse_prayed(rest).map(BotCommand::Prayed),
        // Everything after `+jot` is the entry, as typed.
        "+jot" => (!rest.is_empty()).then(|| BotCommand::Jot(JotCommand::Log(rest.to_string()))),
        "/jot" => parse_jot(rest).map(BotCommand::Jot),

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
            // Before the name arm below, so these words win over a name.
            "overdue" | "late" => Some(BotCommand::ListTodos(TodoView::Filtered(
                TodoFilter::Overdue,
            ))),
            "due" => Some(BotCommand::ListTodos(TodoView::Filtered(TodoFilter::Due))),
            "high" => Some(BotCommand::ListTodos(TodoView::Filtered(TodoFilter::High))),
            "clear" => Some(BotCommand::ClearTodos { done_only: false }),
            "clear done" => Some(BotCommand::ClearTodos { done_only: true }),
            "reminders" | "reminder" => Some(BotCommand::TodoReminders(Toggle::Show)),
            "reminders on" | "reminder on" => Some(BotCommand::TodoReminders(Toggle::On)),
            "reminders off" | "reminder off" => Some(BotCommand::TodoReminders(Toggle::Off)),
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
            "week" => Some(BotCommand::PrayerSummary(Span::Week)),
            "month" => Some(BotCommand::PrayerSummary(Span::Month)),
            "reports" | "report" => Some(BotCommand::PrayerReports(None)),
            "reports here" | "reports group" | "reports on" | "report here" | "report on" => {
                Some(BotCommand::PrayerReports(Some(Delivery::Here)))
            }
            "reports dm" | "reports private" | "report dm" => {
                Some(BotCommand::PrayerReports(Some(Delivery::Dm)))
            }
            "reports off" | "report off" => Some(BotCommand::PrayerReports(Some(Delivery::Off))),
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
        "/points" | "/point" | "/pts" => match rest.to_lowercase().as_str() {
            "" => Some(BotCommand::Points),
            "reports" | "report" => Some(BotCommand::PointsReports(None)),
            "reports here" | "reports group" | "reports on" | "report here" | "report on" => {
                Some(BotCommand::PointsReports(Some(Delivery::Here)))
            }
            "reports dm" | "reports private" | "report dm" => {
                Some(BotCommand::PointsReports(Some(Delivery::Dm)))
            }
            "reports off" | "report off" => Some(BotCommand::PointsReports(Some(Delivery::Off))),
            "weeks" | "week" => Some(BotCommand::PointsHistory(PointsHistory::Weeks)),
            "months" | "month" => Some(BotCommand::PointsHistory(PointsHistory::Months)),
            "year" | "this year" => Some(BotCommand::PointsHistory(PointsHistory::Year(None))),
            other => {
                let year = other.strip_prefix("year ").unwrap_or(other);
                history_year(year).map(|y| BotCommand::PointsHistory(PointsHistory::Year(Some(y))))
            }
        },
        "/push" => match rest.to_lowercase().as_str() {
            "" | "status" => Some(BotCommand::Push(PushCommand::Status)),
            "on" => Some(BotCommand::Push(PushCommand::On)),
            "off" => Some(BotCommand::Push(PushCommand::Off)),
            "test" => Some(BotCommand::Push(PushCommand::Test)),
            _ => None,
        },
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
        "+j" => "+jot",
        "/j" => "/jot",
        other => other,
    }
}

/// The part after `/jot`.
fn parse_jot(rest: &str) -> Option<JotCommand> {
    let lower = rest.to_lowercase();
    let mut words = lower.split_whitespace();
    match words.next() {
        None | Some("status") => Some(JotCommand::Status),
        Some("off") if words.next().is_none() => Some(JotCommand::Off),
        Some("on") => Some(parse_jot_on(words)),
        Some("pause") => Some(parse_jot_pause(words)),
        Some(word) if words.next().is_none() => match word {
            "today" => Some(JotCommand::Day(JotDay::Today)),
            "yesterday" => Some(JotCommand::Day(JotDay::Yesterday)),
            "week" => Some(JotCommand::Week),
            other => other
                .parse::<chrono::Weekday>()
                .ok()
                .map(|day| JotCommand::Day(JotDay::Weekday(day))),
        },
        _ => None,
    }
}

/// `2h`, `90m` and `09:00-18:00`, in either order, each optional.
fn parse_jot_on<'a>(words: impl Iterator<Item = &'a str>) -> JotCommand {
    let (mut interval_minutes, mut hours, mut days) = (None, None, None);
    let mut words = words;
    while let Some(word) = words.next() {
        if matches!(word, "-days" | "--days") {
            let spec = words.next().unwrap_or_default();
            match JotDays::parse(spec) {
                Some(found) => days = Some(found),
                None => return JotCommand::Invalid(JotError::BadDays(spec.to_string())),
            }
        } else if word.contains([':', '-', '–']) {
            match parse_jot_hours(word) {
                Ok(found) => hours = Some(found),
                Err(err) => return JotCommand::Invalid(err),
            }
        } else {
            match parse_jot_interval(word) {
                Some(found) => interval_minutes = Some(found),
                None => return JotCommand::Invalid(JotError::BadInterval(word.to_string())),
            }
        }
    }
    JotCommand::On {
        interval_minutes,
        hours,
        days,
    }
}

/// `3h`, `90m` or `today`. At most 3 days, so a typo like `300h` is
/// refused rather than silently switching Jot off for weeks.
fn parse_jot_pause<'a>(mut words: impl Iterator<Item = &'a str>) -> JotCommand {
    let word = words.next().unwrap_or_default();
    if words.next().is_some() {
        return JotCommand::Invalid(JotError::BadPause(word.to_string()));
    }
    if word == "today" {
        return JotCommand::Pause(JotPause::Today);
    }
    let (number, per_unit) = match (word.strip_suffix('h'), word.strip_suffix('m')) {
        (Some(hours), _) => (hours, 60),
        (_, Some(minutes)) => (minutes, 1),
        _ => (word, 0),
    };
    let minutes = number
        .parse::<u32>()
        .ok()
        .filter(|_| per_unit > 0 && number.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|n| n.checked_mul(per_unit))
        .filter(|m| (1..=3 * 24 * 60).contains(m));
    match minutes {
        Some(minutes) => JotCommand::Pause(JotPause::For { minutes }),
        None => JotCommand::Invalid(JotError::BadPause(word.to_string())),
    }
}

/// `2h` or `90m`, from 30 minutes to 4 hours (the schema's `CHECK` too).
fn parse_jot_interval(word: &str) -> Option<u32> {
    let (number, minutes_per_unit) = match (word.strip_suffix('h'), word.strip_suffix('m')) {
        (Some(hours), _) => (hours, 60),
        (_, Some(minutes)) => (minutes, 1),
        _ => return None,
    };
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let minutes = number.parse::<u32>().ok()?.checked_mul(minutes_per_unit)?;
    (30..=240).contains(&minutes).then_some(minutes)
}

/// `09:00-18:00` (or `9:00-18:00`): a start, then a later end.
fn parse_jot_hours(word: &str) -> Result<(NaiveTime, NaiveTime), JotError> {
    let bad = || JotError::BadHours(word.to_string());
    let (start, end) = word.split_once(['-', '–']).ok_or_else(bad)?;
    let clock = |text: &str| NaiveTime::parse_from_str(text, "%H:%M").map_err(|_| bad());
    let (start, end) = (clock(start)?, clock(end)?);
    if end <= start {
        return Err(JotError::EndBeforeStart);
    }
    Ok((start, end))
}

/// A year for `/points 2025`: exactly four digits, from 2000 on (nobody
/// earned points before the bot existed). Anything else isn't a year, so
/// `/points 12` isn't taken for the year 12.
fn history_year(text: &str) -> Option<i32> {
    if text.len() != 4 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|year| *year >= 2000)
}

/// The part after `/settings`: a key word, then (for some) a value.
fn parse_settings(rest: &str) -> Option<SettingsCommand> {
    let (key, value) = match rest.split_once(char::is_whitespace) {
        Some((key, value)) => (key, value.trim()),
        None => (rest, ""),
    };

    // Both the key and whether there's a value decide.
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

    match words.as_slice() {
        [action, number] if action.eq_ignore_ascii_case("done") => {
            parse_item_number(number).map(ReplyCommand::Done)
        }
        [action, number] if action.eq_ignore_ascii_case("remove") => {
            parse_item_number(number).map(ReplyCommand::Remove)
        }
        [action, number] if action.eq_ignore_ascii_case("skip") => {
            parse_item_number(number).map(ReplyCommand::Skip)
        }
        // `due 2 fri`, `priority 2 high`, or with the value missing.
        [action, number, rest @ ..] if change_kind(action).is_some() => {
            // `?`: "due in 2 days" has no list number, so it's just chat.
            let n = parse_item_number(number)?;
            // Several words ("next fri") are kept together, so the reply
            // can say what it didn't understand.
            let value = rest.join(" ");
            let value = (!value.is_empty()).then_some(value.as_str());
            change_reply(change_kind(action)?, n, value)
        }
        [] => None,
        numbers => {
            // Every word must be a number, so "1 apple" is rejected as a whole.
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

/// Which flag a reply word changes: `due` or `priority` (`prio`).
fn change_kind(word: &str) -> Option<Flag> {
    match word.to_lowercase().as_str() {
        "due" => Some(Flag::Due),
        "priority" | "prio" => Some(Flag::Priority),
        _ => None,
    }
}

/// The reply for `due 2 <value>` / `priority 2 <value>`. `none` (or
/// `clear`, `off`) takes it off; a missing or bad value is `BadChange`.
fn change_reply(kind: Flag, n: usize, value: Option<&str>) -> Option<ReplyCommand> {
    let cleared =
        value.is_some_and(|v| matches!(v.to_lowercase().as_str(), "none" | "clear" | "off"));
    Some(match (kind, value) {
        (Flag::Due, _) if cleared => ReplyCommand::SetDue(n, None),
        (Flag::Priority, _) if cleared => ReplyCommand::SetPriority(n, None),
        (Flag::Due, None) => ReplyCommand::BadChange(FlagError::MissingDate),
        (Flag::Priority, None) => ReplyCommand::BadChange(FlagError::MissingPriority),
        (Flag::Due, Some(v)) => match Due::parse(v) {
            Some(due) => ReplyCommand::SetDue(n, Some(due)),
            None => ReplyCommand::BadChange(FlagError::BadDate(v.to_string())),
        },
        (Flag::Priority, Some(v)) => match Priority::parse(v) {
            Some(p) => ReplyCommand::SetPriority(n, Some(p)),
            None => ReplyCommand::BadChange(FlagError::BadPriority(v.to_string())),
        },
    })
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
/// "bins -d fri -p high" → a due date and priority (also for all of them).
/// `None` if there's no title or no name; `BadTodo` if the flags are wrong.
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

    let (title, flags) = split_flags(title);
    // `?`: no titles at all (empty, or only commas) is `None`, even with
    // flags: "+todo -d fri" isn't a todo.
    let titles = parse_items(title)?;
    match parse_flags(flags) {
        Ok((due, priority)) => Some(BotCommand::AddTodo {
            titles,
            assignee,
            due,
            priority,
        }),
        Err(err) => Some(BotCommand::BadTodo(err)),
    }
}

/// "fix the tap, bins -d fri -p high" → ("fix the tap, bins ", "-d fri -p high").
/// Everything from the first flag on is flags; with no flag, it's all title.
///
/// `split_inclusive` keeps the whitespace on the end of each piece
/// ("bins " rather than "bins"), so adding up the pieces' lengths gives
/// where each word starts. The title keeps its commas and new lines, which
/// is what separates several todos.
fn split_flags(text: &str) -> (&str, &str) {
    let mut start = 0;
    for piece in text.split_inclusive(char::is_whitespace) {
        if flag_kind(piece.trim_end()).is_some() {
            return (&text[..start], &text[start..]);
        }
        start += piece.len();
    }
    (text, "")
}

/// Which flag a word is: `-d`, `--due`, `--by` or `-p`, `--priority`.
/// Phones often turn a typed `--` into a dash (— or –), so those count too.
/// `None` for any other word, including `-` alone and "t-shirt".
fn flag_kind(word: &str) -> Option<Flag> {
    let word = word.to_lowercase();
    // "--due" must be tried before "-", or it would leave "-due".
    let name = word
        .strip_prefix("--")
        .or_else(|| word.strip_prefix('—'))
        .or_else(|| word.strip_prefix('–'))
        .or_else(|| word.strip_prefix('-'))?;
    match name {
        "d" | "due" | "by" => Some(Flag::Due),
        "p" | "priority" | "prio" => Some(Flag::Priority),
        _ => None,
    }
}

/// "-d fri -p high" → (Some(fri), Some(High)). Each flag needs a value
/// after it, at most once each, in either order; nothing else is allowed.
fn parse_flags(text: &str) -> Result<(Option<Due>, Option<Priority>), FlagError> {
    let mut due = None;
    let mut priority = None;
    let mut words = text.split_whitespace();

    // Taking a flag's value with `words.next()` means the loop skips over it.
    while let Some(word) = words.next() {
        let kind = flag_kind(word).ok_or_else(|| FlagError::NotAFlag(word.to_string()))?;
        // The value: the next word, unless it's another flag ("-d -p high"),
        // without a comma or full stop on the end ("-d fri.").
        let value = words
            .next()
            .filter(|value| flag_kind(value).is_none())
            .map(|value| value.trim_end_matches(['.', ',', '!']));

        match kind {
            Flag::Due => {
                if due.is_some() {
                    return Err(FlagError::Twice(Flag::Due));
                }
                let value = value.ok_or(FlagError::MissingDate)?;
                let parsed =
                    Due::parse(value).ok_or_else(|| FlagError::BadDate(value.to_string()))?;
                due = Some(parsed);
            }
            Flag::Priority => {
                if priority.is_some() {
                    return Err(FlagError::Twice(Flag::Priority));
                }
                let value = value.ok_or(FlagError::MissingPriority)?;
                let parsed = Priority::parse(value)
                    .ok_or_else(|| FlagError::BadPriority(value.to_string()))?;
                priority = Some(parsed);
            }
        }
    }
    Ok((due, priority))
}

/// "asr" → [Asr], "fajr, dhuhr" → [Fajr, Dhuhr], "all" → all five, with
/// `on time` / `late` and `yesterday` anywhere among them.
/// `None` if nothing is named or any word isn't a prayer, so a typo gets
/// no silent half-result.
fn parse_prayed(rest: &str) -> Option<Checkin> {
    let mut words: Vec<String> = rest
        .split([',', ' ', '\n'])
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();

    // Take out the words that aren't prayers. `on time` is two words, so
    // "on" waits for the "time" after it.
    let mut timing = Timing::Auto;
    let mut yesterday = false;
    let joined = words.join(" ");
    if joined.contains("on time") {
        timing = Timing::OnTime;
        words = joined
            .replace("on time", "ontime")
            .split(' ')
            .map(str::to_string)
            .collect();
    }
    words.retain(|word| match word.as_str() {
        "ontime" | "intime" => {
            timing = Timing::OnTime;
            false
        }
        "late" | "qada" | "qadha" | "qaza" => {
            timing = Timing::Late;
            false
        }
        "yesterday" | "yday" | "yest" => {
            yesterday = true;
            false
        }
        _ => true,
    });
    let checkin = |prayers| {
        Some(Checkin {
            prayers,
            timing,
            yesterday,
        })
    };

    if words == ["all"] {
        return checkin(Prayer::ALL.to_vec());
    }

    // `None` if any word didn't match.
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
    checkin(prayers)
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
    // 0 is `None`, because lists are numbered from 1.
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
        flagged(titles, mentioned, None, None)
    }

    /// A `+todo` with flags.
    fn flagged(
        titles: &[&str],
        mentioned: Option<&str>,
        due: Option<Due>,
        priority: Option<Priority>,
    ) -> Option<BotCommand> {
        Some(BotCommand::AddTodo {
            titles: titles.iter().map(|t| t.to_string()).collect(),
            assignee: mentioned.map(|name| Assignee::Mentioned(name.to_string())),
            due,
            priority,
        })
    }

    fn bad(err: FlagError) -> Option<BotCommand> {
        Some(BotCommand::BadTodo(err))
    }

    // ---- +todo flags ----

    #[test]
    fn todo_due_date_flag() {
        use chrono::Weekday;
        let fri = Some(Due::Weekday(Weekday::Fri));
        let passport = |due| flagged(&["renew passport"], None, due, None);
        assert_eq!(
            parse("+todo renew passport -d 15-03"),
            passport(Some(Due::DayMonth { day: 15, month: 3 }))
        );
        assert_eq!(parse("+todo renew passport --due fri"), passport(fri));
        assert_eq!(
            parse("+todo renew passport --by tmr"),
            passport(Some(Due::Tomorrow))
        );
        assert_eq!(
            parse("+todo renew passport -d today"),
            passport(Some(Due::Today))
        );
        assert_eq!(
            parse("+todo renew passport -D FRI"),
            passport(fri),
            "any case"
        );
        assert_eq!(
            parse("+todo renew passport -d fri."),
            passport(fri),
            "a full stop"
        );
        assert_eq!(
            parse("+todo renew passport -d 15/03/2027"),
            passport(Some(Due::Date(
                chrono::NaiveDate::from_ymd_opt(2027, 3, 15).unwrap()
            )))
        );
    }

    #[test]
    fn phones_turn_double_dashes_into_one_long_dash() {
        let fri = Some(Due::Weekday(chrono::Weekday::Fri));
        let bins = flagged(&["bins"], None, fri, None);
        assert_eq!(parse("+todo bins —due fri"), bins, "em dash");
        assert_eq!(parse("+todo bins –due fri"), bins, "en dash");
    }

    #[test]
    fn todo_priority_flag() {
        let bins = |p| flagged(&["bins"], None, None, Some(p));
        assert_eq!(parse("+todo bins -p high"), bins(Priority::High));
        assert_eq!(parse("+todo bins --priority h"), bins(Priority::High));
        assert_eq!(parse("+todo bins -p Med"), bins(Priority::Medium));
        assert_eq!(parse("+todo bins -p low"), bins(Priority::Low));
    }

    #[test]
    fn todo_with_both_flags_and_a_name() {
        let expected = flagged(
            &["book dentist"],
            Some("Khadija"),
            Some(Due::Weekday(chrono::Weekday::Mon)),
            Some(Priority::High),
        );
        assert_eq!(parse("+t @Khadija book dentist -d mon -p high"), expected);
        assert_eq!(
            parse("+t @Khadija book dentist -p high -d mon"),
            expected,
            "either order"
        );
    }

    #[test]
    fn flags_are_for_every_todo_in_the_message() {
        let fri = Some(Due::Weekday(chrono::Weekday::Fri));
        let low = Some(Priority::Low);
        assert_eq!(
            parse("+todo hoover, bins, and mop -d fri -p low"),
            flagged(&["hoover", "bins", "mop"], None, fri, low)
        );
        assert_eq!(
            parse("+t @Khadija hoover\nbins\n-d fri"),
            flagged(&["hoover", "bins"], Some("Khadija"), fri, None),
            "new lines, flags on their own line"
        );
    }

    #[test]
    fn dashes_in_titles_are_not_flags() {
        assert_eq!(parse("+todo buy a t-shirt"), todo("buy a t-shirt", None));
        assert_eq!(parse("+todo call mum - dad"), todo("call mum - dad", None));
        assert_eq!(parse("+todo watch -dune"), todo("watch -dune", None));
        assert_eq!(parse("+todo pay -£5 refund"), todo("pay -£5 refund", None));
    }

    #[test]
    fn flags_without_a_title_is_not_a_todo() {
        assert_eq!(parse("+todo -d fri"), None);
        assert_eq!(parse("+todo @Khadija -p high"), None);
    }

    #[test]
    fn bad_todo_flags_say_what_is_wrong() {
        assert_eq!(parse("+todo bins -d"), bad(FlagError::MissingDate));
        assert_eq!(parse("+todo bins -d -p high"), bad(FlagError::MissingDate));
        assert_eq!(parse("+todo bins -p"), bad(FlagError::MissingPriority));
        assert_eq!(
            parse("+todo bins -p urgent"),
            bad(FlagError::BadPriority("urgent".to_string()))
        );
        assert_eq!(
            parse("+todo bins -d friyay"),
            bad(FlagError::BadDate("friyay".to_string()))
        );
        assert_eq!(
            parse("+todo bins -d 31-13"),
            bad(FlagError::BadDate("31-13".to_string()))
        );
        assert_eq!(
            parse("+todo bins -d fri -d sat"),
            bad(FlagError::Twice(Flag::Due))
        );
        assert_eq!(
            parse("+todo bins -p high --priority low"),
            bad(FlagError::Twice(Flag::Priority))
        );
        // Anything after the flags that isn't a flag.
        assert_eq!(
            parse("+todo bins -d fri please"),
            bad(FlagError::NotAFlag("please".to_string()))
        );
        // Flags in the middle: the todos after them would be lost.
        assert_eq!(
            parse("+todo hoover -d fri, bins"),
            bad(FlagError::NotAFlag("bins".to_string()))
        );
    }

    // ---- +skip ----

    #[test]
    fn skip_todos() {
        assert_eq!(
            parse("+skip 2"),
            Some(BotCommand::SkipTodo(Got::Numbers(vec![2])))
        );
        assert_eq!(
            parse("+skip go gym"),
            Some(BotCommand::SkipTodo(Got::Names(vec!["go gym".to_string()])))
        );
        assert_eq!(parse("+skip"), None);
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
    fn prayer_summaries_and_reports() {
        assert_eq!(
            parse("/prayers week"),
            Some(BotCommand::PrayerSummary(Span::Week))
        );
        assert_eq!(
            parse("/p Month"),
            Some(BotCommand::PrayerSummary(Span::Month))
        );
        assert_eq!(
            parse("/prayers reports"),
            Some(BotCommand::PrayerReports(None))
        );
        assert_eq!(
            parse("/prayers reports here"),
            Some(BotCommand::PrayerReports(Some(Delivery::Here)))
        );
        assert_eq!(
            parse("/prayers reports group"),
            Some(BotCommand::PrayerReports(Some(Delivery::Here)))
        );
        assert_eq!(
            parse("/prayers REPORTS DM"),
            Some(BotCommand::PrayerReports(Some(Delivery::Dm)))
        );
        assert_eq!(
            parse("/prayer report off"),
            Some(BotCommand::PrayerReports(Some(Delivery::Off)))
        );
        assert_eq!(parse("/prayers reports sometimes"), None);
        assert_eq!(parse("/prayers weekly please"), None);
    }

    #[test]
    fn prayed_names_one_or_more_prayers() {
        assert_eq!(
            parse("+prayed asr"),
            Some(BotCommand::Prayed(Checkin::today(vec![Prayer::Asr])))
        );
        assert_eq!(
            parse("+Prayed Isha, fajr"),
            Some(BotCommand::Prayed(Checkin::today(vec![
                Prayer::Fajr,
                Prayer::Isha
            ])))
        );
        assert_eq!(
            parse("+prayed zuhr asr zohr"),
            Some(BotCommand::Prayed(Checkin::today(vec![
                Prayer::Dhuhr,
                Prayer::Asr
            ])))
        );
        assert_eq!(
            parse("+prayed all"),
            Some(BotCommand::Prayed(Checkin::today(Prayer::ALL.to_vec())))
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
    fn todo_reminders() {
        let on = Some(BotCommand::TodoReminders(Toggle::On));
        assert_eq!(parse("/todo reminders on"), on);
        assert_eq!(parse("/t Reminder ON"), on);
        assert_eq!(
            parse("/todo reminders off"),
            Some(BotCommand::TodoReminders(Toggle::Off))
        );
        assert_eq!(
            parse("/todo reminders"),
            Some(BotCommand::TodoReminders(Toggle::Show))
        );
        assert_eq!(parse("/todo reminders maybe"), None);
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

        let filtered = |f| Some(TodoView::Filtered(f));
        assert_eq!(view("/todo overdue"), filtered(TodoFilter::Overdue));
        assert_eq!(view("/t late"), filtered(TodoFilter::Overdue));
        assert_eq!(view("/todo due"), filtered(TodoFilter::Due));
        assert_eq!(view("/todo HIGH"), filtered(TodoFilter::High));
        assert_eq!(view("/todo due soon"), None, "two words isn't a name");
    }

    #[test]
    fn push_commands() {
        let push = |c| Some(BotCommand::Push(c));
        assert_eq!(parse("/push"), push(PushCommand::Status));
        assert_eq!(parse("/push status"), push(PushCommand::Status));
        assert_eq!(parse("/push on"), push(PushCommand::On));
        assert_eq!(parse("/Push ON"), push(PushCommand::On));
        assert_eq!(parse("/push off"), push(PushCommand::Off));
        assert_eq!(parse("/push test"), push(PushCommand::Test));
        assert_eq!(parse("/push maybe"), None);
        // Not a `+` command: there's nothing to add.
        assert_eq!(parse("+push on"), None);
    }

    #[test]
    fn moon_command() {
        assert_eq!(parse("/moon"), Some(BotCommand::Moon));
        assert_eq!(parse("/Moon"), Some(BotCommand::Moon));
        assert_eq!(parse("/moon tomorrow"), None);
    }

    #[test]
    fn points_command() {
        assert_eq!(parse("/points"), Some(BotCommand::Points));
        assert_eq!(parse("/Points"), Some(BotCommand::Points));
        assert_eq!(parse("/pts"), Some(BotCommand::Points));
        assert_eq!(parse("/point"), Some(BotCommand::Points));
        assert_eq!(parse("/points please"), None);

        let reports = |d| Some(BotCommand::PointsReports(d));
        assert_eq!(parse("/points reports"), reports(None));
        assert_eq!(parse("/pts report"), reports(None));
        assert_eq!(parse("/points reports here"), reports(Some(Delivery::Here)));
        assert_eq!(parse("/Points Reports ON"), reports(Some(Delivery::Here)));
        assert_eq!(parse("/points reports dm"), reports(Some(Delivery::Dm)));
        assert_eq!(
            parse("/points reports private"),
            reports(Some(Delivery::Dm))
        );
        assert_eq!(parse("/points reports off"), reports(Some(Delivery::Off)));
        assert_eq!(parse("/points reports weekly"), None);
    }

    #[test]
    fn points_history_commands() {
        let history = |h| Some(BotCommand::PointsHistory(h));
        assert_eq!(parse("/points weeks"), history(PointsHistory::Weeks));
        assert_eq!(parse("/pts week"), history(PointsHistory::Weeks));
        assert_eq!(parse("/Points WEEKS"), history(PointsHistory::Weeks));
        assert_eq!(parse("/points months"), history(PointsHistory::Months));
        assert_eq!(parse("/point month"), history(PointsHistory::Months));
        assert_eq!(parse("/points year"), history(PointsHistory::Year(None)));
        assert_eq!(
            parse("/points this year"),
            history(PointsHistory::Year(None))
        );
        assert_eq!(
            parse("/points 2026"),
            history(PointsHistory::Year(Some(2026)))
        );
        assert_eq!(
            parse("/points year 2025"),
            history(PointsHistory::Year(Some(2025)))
        );
        assert_eq!(
            parse("  /pts   2027  "),
            history(PointsHistory::Year(Some(2027)))
        );
    }

    #[test]
    fn points_history_rejects_what_isnt_a_year() {
        for text in [
            "/points 12",
            "/points 1999",
            "/points 20266",
            "/points 2o26",
            "/points year next",
            "/points year 26",
            "/points weeks 4",
            "/points last year",
            "/points -2026",
            "/points ２０２６",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
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
            Some(BotCommand::Prayed(Checkin::today(vec![Prayer::Fajr])))
        );
        assert_eq!(
            parse("+p 3 1"),
            Some(BotCommand::Prayed(Checkin::today(vec![
                Prayer::Fajr,
                Prayer::Asr
            ])))
        );
        assert_eq!(
            parse("+p 1, 2, 3, 4, 5"),
            Some(BotCommand::Prayed(Checkin::today(Prayer::ALL.to_vec())))
        );
        // Names and numbers can be mixed.
        assert_eq!(
            parse("+p isha 4"),
            Some(BotCommand::Prayed(Checkin::today(vec![
                Prayer::Maghrib,
                Prayer::Isha
            ])))
        );
        assert_eq!(parse("+p 0"), None);
    }

    #[test]
    fn prayed_can_say_on_time_late_and_yesterday() {
        let checkin = |prayers: &[Prayer], timing, yesterday| {
            Some(BotCommand::Prayed(Checkin {
                prayers: prayers.to_vec(),
                timing,
                yesterday,
            }))
        };
        assert_eq!(
            parse("+prayed asr on time"),
            checkin(&[Prayer::Asr], Timing::OnTime, false)
        );
        assert_eq!(
            parse("+p 1 3 ontime"),
            checkin(&[Prayer::Fajr, Prayer::Asr], Timing::OnTime, false)
        );
        assert_eq!(
            parse("+prayed fajr late"),
            checkin(&[Prayer::Fajr], Timing::Late, false)
        );
        assert_eq!(
            parse("+prayed qada fajr"),
            checkin(&[Prayer::Fajr], Timing::Late, false)
        );
        assert_eq!(
            parse("+prayed yesterday isha on time"),
            checkin(&[Prayer::Isha], Timing::OnTime, true)
        );
        assert_eq!(
            parse("+p yday all"),
            checkin(&Prayer::ALL, Timing::Auto, true)
        );
        assert_eq!(parse("+prayed on time"), None, "no prayer named");
        assert_eq!(parse("+prayed asr on"), None, "\"on\" alone isn't a prayer");
        assert_eq!(parse("+prayed asr sometime"), None);
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
        assert_eq!(parse_reply("skip 2"), Some(ReplyCommand::Skip(2)));
        assert_eq!(parse_reply("Skip 3"), Some(ReplyCommand::Skip(3)));
    }

    #[test]
    fn reply_changes_due_date_and_priority() {
        use chrono::Weekday;
        let due = |n, d| Some(ReplyCommand::SetDue(n, d));
        assert_eq!(
            parse_reply("due 2 fri"),
            due(2, Some(Due::Weekday(Weekday::Fri)))
        );
        assert_eq!(parse_reply("Due 1 TMR"), due(1, Some(Due::Tomorrow)));
        assert_eq!(
            parse_reply("due 3 15/03"),
            due(3, Some(Due::DayMonth { day: 15, month: 3 }))
        );
        assert_eq!(parse_reply("due 2 none"), due(2, None));
        assert_eq!(parse_reply("due 2 clear"), due(2, None));

        let priority = |n, p| Some(ReplyCommand::SetPriority(n, p));
        assert_eq!(
            parse_reply("priority 2 high"),
            priority(2, Some(Priority::High))
        );
        assert_eq!(parse_reply("prio 4 l"), priority(4, Some(Priority::Low)));
        assert_eq!(parse_reply("priority 2 off"), priority(2, None));
    }

    #[test]
    fn reply_changes_with_a_bad_value_say_so() {
        let bad = |e| Some(ReplyCommand::BadChange(e));
        assert_eq!(parse_reply("due 2"), bad(FlagError::MissingDate));
        assert_eq!(parse_reply("priority 2"), bad(FlagError::MissingPriority));
        assert_eq!(
            parse_reply("due 2 friyay"),
            bad(FlagError::BadDate("friyay".to_string()))
        );
        assert_eq!(
            parse_reply("due 2 next fri"),
            bad(FlagError::BadDate("next fri".to_string()))
        );
        assert_eq!(
            parse_reply("priority 1 urgent"),
            bad(FlagError::BadPriority("urgent".to_string()))
        );
    }

    #[test]
    fn chat_that_starts_with_due_is_not_a_change() {
        // No list number after the word: an ordinary reply.
        assert_eq!(parse_reply("due in 2 days"), None);
        assert_eq!(parse_reply("due tomorrow"), None);
        assert_eq!(parse_reply("priority is the tap"), None);
        assert_eq!(parse_reply("due 0 fri"), None, "lists start at 1");
    }

    #[test]
    fn reply_that_is_not_a_list_action_is_none() {
        for text in [
            "", "thanks", "1 apple", "0", "-1", "done", "done 0", "done two", "done 2 3", "remove",
            "skip", "skip 0", "skip it",
        ] {
            assert_eq!(
                parse_reply(text),
                None,
                "{text:?} should not be a list action"
            );
        }
    }

    // ---- /jot ----

    fn jot(text: &str) -> Option<JotCommand> {
        match parse(text) {
            Some(BotCommand::Jot(command)) => Some(command),
            _ => None,
        }
    }

    fn time(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn on(minutes: Option<u32>, hours: Option<(NaiveTime, NaiveTime)>) -> Option<JotCommand> {
        Some(JotCommand::On {
            interval_minutes: minutes,
            hours,
            days: None,
        })
    }

    fn days_of(text: &str) -> Option<Vec<chrono::Weekday>> {
        let JotCommand::On { days, .. } = jot(&format!("/jot on -days {text}"))? else {
            return None;
        };
        let days = days?;
        Some(
            (0..7)
                .filter_map(|n| chrono::Weekday::try_from(n).ok())
                .filter(|d| days.contains(*d))
                .collect(),
        )
    }

    #[test]
    fn jot_days_ranges_lists_and_names() {
        use chrono::Weekday::{Fri, Mon, Sat, Sun, Thu, Tue, Wed};
        assert_eq!(days_of("mon-fri"), Some(vec![Mon, Tue, Wed, Thu, Fri]));
        assert_eq!(days_of("MON-FRI"), Some(vec![Mon, Tue, Wed, Thu, Fri]));
        assert_eq!(
            days_of("monday-friday"),
            Some(vec![Mon, Tue, Wed, Thu, Fri])
        );
        assert_eq!(days_of("mon,wed,fri"), Some(vec![Mon, Wed, Fri]));
        assert_eq!(days_of("sat"), Some(vec![Sat]));
        assert_eq!(days_of("fri-mon"), Some(vec![Mon, Fri, Sat, Sun]));
        assert_eq!(days_of("weekdays"), Some(vec![Mon, Tue, Wed, Thu, Fri]));
        assert_eq!(days_of("weekends"), Some(vec![Sat, Sun]));
        assert_eq!(days_of("daily").map(|d| d.len()), Some(7));
        assert_eq!(
            jot("/jot on 2h 09:00-18:00 -days mon-fri"),
            Some(JotCommand::On {
                interval_minutes: Some(120),
                hours: Some((time(9, 0), time(18, 0))),
                days: JotDays::parse("mon-fri"),
            })
        );
        for bad in ["", "funday", "mon-", "mon,,fri", "mon-xyz"] {
            assert!(
                matches!(
                    jot(&format!("/jot on -days {bad}")),
                    Some(JotCommand::Invalid(JotError::BadDays(_)))
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn jot_days_say_themselves_back() {
        let label = |t: &str| JotDays::parse(t).unwrap().label();
        assert_eq!(label("daily"), "every day");
        assert_eq!(label("mon-fri"), "Mon–Fri");
        assert_eq!(label("sat,sun"), "Sat–Sun");
        assert_eq!(label("mon,wed,fri"), "Mon, Wed, Fri");
        assert_eq!(JotDays::from_bits(0), None);
        assert_eq!(JotDays::from_bits(128), None);
        assert_eq!(JotDays::from_bits(31), JotDays::parse("mon-fri"));
    }

    #[test]
    fn jot_pause_for_a_time_or_until_tonight() {
        let pause = |minutes| Some(JotCommand::Pause(JotPause::For { minutes }));
        assert_eq!(jot("/jot pause 3h"), pause(180));
        assert_eq!(jot("/j pause 90m"), pause(90));
        assert_eq!(jot("/jot PAUSE 1h"), pause(60));
        assert_eq!(
            jot("/jot pause today"),
            Some(JotCommand::Pause(JotPause::Today))
        );
        for bad in ["", "3", "h", "0h", "-1h", "soon", "99h", "3h today"] {
            assert!(
                matches!(
                    jot(&format!("/jot pause {bad}")),
                    Some(JotCommand::Invalid(JotError::BadPause(_)))
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn jot_status_on_and_off() {
        assert_eq!(jot("/jot"), Some(JotCommand::Status));
        assert_eq!(jot("/JOT status"), Some(JotCommand::Status));
        assert_eq!(jot("/jot off"), Some(JotCommand::Off));
        assert_eq!(jot("/jot on"), on(None, None));
        assert_eq!(
            jot("/jot on 2h 09:00-18:00"),
            on(Some(120), Some((time(9, 0), time(18, 0))))
        );
    }

    #[test]
    fn jot_intervals_and_hours_can_come_in_either_order_and_alone() {
        assert_eq!(jot("/jot on 1h"), on(Some(60), None));
        assert_eq!(jot("/jot on 90m"), on(Some(90), None));
        assert_eq!(jot("/jot on 3h"), on(Some(180), None));
        assert_eq!(jot("/jot on 30m"), on(Some(30), None));
        assert_eq!(jot("/jot on 4h"), on(Some(240), None));
        assert_eq!(
            jot("/jot on 8:30-17:00"),
            on(None, Some((time(8, 30), time(17, 0))))
        );
        assert_eq!(
            jot("/jot on 09:00-18:00 2h"),
            on(Some(120), Some((time(9, 0), time(18, 0))))
        );
    }

    #[test]
    fn jot_today_week_and_days() {
        assert_eq!(jot("/jot today"), Some(JotCommand::Day(JotDay::Today)));
        assert_eq!(jot("/j today"), Some(JotCommand::Day(JotDay::Today)));
        assert_eq!(
            jot("/jot yesterday"),
            Some(JotCommand::Day(JotDay::Yesterday))
        );
        assert_eq!(jot("/jot WEEK"), Some(JotCommand::Week));
        assert_eq!(
            jot("/jot mon"),
            Some(JotCommand::Day(JotDay::Weekday(chrono::Weekday::Mon)))
        );
        assert_eq!(jot("/jot today please"), None);
    }

    #[test]
    fn jot_short_forms() {
        assert_eq!(jot("/j"), Some(JotCommand::Status));
        assert_eq!(jot("/j on 2h"), on(Some(120), None));
        assert_eq!(jot("/j off"), Some(JotCommand::Off));
        assert_eq!(
            jot("+jot wrote the report"),
            Some(JotCommand::Log("wrote the report".to_string()))
        );
        assert_eq!(
            jot("+j Called the School, then lunch"),
            Some(JotCommand::Log("Called the School, then lunch".to_string())),
            "the entry keeps its case"
        );
    }

    #[test]
    fn jot_with_nothing_to_log_or_an_unknown_word_is_not_a_command() {
        for text in [
            "+jot",
            "+j",
            "/jot maybe",
            "/jot off now",
            "/jota",
            "jot hello",
        ] {
            assert_eq!(parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn jot_rejects_a_bad_interval() {
        for word in [
            "5h", "20m", "29m", "241m", "0h", "2", "h", "2.5h", "2hrs", "-2h",
        ] {
            let found = jot(&format!("/jot on {word}"));
            assert!(
                matches!(found, Some(JotCommand::Invalid(_))),
                "{word:?} gave {found:?}"
            );
        }
        assert_eq!(
            jot("/jot on 5h"),
            Some(JotCommand::Invalid(JotError::BadInterval("5h".to_string())))
        );
    }

    #[test]
    fn jot_rejects_bad_hours_and_an_end_before_the_start() {
        for word in [
            "9-18",
            "25:00-26:00",
            "09:00-",
            "-18:00",
            "09:00:18:00",
            "09:00-18:61",
        ] {
            let found = jot(&format!("/jot on {word}"));
            assert!(
                matches!(found, Some(JotCommand::Invalid(JotError::BadHours(_)))),
                "{word:?} gave {found:?}"
            );
        }
        assert_eq!(
            jot("/jot on 18:00-09:00"),
            Some(JotCommand::Invalid(JotError::EndBeforeStart))
        );
        assert_eq!(
            jot("/jot on 09:00-09:00"),
            Some(JotCommand::Invalid(JotError::EndBeforeStart)),
            "no hours at all"
        );
    }
}

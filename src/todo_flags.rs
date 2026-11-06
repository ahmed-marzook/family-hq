//! The flags that can follow a `+todo`: a due date (`-d fri`) and a
//! priority (`-p high`).
//!
//! Pure, like the parser: no clock. A due date is kept *as typed* (`Due`)
//! until the handler knows the sender's "today" and calls `Due::resolve`.
//! That way `-d fri` can be tested for any day of the week.

use chrono::{Datelike, Days, NaiveDate, Weekday};

/// How urgent a todo is. Stored as its `as_str` text (the schema's CHECK
/// allows exactly these three).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    High,
    Medium,
    Low,
}

impl Priority {
    /// `high`, `h`, `hi`, ... ignoring case. `None` for anything else.
    pub fn parse(word: &str) -> Option<Priority> {
        match word.to_lowercase().as_str() {
            "high" | "hi" | "h" => Some(Priority::High),
            "medium" | "med" | "mid" | "m" => Some(Priority::Medium),
            "low" | "lo" | "l" => Some(Priority::Low),
            _ => None,
        }
    }

    /// Where a todo with this priority goes on a list: high (0) first, low
    /// (2) last. No priority counts as medium.
    pub fn rank(priority: Option<Priority>) -> u8 {
        match priority {
            Some(Priority::High) => 0,
            Some(Priority::Medium) | None => 1,
            Some(Priority::Low) => 2,
        }
    }

    /// The text stored in `todos.priority`.
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::High => "high",
            Priority::Medium => "medium",
            Priority::Low => "low",
        }
    }

    /// 🔴 / 🟠 / 🟢
    pub fn emoji(self) -> &'static str {
        match self {
            Priority::High => HIGH,
            Priority::Medium => MEDIUM,
            Priority::Low => LOW,
        }
    }
}

const HIGH: &str = "🔴";
const MEDIUM: &str = "🟠";
const LOW: &str = "🟢";

/// A due date as typed, before it's turned into a real date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// `today`
    Today,
    /// `tmr`, `tmrw`, `tomorrow`
    Tomorrow,
    /// `fri`, `friday`: the next one, or today if it's that day already.
    Weekday(Weekday),
    /// `15-03` (or `15/03`): this year, or next year if it's gone by.
    DayMonth { day: u32, month: u32 },
    /// `15-03-2027`: exactly that date.
    Date(NaiveDate),
}

impl Due {
    /// `fri`, `tmr`, `15-03`, `15/03/2027`, ... `None` if it isn't a date.
    /// Only checks the shape here; whether `31-04` exists is checked in
    /// `resolve`, which knows the year.
    pub fn parse(word: &str) -> Option<Due> {
        let word = word.to_lowercase();
        match word.as_str() {
            "today" | "tdy" => return Some(Due::Today),
            "tomorrow" | "tmr" | "tmrw" | "tmw" => return Some(Due::Tomorrow),
            _ => {}
        }
        if let Some(day) = weekday(&word) {
            return Some(Due::Weekday(day));
        }

        // "15-03-2027" → ["15", "03", "2027"]. Both - and / work.
        let parts: Vec<&str> = word.split(['-', '/']).collect();
        match parts.as_slice() {
            [day, month] => {
                let (day, month) = (day.parse().ok()?, month.parse().ok()?);
                let plausible = (1..=31).contains(&day) && (1..=12).contains(&month);
                plausible.then_some(Due::DayMonth { day, month })
            }
            // A 4-digit year only: "15-03-27" could mean 1927 or 2027.
            [day, month, year] if year.len() == 4 => {
                NaiveDate::from_ymd_opt(year.parse().ok()?, month.parse().ok()?, day.parse().ok()?)
                    .map(Due::Date)
            }
            _ => None,
        }
    }

    /// The real date, counting from `today` (the sender's own today).
    pub fn resolve(self, today: NaiveDate) -> Result<NaiveDate, FlagError> {
        match self {
            Due::Today => Ok(today),
            // `checked_add_days` is `None` only past the year 262,143.
            Due::Tomorrow => today
                .checked_add_days(Days::new(1))
                .ok_or(FlagError::NoSuchDate),
            Due::Weekday(day) => {
                // `num_days_from_monday`: Mon 0 … Sun 6. The `+ 7` and `% 7`
                // keep it from going negative: Fri (4) asked on Sat (5) is
                // (4 + 7 - 5) % 7 = 6 days on.
                let ahead =
                    (day.num_days_from_monday() + 7 - today.weekday().num_days_from_monday()) % 7;
                today
                    .checked_add_days(Days::new(ahead.into()))
                    .ok_or(FlagError::NoSuchDate)
            }
            Due::DayMonth { day, month } => {
                // This year, unless that's gone by (or doesn't exist, like
                // 29-02 in a normal year); then next year.
                let this_year = NaiveDate::from_ymd_opt(today.year(), month, day);
                match this_year {
                    Some(date) if date >= today => Ok(date),
                    _ => NaiveDate::from_ymd_opt(today.year() + 1, month, day)
                        .ok_or(FlagError::NoSuchDate),
                }
            }
            Due::Date(date) if date < today => Err(FlagError::Past(date)),
            Due::Date(date) => Ok(date),
        }
    }
}

/// `fri`, `friday`, `thurs`, ... (already lowercase).
fn weekday(word: &str) -> Option<Weekday> {
    match word {
        "mon" | "monday" => Some(Weekday::Mon),
        "tue" | "tues" | "tuesday" => Some(Weekday::Tue),
        "wed" | "weds" | "wednesday" => Some(Weekday::Wed),
        "thu" | "thur" | "thurs" | "thursday" => Some(Weekday::Thu),
        "fri" | "friday" => Some(Weekday::Fri),
        "sat" | "saturday" => Some(Weekday::Sat),
        "sun" | "sunday" => Some(Weekday::Sun),
        _ => None,
    }
}

/// A due date as the bot shows it: "Fri 13 Nov", with the year only when
/// it isn't this year's ("Mon 15 Mar 2027").
pub fn show_date(date: NaiveDate, today: NaiveDate) -> String {
    // `%-d`: the day without a leading zero ("3", not "03").
    if date.year() == today.year() {
        date.format("%a %-d %b").to_string()
    } else {
        date.format("%a %-d %b %Y").to_string()
    }
}

/// Which flag, for messages about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    Due,
    Priority,
}

/// Why the flags on a `+todo` couldn't be used. The `#[error]` text is
/// what the bot replies (after a ❓), so it's written for the family.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FlagError {
    #[error("`-d` needs a date after it, like `-d fri`, `-d tmr` or `-d 15-03`.")]
    MissingDate,

    #[error("`-p` needs `high`, `medium` or `low` after it, like `-p high`.")]
    MissingPriority,

    #[error(
        "I don't understand the date \"{0}\". Try `today`, `tmr`, a day like `fri`, \
         or `15-03` / `15-03-2027`."
    )]
    BadDate(String),

    #[error("Priority can be `high`, `medium` or `low`, not \"{0}\".")]
    BadPriority(String),

    // `.0` is the variant's first field; `*` copies it out of the `&self`
    // that thiserror's generated `Display` has.
    #[error("{} is there twice. Give it once, at the end.", flag_name(*.0))]
    Twice(Flag),

    #[error(
        "I don't understand \"{0}\". Flags go at the end and are for every todo \
         in the message: `+todo fix the tap, bins -d fri -p high`."
    )]
    NotAFlag(String),

    #[error("That date has already passed ({}).", .0.format("%a %-d %b %Y"))]
    Past(NaiveDate),

    #[error("There's no such date.")]
    NoSuchDate,
}

fn flag_name(flag: Flag) -> &'static str {
    match flag {
        Flag::Due => "The due date (`-d`)",
        Flag::Priority => "The priority (`-p`)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// Friday 6 November 2026.
    fn friday() -> NaiveDate {
        date(2026, 11, 6)
    }

    fn resolve(word: &str, today: NaiveDate) -> Result<NaiveDate, FlagError> {
        Due::parse(word).expect("should parse").resolve(today)
    }

    #[test]
    fn priorities() {
        assert_eq!(Priority::parse("high"), Some(Priority::High));
        assert_eq!(Priority::parse("H"), Some(Priority::High));
        assert_eq!(Priority::parse("Med"), Some(Priority::Medium));
        assert_eq!(Priority::parse("l"), Some(Priority::Low));
        assert_eq!(Priority::parse("urgent"), None);
        assert_eq!(Priority::parse(""), None);
    }

    #[test]
    fn today_and_tomorrow() {
        assert_eq!(resolve("today", friday()), Ok(friday()));
        assert_eq!(resolve("TMR", friday()), Ok(date(2026, 11, 7)));
        assert_eq!(resolve("tomorrow", friday()), Ok(date(2026, 11, 7)));
        // Across a year end.
        assert_eq!(resolve("tmr", date(2026, 12, 31)), Ok(date(2027, 1, 1)));
    }

    #[test]
    fn weekdays_are_the_next_one_or_today() {
        assert_eq!(resolve("fri", friday()), Ok(friday()), "today counts");
        assert_eq!(resolve("sat", friday()), Ok(date(2026, 11, 7)));
        assert_eq!(resolve("Monday", friday()), Ok(date(2026, 11, 9)));
        assert_eq!(
            resolve("thurs", friday()),
            Ok(date(2026, 11, 12)),
            "6 days on"
        );
    }

    #[test]
    fn day_and_month_is_the_next_one() {
        assert_eq!(resolve("15-11", friday()), Ok(date(2026, 11, 15)));
        assert_eq!(resolve("6/11", friday()), Ok(friday()), "today counts");
        assert_eq!(
            resolve("15-03", friday()),
            Ok(date(2027, 3, 15)),
            "next year"
        );
        assert_eq!(resolve("01-01", friday()), Ok(date(2027, 1, 1)));
    }

    #[test]
    fn day_and_month_that_doesnt_exist() {
        assert_eq!(resolve("31-04", friday()), Err(FlagError::NoSuchDate));
        // 2027 isn't a leap year either.
        assert_eq!(resolve("29-02", friday()), Err(FlagError::NoSuchDate));
        assert_eq!(
            resolve("29-02", date(2027, 6, 1)),
            Ok(date(2028, 2, 29)),
            "the next leap year"
        );
    }

    #[test]
    fn full_dates() {
        assert_eq!(resolve("15-03-2027", friday()), Ok(date(2027, 3, 15)));
        assert_eq!(resolve("06/11/2026", friday()), Ok(friday()));
        assert_eq!(
            resolve("15-03-2026", friday()),
            Err(FlagError::Past(date(2026, 3, 15)))
        );
    }

    #[test]
    fn not_dates() {
        for word in [
            "",
            "friyay",
            "32-01",
            "15-13",
            "0-5",
            "15",
            "15-03-27",
            "31-02-2027",
            "a-b",
            "15-03-2027-1",
            "-1-03",
        ] {
            assert_eq!(Due::parse(word), None, "{word:?}");
        }
    }

    #[test]
    fn shown_dates() {
        assert_eq!(show_date(date(2026, 11, 13), friday()), "Fri 13 Nov");
        assert_eq!(show_date(date(2026, 12, 1), friday()), "Tue 1 Dec");
        assert_eq!(show_date(date(2027, 3, 15), friday()), "Mon 15 Mar 2027");
    }

    #[test]
    fn error_messages() {
        assert_eq!(
            FlagError::Twice(Flag::Priority).to_string(),
            "The priority (`-p`) is there twice. Give it once, at the end."
        );
        assert_eq!(
            FlagError::Past(date(2026, 3, 15)).to_string(),
            "That date has already passed (Sun 15 Mar 2026)."
        );
    }
}

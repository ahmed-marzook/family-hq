//! Points: what closing a todo is worth, and how `/points` shows them.
//!
//! Pure functions only, like `parser.rs` and `prayer/stats.rs`: the facts
//! come in, a number or the text goes out. Saving and reading the points is
//! `repo::points`'s job.
//!
//! The rules (whoever closes the todo earns them):
//!
//! | Closed as                         | Points                          |
//! | --------------------------------- | ------------------------------- |
//! | Done, 🔴 high                     | 3                               |
//! | Done, medium or no priority       | 2                               |
//! | Done, 🟢 low                      | 1                               |
//! | Done on or before its due date    | +1                              |
//! | Done after its due date           | −1, but never less than 1 point |
//! | Skipped                           | −1                              |
//!
//! Removing a todo is neither: it earns nothing and costs nothing.

use chrono::NaiveDate;

use crate::progress;
use crate::reports::Period;
use crate::todo::flags::Priority;

pub mod award;
pub mod reports;

pub const STAR: &str = "⭐";
pub const TROPHY: &str = "🏆";
const GOLD: &str = "🥇";
const SILVER: &str = "🥈";
const BRONZE: &str = "🥉";
/// A proper minus sign, which reads better than a hyphen in "−1".
const MINUS: &str = "−";

/// What earned the points, as `points.source` stores it. Habits and goals
/// come in Stage 2.5 (the schema allows them already).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Todo,
}

impl Source {
    /// The exact text stored in the database (and allowed by its CHECK).
    pub fn as_db_str(self) -> &'static str {
        match self {
            Source::Todo => "todo",
        }
    }
}

/// How a todo was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    Done,
    /// Closed as "won't be done".
    Skipped,
}

/// Why a todo earned what it did, as `points.reason` stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Done, and it had no due date.
    Done,
    /// Done on or before its due date.
    DoneOnTime,
    /// Done after its due date.
    DoneLate,
    Skipped,
}

impl Reason {
    /// The exact text stored in the database (and allowed by its CHECK).
    pub fn as_db_str(self) -> &'static str {
        match self {
            Reason::Done => "done",
            Reason::DoneOnTime => "done_on_time",
            Reason::DoneLate => "done_late",
            Reason::Skipped => "skipped",
        }
    }
}

/// What one closed todo is worth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Award {
    pub points: i64,
    pub reason: Reason,
}

/// The points for a todo closed as `finish` on `closed_on`: the closer's
/// own local day, so "on time" means on time on their clock, the same
/// clock `-d fri` was worked out on.
pub fn for_todo(
    finish: Finish,
    priority: Option<Priority>,
    due: Option<NaiveDate>,
    closed_on: NaiveDate,
) -> Award {
    if finish == Finish::Skipped {
        return Award {
            points: -1,
            reason: Reason::Skipped,
        };
    }
    let base = match priority {
        Some(Priority::High) => 3,
        Some(Priority::Medium) | None => 2,
        Some(Priority::Low) => 1,
    };
    // A match guard (`if ...`) on a pattern that binds `due`: the arm only
    // matches when the guard is true, otherwise the next arm is tried.
    match due {
        None => Award {
            points: base,
            reason: Reason::Done,
        },
        Some(due) if closed_on <= due => Award {
            points: base + 1,
            reason: Reason::DoneOnTime,
        },
        Some(_) => Award {
            // `max(1)`: late still earns something, so it's worth doing.
            points: (base - 1).max(1),
            reason: Reason::DoneLate,
        },
    }
}

/// "+4", "−1", "0": a change in points, with its sign.
pub fn signed(points: i64) -> String {
    match points {
        p if p > 0 => format!("+{p}"),
        p if p < 0 => format!("{MINUS}{}", p.unsigned_abs()),
        _ => "0".to_string(),
    }
}

/// A total, with a proper minus sign if it's below zero ("−2", "18").
fn total(points: i64) -> String {
    if points < 0 {
        signed(points)
    } else {
        points.to_string()
    }
}

/// One line of a leaderboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked<'a> {
    /// 1 for first. People on the same points share a place, and the next
    /// place is skipped: 1, 1, 3 (as in sport).
    pub place: usize,
    pub name: &'a str,
    pub points: i64,
}

/// Puts `(name, points)` in leaderboard order: most points first, then by
/// name so the order is the same every time.
///
/// The `'a` in `Ranked<'a>` says each `name` borrows from `totals`, so the
/// result can't outlive the list it came from. Rust checks that at compile
/// time, and nothing is copied.
pub fn rank<'a>(totals: &'a [(String, i64)]) -> Vec<Ranked<'a>> {
    let mut sorted: Vec<&(String, i64)> = totals.iter().collect();
    // `cmp` on `b` then `a` sorts high to low; `then_with` breaks ties.
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut ranked: Vec<Ranked<'a>> = Vec::with_capacity(sorted.len());
    for (i, (name, points)) in sorted.into_iter().enumerate() {
        // Same points as the one above: same place. Otherwise the place is
        // the position in the list, counting from 1.
        let place = match ranked.last() {
            Some(above) if above.points == *points => above.place,
            _ => i + 1,
        };
        ranked.push(Ranked {
            place,
            name,
            points: *points,
        });
    }
    ranked
}

/// 🥇 / 🥈 / 🥉 for the top three places, "4." and so on after that.
pub fn medal(place: usize) -> String {
    match place {
        1 => GOLD.to_string(),
        2 => SILVER.to_string(),
        3 => BRONZE.to_string(),
        n => format!("{n}."),
    }
}

/// "Mon 2 Nov – Sun 8 Nov"
pub fn show_period(period: Period) -> String {
    format!(
        "{} – {}",
        period.first.format("%a %-d %b"),
        period.last.format("%a %-d %b")
    )
}

/// What a todo is worth, for the empty `/points` and the help page.
pub const RULES: &str = "🔴 high 3 · medium 2 · 🟢 low 1, +1 if it's done by its due \
                         date (−1 if after), and a skip costs 1";

/// `/points` in a group: this week's leaderboard for the group, and who has
/// earned what today. Each list is `(name, points)`, anyone with no points
/// left out. Someone whose day evens out at 0 (a done and a skip) isn't
/// named in "Today:".
pub fn group_text(week: Period, totals: &[(String, i64)], today: &[(String, i64)]) -> String {
    if totals.is_empty() {
        return format!("{TROPHY} No points yet this week. Tick off a todo to earn some: {RULES}.");
    }
    let mut lines = vec![format!(
        "{TROPHY} *Points this week* ({})",
        show_period(week)
    )];
    for r in rank(totals) {
        lines.push(format!("{} {} {}", medal(r.place), r.name, total(r.points)));
    }
    let today: Vec<String> = rank(today)
        .iter()
        .filter(|r| r.points != 0)
        .map(|r| format!("{} {}", r.name, signed(r.points)))
        .collect();
    if !today.is_empty() {
        lines.push(String::new());
        lines.push(format!("Today: {}", today.join(" · ")));
    }
    lines.join("\n")
}

/// What `/points` in a DM shows: the person's own points from every chat.
#[derive(Debug, Clone, PartialEq)]
pub struct Mine {
    /// The last seven days, oldest first, each with its points (0 for a day
    /// with none).
    pub last_7_days: Vec<(NaiveDate, i64)>,
    pub this_week: i64,
    pub last_week: i64,
    /// This week's points in each chat: the group's name, or `None` for
    /// their DM with the bot.
    pub by_chat: Vec<(Option<String>, i64)>,
}

/// `/points` in a DM.
pub fn mine_text(mine: &Mine) -> String {
    let today = mine.last_7_days.last().map_or(0, |(_, p)| *p);
    let mut lines = vec![
        format!("{STAR} *Your points*"),
        format!(
            "Today: {} · This week: {} · Last week: {}",
            total(today),
            total(mine.this_week),
            total(mine.last_week)
        ),
    ];

    // Each day's bar is in proportion to the best day, so the best is full.
    let best = mine.last_7_days.iter().map(|(_, p)| *p).max().unwrap_or(0);
    if best > 0 {
        lines.push(String::new());
        lines.push("*Last 7 days*".to_string());
        for (day, points) in &mine.last_7_days {
            lines.push(format!(
                "{} {} {}",
                day.format("%a %-d"),
                progress::bar(*points, best),
                total(*points)
            ));
        }
    }

    // Only worth showing when the points came from more than one chat.
    if mine.by_chat.len() > 1 {
        let chats: Vec<String> = mine
            .by_chat
            .iter()
            .map(|(name, p)| format!("{} {}", name.as_deref().unwrap_or("Just me"), total(*p)))
            .collect();
        lines.push(String::new());
        lines.push(format!("This week by chat: {}", chats.join(" · ")));
    }

    if mine.this_week == 0 && best <= 0 {
        lines.push(String::new());
        lines.push(format!("Tick off a todo to earn some: {RULES}."));
    }
    lines.join("\n")
}

// ---- The Sunday weekly summary (`points::reports`) ----

const UP: &str = "⬆️";
const DOWN: &str = "⬇️";
const SPARKLE: &str = "🌟";

/// One person's points on one day in one group: what the weekly summary is
/// made from.
#[derive(Debug, Clone, PartialEq)]
pub struct DayPoints {
    pub user_id: i64,
    /// Display name. Unique (the schema says so), so it can be a key.
    pub name: String,
    pub day: NaiveDate,
    pub points: i64,
}

/// Each person's total over `days`, as `(user_id, name, points)`, in the
/// order they first appear.
pub fn totals_by_person(days: &[DayPoints]) -> Vec<(i64, String, i64)> {
    let mut totals: Vec<(i64, String, i64)> = Vec::new();
    for d in days {
        // `iter_mut().find`: a `&mut` to the matching entry, to add to it.
        match totals.iter_mut().find(|(id, _, _)| *id == d.user_id) {
            Some((_, _, total)) => *total += d.points,
            None => totals.push((d.user_id, d.name.clone(), d.points)),
        }
    }
    totals
}

/// " ⬆️ 6" / " ⬇️ 4" against last week, or nothing if it's the same.
fn change(now: i64, before: i64) -> String {
    match now - before {
        d if d > 0 => format!(" {UP} {d}"),
        d if d < 0 => format!(" {DOWN} {}", d.unsigned_abs()),
        _ => String::new(),
    }
}

/// The best single day in `days` (most points), if any earned something.
/// On a tie, the earliest day wins, then the name, so it's the same every
/// time.
fn best_day(days: &[DayPoints]) -> Option<&DayPoints> {
    // `max_by` keeps the *last* of equal items, so the tie-breakers are
    // reversed: a later day or later name counts as "smaller".
    days.iter().filter(|d| d.points > 0).max_by(|a, b| {
        a.points
            .cmp(&b.points)
            .then_with(|| b.day.cmp(&a.day))
            .then_with(|| b.name.cmp(&a.name))
    })
}

/// A group's weekly summary: the week's leaderboard, each person's change
/// from last week (`last_week` is `(name, points)`), and the best day.
/// `None` if nobody earned anything, so there's nothing to say.
pub fn week_text(week: Period, days: &[DayPoints], last_week: &[(String, i64)]) -> Option<String> {
    let totals: Vec<(String, i64)> = totals_by_person(days)
        .into_iter()
        .map(|(_, name, points)| (name, points))
        .collect();
    if totals.is_empty() {
        return None;
    }
    let mut lines = vec![format!(
        "{TROPHY} *This week's points* ({})",
        show_period(week)
    )];
    for r in rank(&totals) {
        // Only compared with someone who earned something last week.
        let change = last_week
            .iter()
            .find(|(name, _)| name == r.name)
            .map_or(String::new(), |(_, before)| change(r.points, *before));
        lines.push(format!(
            "{} {} {}{change}",
            medal(r.place),
            r.name,
            total(r.points)
        ));
    }
    if let Some(best) = best_day(days) {
        lines.push(String::new());
        lines.push(format!(
            "{SPARKLE} Best day: {}, {} on {}",
            best.name,
            best.points,
            best.day.format("%a %-d %b")
        ));
    }
    Some(lines.join("\n"))
}

/// One person's own week, sent privately (`/points reports dm`). `days` are
/// only theirs; `last_week` is their total then, if they earned any.
pub fn own_week_text(
    group: Option<&str>,
    week: Period,
    days: &[DayPoints],
    last_week: Option<i64>,
) -> String {
    let points: i64 = days.iter().map(|d| d.points).sum();
    let group = group.unwrap_or("the group");
    let change = last_week.map_or(String::new(), |before| {
        let change = change(points, before);
        if change.is_empty() {
            " (the same as last week)".to_string()
        } else {
            format!("{change} on last week")
        }
    });
    let mut lines = vec![
        format!("{STAR} *Your week in {group}* ({})", show_period(week)),
        format!("{} points{change}", total(points)),
    ];
    if let Some(best) = best_day(days) {
        lines.push(format!(
            "{SPARKLE} Best day: {}, {}",
            best.day.format("%a %-d %b"),
            best.points
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::date;

    const DUE: &str = "2026-11-13";

    /// The points for a todo done on `closed_on`, with `due` (or none).
    fn done(priority: Option<Priority>, due: Option<&str>, closed_on: &str) -> Award {
        for_todo(Finish::Done, priority, due.map(date), date(closed_on))
    }

    #[test]
    fn priority_sets_the_base() {
        let d = "2026-11-06";
        assert_eq!(done(Some(Priority::High), None, d).points, 3);
        assert_eq!(done(Some(Priority::Medium), None, d).points, 2);
        assert_eq!(done(None, None, d).points, 2);
        assert_eq!(done(Some(Priority::Low), None, d).points, 1);
        assert_eq!(done(None, None, d).reason, Reason::Done);
    }

    #[test]
    fn on_time_adds_one_including_on_the_day() {
        let early = done(Some(Priority::High), Some(DUE), "2026-11-10");
        assert_eq!(
            early,
            Award {
                points: 4,
                reason: Reason::DoneOnTime
            }
        );
        // On the due day itself is still on time.
        assert_eq!(done(None, Some(DUE), DUE).points, 3);
        assert_eq!(done(Some(Priority::Low), Some(DUE), DUE).points, 2);
    }

    #[test]
    fn late_takes_one_off_but_never_below_one() {
        let late = "2026-11-14";
        assert_eq!(
            done(Some(Priority::High), Some(DUE), late),
            Award {
                points: 2,
                reason: Reason::DoneLate
            }
        );
        assert_eq!(done(None, Some(DUE), late).points, 1);
        // Low is 1 already: late doesn't take it to 0.
        assert_eq!(done(Some(Priority::Low), Some(DUE), late).points, 1);
        // Across a month end and a year end, too.
        assert_eq!(
            done(None, Some("2026-12-31"), "2027-01-01").reason,
            Reason::DoneLate
        );
    }

    #[test]
    fn a_skip_costs_one_whatever_the_todo() {
        for priority in [Some(Priority::High), None, Some(Priority::Low)] {
            for due in [None, Some(date(DUE))] {
                let award = for_todo(Finish::Skipped, priority, due, date("2026-11-20"));
                assert_eq!(
                    award,
                    Award {
                        points: -1,
                        reason: Reason::Skipped
                    }
                );
            }
        }
    }

    #[test]
    fn signed_shows_the_sign() {
        assert_eq!(signed(4), "+4");
        assert_eq!(signed(-1), "−1");
        assert_eq!(signed(0), "0");
    }

    fn totals(list: &[(&str, i64)]) -> Vec<(String, i64)> {
        list.iter().map(|(n, p)| (n.to_string(), *p)).collect()
    }

    #[test]
    fn rank_orders_by_points_then_name_and_ties_share_a_place() {
        let list = totals(&[("Sara", 3), ("Bilal", 9), ("Ahmed", 9), ("Khadija", 24)]);
        let ranked: Vec<(usize, &str, i64)> = rank(&list)
            .into_iter()
            .map(|r| (r.place, r.name, r.points))
            .collect();
        assert_eq!(
            ranked,
            vec![
                (1, "Khadija", 24),
                (2, "Ahmed", 9),
                (2, "Bilal", 9),
                (4, "Sara", 3),
            ]
        );
    }

    #[test]
    fn rank_handles_nobody_and_negative_totals() {
        assert!(rank(&[]).is_empty());
        let list = totals(&[("Bilal", -1), ("Sara", 0)]);
        let names: Vec<&str> = rank(&list).iter().map(|r| r.name).collect();
        assert_eq!(names, vec!["Sara", "Bilal"]);
    }

    #[test]
    fn medals_for_the_top_three() {
        assert_eq!(medal(1), "🥇");
        assert_eq!(medal(2), "🥈");
        assert_eq!(medal(3), "🥉");
        assert_eq!(medal(4), "4.");
    }

    fn week() -> Period {
        Period {
            first: date("2026-11-02"),
            last: date("2026-11-08"),
        }
    }

    #[test]
    fn group_text_is_a_leaderboard_with_today_underneath() {
        let week_totals = totals(&[("Ahmed", 18), ("Khadija", 24), ("Bilal", -1)]);
        let today = totals(&[("Ahmed", 2), ("Khadija", 5)]);
        assert_eq!(
            group_text(week(), &week_totals, &today),
            "🏆 *Points this week* (Mon 2 Nov – Sun 8 Nov)\n\
             🥇 Khadija 24\n\
             🥈 Ahmed 18\n\
             🥉 Bilal −1\n\
             \n\
             Today: Khadija +5 · Ahmed +2"
        );
    }

    #[test]
    fn group_text_with_nothing_yet_says_how_to_earn() {
        let text = group_text(week(), &[], &[]);
        assert!(text.contains("No points yet"));
        assert!(text.contains("high 3"));
    }

    fn mine(days: &[i64], by_chat: Vec<(Option<String>, i64)>) -> Mine {
        let first = date("2026-10-31");
        Mine {
            last_7_days: days
                .iter()
                .enumerate()
                .map(|(i, p)| (first + chrono::Duration::days(i as i64), *p))
                .collect(),
            this_week: days[2..].iter().sum(),
            last_week: 22,
            by_chat,
        }
    }

    #[test]
    fn mine_text_shows_today_week_and_bars() {
        let text = mine_text(&mine(
            &[6, 0, 2, 0, 3, 10, 5],
            vec![(Some("Family".to_string()), 14), (None, 6)],
        ));
        assert!(text.starts_with("⭐ *Your points*\nToday: 5 · This week: 20 · Last week: 22"));
        // The best day is a full bar, a day with none is empty.
        assert!(text.contains("Thu 5 ■■■■■■■■■■ 10"), "{text}");
        assert!(text.contains("Sun 1 □□□□□□□□□□ 0"), "{text}");
        assert!(text.contains("This week by chat: Family 14 · Just me 6"));
        assert!(!text.contains("Tick off"));
    }

    #[test]
    fn mine_text_with_nothing_says_how_to_earn() {
        let text = mine_text(&mine(&[0; 7], vec![]));
        assert!(!text.contains("Last 7 days"));
        assert!(!text.contains("by chat"));
        assert!(text.contains("Tick off a todo"));
    }

    fn day(user_id: i64, name: &str, d: &str, points: i64) -> DayPoints {
        DayPoints {
            user_id,
            name: name.to_string(),
            day: date(d),
            points,
        }
    }

    /// Khadija 24 over two days, Ahmed 18, Bilal 9 (with a skip).
    fn family_week() -> Vec<DayPoints> {
        vec![
            day(1, "Khadija", "2026-11-03", 14),
            day(1, "Khadija", "2026-11-05", 10),
            day(2, "Ahmed", "2026-11-04", 18),
            day(3, "Bilal", "2026-11-02", 10),
            day(3, "Bilal", "2026-11-06", -1),
        ]
    }

    #[test]
    fn totals_add_up_each_persons_days() {
        assert_eq!(
            totals_by_person(&family_week()),
            vec![
                (1, "Khadija".to_string(), 24),
                (2, "Ahmed".to_string(), 18),
                (3, "Bilal".to_string(), 9),
            ]
        );
    }

    #[test]
    fn week_text_ranks_compares_and_names_the_best_day() {
        // Khadija up 6, Ahmed down 4, Bilal had nothing last week.
        let last_week = totals(&[("Khadija", 18), ("Ahmed", 22)]);
        assert_eq!(
            week_text(week(), &family_week(), &last_week).unwrap(),
            "🏆 *This week's points* (Mon 2 Nov – Sun 8 Nov)\n\
             🥇 Khadija 24 ⬆️ 6\n\
             🥈 Ahmed 18 ⬇️ 4\n\
             🥉 Bilal 9\n\
             \n\
             🌟 Best day: Ahmed, 18 on Wed 4 Nov"
        );
    }

    #[test]
    fn best_day_ties_go_to_the_earliest() {
        let days = vec![
            day(1, "Khadija", "2026-11-05", 10),
            day(2, "Ahmed", "2026-11-03", 10),
            day(3, "Bilal", "2026-11-03", 10),
        ];
        let text = week_text(week(), &days, &[]).unwrap();
        assert!(text.ends_with("Best day: Ahmed, 10 on Tue 3 Nov"), "{text}");
    }

    #[test]
    fn a_week_with_nothing_earned_says_nothing() {
        assert_eq!(week_text(week(), &[], &[]), None);
        // Only skips: still a leaderboard, but no best day.
        let skips = vec![day(3, "Bilal", "2026-11-06", -1)];
        let text = week_text(week(), &skips, &[]).unwrap();
        assert!(text.contains("🥇 Bilal −1"), "{text}");
        assert!(!text.contains("Best day"), "{text}");
    }

    #[test]
    fn own_week_text_is_just_theirs() {
        let khadija: Vec<DayPoints> = family_week()
            .into_iter()
            .filter(|d| d.user_id == 1)
            .collect();
        assert_eq!(
            own_week_text(Some("Family"), week(), &khadija, Some(18)),
            "⭐ *Your week in Family* (Mon 2 Nov – Sun 8 Nov)\n\
             24 points ⬆️ 6 on last week\n\
             🌟 Best day: Tue 3 Nov, 14"
        );
        let text = own_week_text(None, week(), &khadija, Some(24));
        assert!(text.contains("in the group"), "{text}");
        assert!(text.contains("24 points (the same as last week)"), "{text}");
        let text = own_week_text(None, week(), &khadija, None);
        assert!(text.contains("\n24 points\n"), "{text}");
    }
}

//! Your history (Stage P4, part 3): every week, month and year you've had,
//! compared with yourself rather than with each other. Private, so only
//! ever shown in a DM.
//!
//! Pure functions only, like the rest of `points`: a person's ledger comes
//! in (`Ledger`, their points per day and category from every chat), and
//! numbers or text go out. Reading it is `repo::points::by_day_and_source`.
//!
//! The rules every view shares:
//!
//! * Weeks are Monday to Sunday and months calendar months, on the
//!   person's own clock (`reports::Period`), as everywhere else.
//! * A week or month with nothing in it is a 0, not left out. But nothing
//!   before the person's first point is shown or counted: someone who
//!   started last week has one week of history, not eleven empty ones.
//! * Averages only count the weeks (or months) since that first point, and
//!   not the one in progress. With none to count there's no average yet.
//! * Points are net, like the week's points: a skip's −1 counts here.

use std::collections::BTreeMap;

use chrono::{Datelike, Months, NaiveDate};

use crate::points::levels::{Category, MEDAL, PARTY, Stats, Track};
use crate::points::{DOWN, RULES, SPARKLE, Source, UP, total};
use crate::progress;
use crate::reports::{Period, Span};

const GRAPH: &str = "📈";
const CALENDAR: &str = "🗓️";
const LOCK: &str = "🔒";
/// Marks the week or month still in progress.
const NOW: &str = "◀️";

/// How many weeks `/points weeks` shows, and months `/points months`.
pub const SHOWN: usize = 12;
/// A category within this many percent of its average is "flat": a
/// point or two either way isn't a trend.
const FLAT_PERCENT: i64 = 5;

/// The reply to a history command in a group.
pub fn private_text() -> String {
    format!(
        "{LOCK} Your history is just for you, so I only show it in your chat with me. \
         Message me directly and send `/points weeks`, `/points months` or `/points year`."
    )
}

/// One person's points on each day, per category, from every chat.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ledger {
    days: BTreeMap<NaiveDate, BTreeMap<Category, i64>>,
}

impl Ledger {
    /// From `(day, source, points)` rows. A day whose points net to 0 (a
    /// done and a skip) still counts as a day with history.
    pub fn from_rows(rows: impl IntoIterator<Item = (NaiveDate, Source, i64)>) -> Ledger {
        let mut days: BTreeMap<NaiveDate, BTreeMap<Category, i64>> = BTreeMap::new();
        for (day, source, points) in rows {
            *days
                .entry(day)
                .or_default()
                .entry(Category::of(source))
                .or_default() += points;
        }
        Ledger { days }
    }

    /// The first day with anything in it: where the history starts.
    pub fn first_day(&self) -> Option<NaiveDate> {
        self.days.keys().next().copied()
    }

    fn days_in(
        &self,
        period: Period,
    ) -> impl Iterator<Item = (NaiveDate, &BTreeMap<Category, i64>)> {
        self.days
            .range(period.first..=period.last)
            .map(|(day, points)| (*day, points))
    }

    /// Whether anything was earned or lost in `period`.
    pub fn any_in(&self, period: Period) -> bool {
        self.days_in(period).next().is_some()
    }

    /// The points in `period`: all of them, or one category's.
    pub fn points(&self, period: Period, category: Option<Category>) -> i64 {
        self.days_in(period)
            .flat_map(|(_, points)| points.iter())
            .filter(|(c, _)| category.is_none_or(|wanted| **c == wanted))
            .map(|(_, p)| p)
            .sum()
    }

    /// Every category's points in `period`, 0 for one with none.
    pub fn by_category(&self, period: Period) -> BTreeMap<Category, i64> {
        Category::ALL
            .into_iter()
            .map(|c| (c, self.points(period, Some(c))))
            .collect()
    }

    /// The day in `period` with the most points (above 0); the earliest on
    /// a tie.
    pub fn best_day(&self, period: Period) -> Option<(NaiveDate, i64)> {
        best_of(
            self.days_in(period)
                .map(|(day, points)| (day, points.values().sum())),
        )
    }
}

/// The highest of `(key, points)` above 0, the first on a tie (the input
/// is oldest first, so the earliest).
fn best_of<K>(items: impl IntoIterator<Item = (K, i64)>) -> Option<(K, i64)> {
    let mut best: Option<(K, i64)> = None;
    for (key, points) in items {
        if points > 0 && best.as_ref().is_none_or(|(_, top)| points > *top) {
            best = Some((key, points));
        }
    }
    best
}

/// The `count` weeks or months up to and including the one `today` is in,
/// oldest first.
pub fn last(span: Span, today: NaiveDate, count: usize) -> Vec<Period> {
    let mut periods = vec![Period::of(span, today)];
    while periods.len() < count {
        let Some(day_before) = periods[periods.len() - 1].first.pred_opt() else {
            break;
        };
        periods.push(Period::of(span, day_before));
    }
    periods.reverse();
    periods
}

/// Every week or month that overlaps `range`, cut to fit inside it, oldest
/// first: a year's weeks stop at 31 December, so the year's numbers add
/// up to the year.
pub fn within(span: Span, range: Period) -> Vec<Period> {
    let mut periods = Vec::new();
    let mut day = range.first;
    while day <= range.last {
        let whole = Period::of(span, day);
        periods.push(Period {
            first: whole.first.max(range.first),
            last: whole.last.min(range.last),
        });
        match whole.last.succ_opt() {
            Some(next) => day = next,
            None => break,
        }
    }
    periods
}

/// The whole of `year`, 1 January to 31 December.
pub fn year(year: i32) -> Option<Period> {
    Some(Period {
        first: NaiveDate::from_ymd_opt(year, 1, 1)?,
        last: NaiveDate::from_ymd_opt(year, 12, 31)?,
    })
}

/// The same month a year earlier.
pub fn a_year_before(month: Period) -> Option<Period> {
    let first = month.first.checked_sub_months(Months::new(12))?;
    Some(Period::of(Span::Month, first))
}

/// The average points over `periods` (all, or one category's), counting
/// only those that end on or after the person's first point. `None` with
/// none to count. Rounded to the nearest point.
pub fn average(ledger: &Ledger, periods: &[Period], category: Option<Category>) -> Option<i64> {
    let first = ledger.first_day()?;
    let counted: Vec<i64> = periods
        .iter()
        .filter(|p| p.last >= first)
        .map(|p| ledger.points(*p, category))
        .collect();
    if counted.is_empty() {
        return None;
    }
    let (sum, n) = (counted.iter().sum::<i64>(), counted.len() as i64);
    // Round half away from zero, without floats: (2·sum ± n) / 2n.
    Some((2 * sum + sum.signum() * n) / (2 * n))
}

/// The best of `periods` (most points, above 0); the earliest on a tie.
pub fn best(ledger: &Ledger, periods: &[Period]) -> Option<(Period, i64)> {
    best_of(periods.iter().map(|p| (*p, ledger.points(*p, None))))
}

/// The week or month `today` is in, with its points, if it beats every one
/// before it: a personal best. `None` in the person's first week or month
/// (there's nothing to beat), or when nothing before it earned anything.
pub fn new_best(ledger: &Ledger, span: Span, today: NaiveDate) -> Option<(Period, i64)> {
    let first = ledger.first_day()?;
    let current = Period::of(span, today);
    if first >= current.first {
        return None;
    }
    let earlier = Period {
        first,
        last: current.first.pred_opt()?,
    };
    let record = within(span, earlier)
        .into_iter()
        .map(|p| ledger.points(p, None))
        .max()?;
    let now = ledger.points(current, None);
    (record > 0 && now > record).then_some((current, now))
}

/// The line that announces a personal best. In a DM, with the points; in
/// a group, by name and without them, since the number comes from every
/// chat and the history is private.
pub fn best_text(span: Span, points: i64, who: Option<&str>) -> String {
    let what = span.as_db_str();
    match who {
        None => format!("{SPARKLE} Your best {what} ever ({points})"),
        Some(name) => format!("{SPARKLE} {name}: best {what} ever!"),
    }
}

/// "up 20%", "down 15%" or "flat": `now` against `before`. With nothing
/// before to take a percentage of, just "up" or "down".
pub fn trend(now: i64, before: i64) -> String {
    if before <= 0 {
        return match now.cmp(&before) {
            std::cmp::Ordering::Greater => "up".to_string(),
            std::cmp::Ordering::Less => "down".to_string(),
            std::cmp::Ordering::Equal => "flat".to_string(),
        };
    }
    let diff = now - before;
    // Rounded to the nearest percent, half away from zero.
    let percent = (200 * diff + diff.signum() * before) / (2 * before);
    match percent {
        p if p.abs() < FLAT_PERCENT => "flat".to_string(),
        p if p > 0 => format!("up {p}%"),
        p => format!("down {}%", p.abs()),
    }
}

/// " ⬆️" / " ⬇️" for `now` against `before`, or nothing if they're equal.
fn arrow(now: i64, before: i64) -> String {
    match now.cmp(&before) {
        std::cmp::Ordering::Greater => format!(" {UP}"),
        std::cmp::Ordering::Less => format!(" {DOWN}"),
        std::cmp::Ordering::Equal => String::new(),
    }
}

/// "🕌 Prayers 18, up 20% · 📝 Todos 13, flat": each category in `current`
/// against its average over `before`. `None` without an average yet.
fn categories_line(ledger: &Ledger, current: Period, before: &[Period]) -> Option<String> {
    let parts: Vec<String> = Category::ALL
        .into_iter()
        .filter_map(|c| {
            let average = average(ledger, before, Some(c))?;
            let now = ledger.points(current, Some(c));
            (now != 0 || average != 0).then(|| {
                format!(
                    "{} {} {}, {}",
                    c.emoji(),
                    c.name(),
                    total(now),
                    trend(now, average)
                )
            })
        })
        .collect();
    (!parts.is_empty()).then(|| format!("Against your average: {}", parts.join(" · ")))
}

/// " (🕌 18 · 📝 13)", when more than one category has points in `period`.
pub(crate) fn split(ledger: &Ledger, period: Period) -> String {
    let parts: Vec<String> = ledger
        .by_category(period)
        .into_iter()
        .filter(|(_, p)| *p != 0)
        .map(|(c, p)| format!("{} {}", c.emoji(), total(p)))
        .collect();
    if parts.len() > 1 {
        format!(" ({})", parts.join(" · "))
    } else {
        String::new()
    }
}

/// "w/c 5 Oct": a week by its Monday, even when it was cut short at the
/// start of a year. With the year when it isn't `today`'s.
pub(crate) fn week_label(week: Period, today: NaiveDate) -> String {
    let monday = Period::of(Span::Week, week.last).first;
    if monday.year() == today.year() {
        format!("w/c {}", monday.format("%-d %b"))
    } else {
        format!("w/c {}", monday.format("%-d %b %Y"))
    }
}

/// One bar chart row: "5 Oct ■■■■□□□□□□ 22", with ◀️ on the one in
/// progress. No bar at all when nothing anywhere is above 0.
fn bar_row(label: &str, points: i64, top: i64, current: bool) -> String {
    let mut row = label.to_string();
    let bar = progress::bar(points, top);
    if !bar.is_empty() {
        row.push(' ');
        row.push_str(&bar);
    }
    row.push(' ');
    row.push_str(&total(points));
    if current {
        row.push(' ');
        row.push_str(NOW);
    }
    row
}

/// What each history view says when there's no history at all.
fn nothing_yet() -> String {
    format!(
        "{GRAPH} No points yet, so no history. Tick off a todo or a prayer to earn some: {RULES}."
    )
}

/// `/points weeks`: the last 12 weeks as bars (from the first point on),
/// this week marked, then this week against the average of the weeks
/// before it, the best week ever, and each category against its average.
pub fn weeks_text(ledger: &Ledger, today: NaiveDate) -> String {
    periods_text(ledger, today, Span::Week)
}

/// `/points months`: the same for the last 12 months, each next to the
/// same month a year before, once there's history that far back.
pub fn months_text(ledger: &Ledger, today: NaiveDate) -> String {
    periods_text(ledger, today, Span::Month)
}

fn periods_text(ledger: &Ledger, today: NaiveDate, span: Span) -> String {
    let Some(first) = ledger.first_day() else {
        return nothing_yet();
    };
    let current = Period::of(span, today);
    let shown: Vec<Period> = last(span, today, SHOWN)
        .into_iter()
        .filter(|p| p.last >= first)
        .collect();
    if shown.is_empty() {
        return nothing_yet();
    }
    let what = span.as_db_str();
    let mut lines = vec![match shown.len() {
        1 => format!("{GRAPH} *Your first {what}*"),
        n => format!("{GRAPH} *Your last {n} {what}s*"),
    }];

    let top = shown
        .iter()
        .map(|p| ledger.points(*p, None))
        .max()
        .unwrap_or(0);
    for p in &shown {
        let points = ledger.points(*p, None);
        let label = match span {
            Span::Month => p.first.format("%b %Y").to_string(),
            _ => p.first.format("%-d %b").to_string(),
        };
        let mut row = bar_row(&label, points, top, *p == current);
        if span == Span::Month
            && let Some(ago) = a_year_before(*p)
            && ago.last >= first
        {
            row.push_str(&format!(
                " · a year before {}",
                total(ledger.points(ago, None))
            ));
        }
        lines.push(row);
    }

    let before: Vec<Period> = shown.iter().copied().filter(|p| *p != current).collect();
    let now = ledger.points(current, None);
    let this = match span {
        Span::Month => "This month",
        _ => "This week",
    };
    let mut summary = format!("{this} {}", total(now));
    match average(ledger, &before, None) {
        Some(average) => {
            summary.push_str(&format!(
                " · average {}{}",
                total(average),
                arrow(now, average)
            ));
        }
        None => summary.push_str(&format!(" · no average until you've had a whole {what}")),
    }
    let all_time = Period {
        first,
        last: current.last,
    };
    if let Some((p, points)) = best(ledger, &within(span, all_time)) {
        let label = match span {
            Span::Month => p.first.format("%b %Y").to_string(),
            _ => week_label(p, today),
        };
        summary.push_str(&format!(" · best {points} ({label})"));
    }
    lines.push(String::new());
    lines.push(summary);
    lines.extend(categories_line(ledger, current, &before));
    lines.join("\n")
}

/// Someone's lifetime stats at the start and at the end of a month or a
/// year: what the levels and XP they gained in it are worked out from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub before: Stats,
    pub after: Stats,
}

impl Progress {
    /// Nothing gained: the same stats at both ends.
    pub fn none() -> Progress {
        let nothing = Stats::from_sources([]);
        Progress {
            before: nothing.clone(),
            after: nothing,
        }
    }

    /// The levels gained, as `levels_gained`.
    pub fn gained(&self) -> Vec<(Track, i64, i64)> {
        levels_gained(&self.before, &self.after)
    }

    /// The XP earned in each category: only what was earned, as XP always
    /// is, so a skip doesn't take any away.
    pub fn xp(&self) -> BTreeMap<Category, i64> {
        Category::ALL
            .into_iter()
            .map(|c| (c, self.after.xp(c) - self.before.xp(c)))
            .collect()
    }
}

/// "XP earned: 🕌 +900 · 📝 +360", or `None` if none was.
pub fn xp_text(progress: &Progress) -> Option<String> {
    let parts: Vec<String> = progress
        .xp()
        .into_iter()
        .filter(|(_, xp)| *xp > 0)
        .map(|(c, xp)| format!("{} +{xp}", c.emoji()))
        .collect();
    (!parts.is_empty()).then(|| format!("XP earned: {}", parts.join(" · ")))
}

/// The levels someone moved up between two snapshots of their stats, as
/// `(track, from, to)`: each category, then Overall.
pub fn levels_gained(before: &Stats, after: &Stats) -> Vec<(Track, i64, i64)> {
    Track::all()
        .filter_map(|track| {
            let (from, to) = (before.level_on(track), after.level_on(track));
            (to > from).then_some((track, from, to))
        })
        .collect()
}

/// "🎉 Levels gained: 🕌 Prayers 4 → 7 · 🏅 Overall 6 → 9", or `None`
/// if there were none.
pub fn levels_text(gained: &[(Track, i64, i64)]) -> Option<String> {
    if gained.is_empty() {
        return None;
    }
    let parts: Vec<String> = gained
        .iter()
        .map(|(track, from, to)| {
            let name = match track {
                Track::Category(c) => format!("{} {}", c.emoji(), c.name()),
                Track::Overall => format!("{MEDAL} Overall"),
            };
            format!("{name} {from} → {to}")
        })
        .collect();
    Some(format!("{PARTY} Levels gained: {}", parts.join(" · ")))
}

/// `/points year` (or `/points 2025`): the year's total, against the year
/// before, the best week, month and day, the levels gained (`gained`, worked
/// out by the caller from the XP at each end), and each month as a bar.
/// A year still in progress is "so far", up to `today`.
pub fn year_text(ledger: &Ledger, which: i32, today: NaiveDate, progress: &Progress) -> String {
    let Some(whole) = year(which) else {
        return format!("{CALENDAR} I can't show the year {which}.");
    };
    if today < whole.first {
        return format!("{CALENDAR} {which} hasn't started yet.");
    }
    if !ledger.any_in(whole) {
        return match ledger.first_day() {
            None => nothing_yet(),
            Some(_) => format!("{CALENDAR} Nothing earned in {which}."),
        };
    }
    let so_far = today <= whole.last;
    let shown = Period {
        first: whole.first,
        last: whole.last.min(today),
    };
    let now = ledger.points(shown, None);
    let mut lines = vec![
        if so_far {
            format!("{CALENDAR} *Your {which} so far*")
        } else {
            format!("{CALENDAR} *Your {which}*")
        },
        format!("{} points{}", total(now), split(ledger, shown)),
    ];
    if let Some(before) = year(which - 1)
        && ledger.first_day().is_some_and(|first| first <= before.last)
    {
        let then = ledger.points(before, None);
        lines.push(format!(
            "Against {}: {}, {}",
            which - 1,
            total(then),
            trend(now, then)
        ));
    }
    if let Some((week, points)) = best(ledger, &within(Span::Week, shown)) {
        lines.push(format!(
            "{SPARKLE} Best week: {points}, {}",
            week_label(week, week.last)
        ));
    }
    let months = within(Span::Month, shown);
    if let Some((month, points)) = best(ledger, &months) {
        lines.push(format!(
            "{SPARKLE} Best month: {points}, {}",
            month.first.format("%B")
        ));
    }
    if let Some((day, points)) = ledger.best_day(shown) {
        lines.push(format!(
            "{SPARKLE} Best day: {points}, {}",
            day.format("%a %-d %b")
        ));
    }
    lines.extend(levels_text(&progress.gained()));
    lines.extend(xp_text(progress));

    lines.push(String::new());
    lines.push("*Each month*".to_string());
    let top = months
        .iter()
        .map(|m| ledger.points(*m, None))
        .max()
        .unwrap_or(0);
    let current = Period::of(Span::Month, today);
    for month in &months {
        let label = month.first.format("%b").to_string();
        let in_progress = so_far && month.first == current.first;
        lines.push(bar_row(
            &label,
            ledger.points(*month, None),
            top,
            in_progress,
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::date;

    fn period(first: &str, last: &str) -> Period {
        Period {
            first: date(first),
            last: date(last),
        }
    }

    /// A ledger of todo points: `(day, points)`.
    fn todos(days: &[(&str, i64)]) -> Ledger {
        Ledger::from_rows(days.iter().map(|(d, p)| (date(d), Source::Todo, *p)))
    }

    #[test]
    fn the_ledger_adds_up_days_and_categories() {
        let ledger = Ledger::from_rows([
            (date("2026-11-02"), Source::Prayer, 4),
            (date("2026-11-02"), Source::Todo, 3),
            // Overdue charges count under todos.
            (date("2026-11-02"), Source::Overdue, -1),
            (date("2026-11-04"), Source::Todo, 2),
        ]);
        let week = period("2026-11-02", "2026-11-08");
        assert_eq!(ledger.first_day(), Some(date("2026-11-02")));
        assert_eq!(ledger.points(week, None), 8);
        assert_eq!(ledger.points(week, Some(Category::Todos)), 4);
        assert_eq!(ledger.points(week, Some(Category::Prayers)), 4);
        assert_eq!(
            ledger.best_day(week),
            Some((date("2026-11-02"), 6)),
            "net of the charge"
        );
        assert!(!ledger.any_in(period("2026-11-09", "2026-11-15")));
        assert_eq!(Ledger::default().first_day(), None);
    }

    #[test]
    fn a_day_that_nets_to_zero_still_starts_the_history() {
        let ledger = todos(&[("2026-11-03", 0)]);
        assert_eq!(ledger.first_day(), Some(date("2026-11-03")));
        assert_eq!(ledger.best_day(period("2026-11-02", "2026-11-08")), None);
    }

    #[test]
    fn the_last_weeks_and_months_run_oldest_first_across_a_year_end() {
        let weeks = last(Span::Week, date("2027-01-06"), 3);
        assert_eq!(
            weeks,
            [
                period("2026-12-21", "2026-12-27"),
                period("2026-12-28", "2027-01-03"),
                period("2027-01-04", "2027-01-10"),
            ]
        );
        let months = last(Span::Month, date("2027-02-28"), 3);
        assert_eq!(
            months,
            [
                period("2026-12-01", "2026-12-31"),
                period("2027-01-01", "2027-01-31"),
                period("2027-02-01", "2027-02-28"),
            ]
        );
        assert_eq!(last(Span::Week, date("2026-11-13"), SHOWN).len(), SHOWN);
    }

    #[test]
    fn within_cuts_weeks_at_a_month_and_a_year_end() {
        // 2026 ends on a Thursday: its last week is cut to Mon–Thu, and
        // 2027's first is Fri 1 to Sun 3 January.
        let weeks = within(Span::Week, year(2026).unwrap());
        assert_eq!(weeks.len(), 53);
        assert_eq!(weeks[0], period("2026-01-01", "2026-01-04"));
        assert_eq!(weeks[52], period("2026-12-28", "2026-12-31"));
        let next = within(Span::Week, year(2027).unwrap());
        assert_eq!(next[0], period("2027-01-01", "2027-01-03"));

        // February in a leap year: its weeks stop on the 29th.
        let feb = within(Span::Week, period("2028-02-01", "2028-02-29"));
        assert_eq!(feb[0], period("2028-02-01", "2028-02-06"));
        assert_eq!(feb[feb.len() - 1], period("2028-02-28", "2028-02-29"));

        let months = within(Span::Month, year(2026).unwrap());
        assert_eq!(months.len(), 12);
        assert_eq!(months[11], period("2026-12-01", "2026-12-31"));
        // Part of a month: cut to fit.
        let part = within(Span::Month, period("2026-10-20", "2026-11-13"));
        assert_eq!(
            part,
            [
                period("2026-10-20", "2026-10-31"),
                period("2026-11-01", "2026-11-13")
            ]
        );
    }

    #[test]
    fn a_year_before_is_the_same_month_even_from_a_leap_day() {
        let feb = Period::of(Span::Month, date("2028-02-29"));
        assert_eq!(a_year_before(feb), Some(period("2027-02-01", "2027-02-28")));
    }

    #[test]
    fn a_week_that_crosses_a_month_end_counts_whole_in_the_weeks() {
        // Mon 26 Oct to Sun 1 Nov is one week, across the month end.
        let ledger = todos(&[("2026-10-28", 3), ("2026-11-01", 2), ("2026-11-03", 4)]);
        let week = Period::of(Span::Week, date("2026-11-01"));
        assert_eq!(ledger.points(week, None), 5);
        let october = Period::of(Span::Month, date("2026-10-01"));
        assert_eq!(ledger.points(october, None), 3);
        let november = Period::of(Span::Month, date("2026-11-30"));
        assert_eq!(ledger.points(november, None), 6);
    }

    #[test]
    fn averages_only_count_since_the_first_point() {
        // Started on Wed 4 Nov; the weeks before don't drag it down.
        let ledger = todos(&[("2026-11-04", 10), ("2026-11-10", 20)]);
        let weeks = last(Span::Week, date("2026-11-16"), 4);
        let before = &weeks[..3];
        // Weeks of 2 Nov (10) and 9 Nov (20); 26 Oct is before the start.
        assert_eq!(average(&ledger, before, None), Some(15));
        assert_eq!(average(&ledger, before, Some(Category::Prayers)), Some(0));
        // Rounded to the nearest, halves away from zero.
        let odd = todos(&[("2026-11-02", 1), ("2026-11-09", 2)]);
        assert_eq!(average(&odd, &weeks[1..3], None), Some(2));
        let negative = todos(&[("2026-11-02", -1), ("2026-11-09", -2)]);
        assert_eq!(average(&negative, &weeks[1..3], None), Some(-2));
    }

    #[test]
    fn with_less_than_a_week_there_is_no_average() {
        let ledger = todos(&[("2026-11-11", 4)]);
        let weeks = last(Span::Week, date("2026-11-13"), SHOWN);
        let before = &weeks[..SHOWN - 1];
        assert_eq!(average(&ledger, before, None), None);
        assert_eq!(average(&Ledger::default(), &weeks, None), None);
    }

    #[test]
    fn the_best_is_the_highest_above_zero_and_the_earliest_on_a_tie() {
        let ledger = todos(&[("2026-10-05", 5), ("2026-10-12", 8), ("2026-10-19", 8)]);
        let weeks = within(Span::Week, period("2026-10-05", "2026-10-25"));
        assert_eq!(
            best(&ledger, &weeks),
            Some((period("2026-10-12", "2026-10-18"), 8))
        );
        let skips = todos(&[("2026-10-05", -1)]);
        assert_eq!(best(&skips, &weeks), None);
    }

    #[test]
    fn a_new_best_needs_something_to_beat() {
        // First week: nothing to beat, however good.
        let first = todos(&[("2026-11-10", 30)]);
        assert_eq!(new_best(&first, Span::Week, date("2026-11-13")), None);

        // Last week 10, this week 12: a best. 10 isn't.
        let ledger = todos(&[("2026-11-03", 10), ("2026-11-10", 12)]);
        assert_eq!(
            new_best(&ledger, Span::Week, date("2026-11-13")),
            Some((period("2026-11-09", "2026-11-15"), 12))
        );
        let level = todos(&[("2026-11-03", 10), ("2026-11-10", 10)]);
        assert_eq!(new_best(&level, Span::Week, date("2026-11-13")), None);

        // Only skips before: nothing worth beating.
        let skips = todos(&[("2026-11-03", -1), ("2026-11-10", 3)]);
        assert_eq!(new_best(&skips, Span::Week, date("2026-11-13")), None);
    }

    #[test]
    fn a_new_best_month_across_a_year_end() {
        let ledger = todos(&[("2026-11-20", 40), ("2026-12-15", 20), ("2027-01-02", 41)]);
        assert_eq!(
            new_best(&ledger, Span::Month, date("2027-01-03")),
            Some((period("2027-01-01", "2027-01-31"), 41))
        );
    }

    #[test]
    fn best_text_keeps_the_number_out_of_a_group() {
        assert_eq!(
            best_text(Span::Week, 45, None),
            "🌟 Your best week ever (45)"
        );
        assert_eq!(
            best_text(Span::Month, 120, Some("Sara")),
            "🌟 Sara: best month ever!"
        );
    }

    #[test]
    fn trend_rounds_and_calls_small_changes_flat() {
        assert_eq!(trend(12, 10), "up 20%");
        assert_eq!(trend(17, 20), "down 15%");
        assert_eq!(trend(104, 100), "flat");
        assert_eq!(trend(96, 100), "flat");
        assert_eq!(trend(105, 100), "up 5%");
        assert_eq!(trend(3, 0), "up");
        assert_eq!(trend(-1, 0), "down");
        assert_eq!(trend(0, 0), "flat");
    }

    #[test]
    fn weeks_text_matches_the_roadmap_example() {
        // Best ever 44 in the week of 5 Oct; then 26 a week; this week 31.
        let mut days = vec![("2026-10-05", 44)];
        let weeks = [
            "2026-10-12",
            "2026-10-19",
            "2026-10-26",
            "2026-11-02",
            "2026-11-09",
        ];
        days.extend(weeks.iter().map(|d| (*d, 26)));
        days.push(("2026-11-16", 31));
        let ledger = todos(&days);
        let text = weeks_text(&ledger, date("2026-11-18"));
        assert!(
            text.starts_with("📈 *Your last 7 weeks*\n5 Oct ■■■■■■■■■■ 44\n"),
            "{text}"
        );
        assert!(text.contains("\n16 Nov ■■■■■■■□□□ 31 ◀️\n"), "{text}");
        // The average of the six weeks before this one: (44 + 5×26) / 6 = 29.
        assert!(
            text.contains("This week 31 · average 29 ⬆️ · best 44 (w/c 5 Oct)"),
            "{text}"
        );
        assert!(
            text.ends_with("Against your average: 📝 Todos 31, up 7%"),
            "{text}"
        );
    }

    #[test]
    fn weeks_text_shows_twelve_with_empty_weeks_as_zero() {
        let ledger = todos(&[("2026-01-05", 5), ("2026-11-10", 3)]);
        let text = weeks_text(&ledger, date("2026-11-13"));
        assert!(text.starts_with("📈 *Your last 12 weeks*"), "{text}");
        assert_eq!(
            text.lines().filter(|l| l.ends_with(" 0")).count(),
            11,
            "{text}"
        );
        // The best week was long before the 12 shown.
        assert!(text.contains("best 5 (w/c 5 Jan)"), "{text}");
    }

    #[test]
    fn weeks_text_in_the_first_week() {
        let ledger = todos(&[("2026-11-11", 4)]);
        let text = weeks_text(&ledger, date("2026-11-13"));
        assert!(
            text.starts_with("📈 *Your first week*\n9 Nov ■■■■■■■■■■ 4 ◀️"),
            "{text}"
        );
        assert!(
            text.contains(
                "This week 4 · no average until you've had a whole week · best 4 (w/c 9 Nov)"
            ),
            "{text}"
        );
        assert!(!text.contains("Against your average"), "{text}");
    }

    #[test]
    fn history_with_nothing_says_how_to_earn() {
        let empty = Ledger::default();
        for text in [
            weeks_text(&empty, date("2026-11-13")),
            months_text(&empty, date("2026-11-13")),
            year_text(&empty, 2026, date("2026-11-13"), &Progress::none()),
        ] {
            assert!(text.starts_with("📈 No points yet"), "{text}");
        }
    }

    #[test]
    fn months_text_compares_with_a_year_before_once_there_is_one() {
        let ledger = Ledger::from_rows([
            (date("2025-11-10"), Source::Todo, 20),
            (date("2026-10-31"), Source::Prayer, 30),
            (date("2026-11-01"), Source::Prayer, 10),
            (date("2026-11-02"), Source::Todo, 15),
        ]);
        let text = months_text(&ledger, date("2026-11-13"));
        assert!(
            text.starts_with("📈 *Your last 12 months*\nDec 2025"),
            "{text}"
        );
        // November 2025 is 13 months back, so not a row of its own, but it
        // is November 2026's year before. October 2025 was before the first
        // point, so October 2026 has none.
        assert!(
            text.contains("\nDec 2025 0\n") || text.contains("\nDec 2025 □"),
            "{text}"
        );
        assert!(
            text.contains("Nov 2026 ■■■■■■■■□□ 25 ◀️ · a year before 20"),
            "{text}"
        );
        assert!(
            text.contains(
                "
Oct 2026 ■■■■■■■■■■ 30
"
            ),
            "{text}"
        );
        // The 11 months before this one, all since the first point: 30/11.
        assert!(
            text.contains("This month 25 · average 3 ⬆️ · best 30 (Oct 2026)"),
            "{text}"
        );
    }

    #[test]
    fn year_text_sums_the_year_and_its_best_week_and_month() {
        let ledger = Ledger::from_rows([
            (date("2025-06-01"), Source::Todo, 50),
            // The week of Mon 29 Dec 2025 is cut at the year: only Thu 1 and
            // Sat 3 January count in 2026.
            (date("2025-12-30"), Source::Todo, 100),
            (date("2026-01-01"), Source::Todo, 5),
            (date("2026-01-03"), Source::Prayer, 6),
            (date("2026-03-10"), Source::Prayer, 8),
            (date("2026-12-31"), Source::Todo, 2),
        ]);
        // 30 prayer XP (Lv 1) at the start of the year, 40 (Lv 2) at the end.
        let progress = Progress {
            before: Stats::from_sources([(Source::Prayer, 30)]),
            after: Stats::from_sources([(Source::Prayer, 40)]),
        };
        let text = year_text(&ledger, 2026, date("2027-01-01"), &progress);
        let expected = "🗓️ *Your 2026*\n\
                        21 points (🕌 14 · 📝 7)\n\
                        Against 2025: 150, down 86%\n\
                        🌟 Best week: 11, w/c 29 Dec 2025\n\
                        🌟 Best month: 11, January\n\
                        🌟 Best day: 8, Tue 10 Mar\n\
                        🎉 Levels gained: 🕌 Prayers 1 → 2 · 🏅 Overall 2 → 3\n\
                        XP earned: 🕌 +10\n\
                        \n\
                        *Each month*\n";
        assert!(text.starts_with(expected), "{text}");
        assert!(text.contains("\nMar ■■■■■■■□□□ 8\n"), "{text}");
        assert!(text.ends_with("\nDec ■■□□□□□□□□ 2"), "{text}");
        assert_eq!(
            text.lines()
                .filter(|l| l.contains("■") || l.ends_with(" 0"))
                .count(),
            12
        );
    }

    #[test]
    fn year_text_so_far_stops_at_today() {
        let ledger = todos(&[("2026-02-10", 4), ("2026-11-12", 6)]);
        let text = year_text(&ledger, 2026, date("2026-11-13"), &Progress::none());
        assert!(
            text.starts_with("🗓️ *Your 2026 so far*\n10 points\n🌟"),
            "{text}"
        );
        assert!(text.ends_with("\nNov ■■■■■■■■■■ 6 ◀️"), "{text}");
        assert!(!text.contains("Against 2025"), "no history then: {text}");
        assert!(!text.contains("Levels gained"), "{text}");
    }

    #[test]
    fn year_text_for_a_year_with_nothing_or_still_to_come() {
        let ledger = todos(&[("2026-02-10", 4)]);
        assert_eq!(
            year_text(&ledger, 2025, date("2026-11-13"), &Progress::none()),
            "🗓️ Nothing earned in 2025."
        );
        assert_eq!(
            year_text(&ledger, 2030, date("2026-11-13"), &Progress::none()),
            "🗓️ 2030 hasn't started yet."
        );
    }

    #[test]
    fn levels_gained_lists_only_the_tracks_that_rose() {
        let before = Stats::from_sources([(Source::Prayer, 30), (Source::Todo, 20)]);
        let after = Stats::from_sources([(Source::Prayer, 40), (Source::Todo, 20)]);
        assert_eq!(
            levels_gained(&before, &after),
            [
                (Track::Category(Category::Prayers), 1, 2),
                (Track::Overall, 3, 4)
            ]
        );
        assert_eq!(levels_gained(&after, &after), []);
        assert_eq!(levels_text(&[]), None);
    }
}

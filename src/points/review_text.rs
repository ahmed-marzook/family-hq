//! What the monthly and yearly points reviews say (Stage P4, part 4). Pure
//! functions only: a person's ledger (`history::Ledger`), the stats at
//! each end of the period (`history::Progress`) and their prayer record
//! come in, and the text goes out. When the reviews go, and to whom, is
//! `points::reviews`.
//!
//! A review compares the person with themselves, never with anyone else:
//! the month before, the same month a year before, and the levels and XP
//! they gained. It's private, so it only ever goes to their own DM.

use chrono::{Datelike, NaiveDate};

use crate::points::history::{
    Ledger, Progress, a_year_before, best, levels_text, split, trend, week_label, within, xp_text,
    year, year_text,
};
use crate::points::levels::Category;
use crate::points::{SPARKLE, total};
use crate::prayer::stats::{MADE_UP, MISSED, PRAYED, Summary};
use crate::reports::{Period, Span};

const CALENDAR: &str = "🗓️";
const FIREWORKS: &str = "🎆";

/// How someone's prayers went over a month, from the prayer log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrayerRecord {
    pub on_time: usize,
    pub made_up: usize,
    pub missed: usize,
}

impl PrayerRecord {
    /// From the month's prayer summary, or `None` if nothing was recorded
    /// (prayer reminders off, say).
    pub fn of(summary: &Summary) -> Option<PrayerRecord> {
        if summary.is_empty() {
            return None;
        }
        let made_up = summary.made_up.len();
        Some(PrayerRecord {
            // `prayed` counts made-up prayers too.
            on_time: summary.prayed - made_up,
            made_up,
            missed: summary.missed.len(),
        })
    }
}

/// "September", or "December 2025" when it's another year from `than`.
fn month_name(month: Period, than: Period) -> String {
    if month.first.year() == than.first.year() {
        month.first.format("%B").to_string()
    } else {
        month.first.format("%B %Y").to_string()
    }
}

/// The monthly review, sent on the 1st: the month's points by category,
/// against the month before and the same month a year before (each only
/// once there's history that far back), the levels and XP gained, the best
/// week and day, and the prayer record. `None` for a month with nothing
/// earned or lost, so a quiet month sends nothing.
pub fn month_text(
    ledger: &Ledger,
    month: Period,
    progress: &Progress,
    prayers: Option<PrayerRecord>,
) -> Option<String> {
    if !ledger.any_in(month) {
        return None;
    }
    let first = ledger.first_day()?;
    let now = ledger.points(month, None);
    let mut lines = vec![
        format!("{CALENDAR} *Your {}*", month.first.format("%B %Y")),
        format!("{} points{}", total(now), split(ledger, month)),
    ];

    // What it's compared with: only months since the first point.
    let before = month
        .first
        .pred_opt()
        .map(|day| Period::of(Span::Month, day));
    let compared: Vec<(String, Period)> = [before, a_year_before(month)]
        .into_iter()
        .flatten()
        .filter(|p| first <= p.last)
        .map(|p| (month_name(p, month), p))
        .collect();
    for (name, p) in &compared {
        let then = ledger.points(*p, None);
        lines.push(format!(
            "Against {name}: {}, {}",
            total(then),
            trend(now, then)
        ));
    }
    if !compared.is_empty() {
        for c in Category::ALL {
            let now = ledger.points(month, Some(c));
            let befores: Vec<i64> = compared
                .iter()
                .map(|(_, p)| ledger.points(*p, Some(c)))
                .collect();
            if now == 0 && befores.iter().all(|b| *b == 0) {
                continue;
            }
            let against: Vec<String> = compared
                .iter()
                .zip(&befores)
                .map(|((name, _), then)| format!("{} on {name}", trend(now, *then)))
                .collect();
            lines.push(format!(
                "{} {} {}: {}",
                c.emoji(),
                c.name(),
                total(now),
                against.join(", ")
            ));
        }
    }

    lines.extend(levels_text(&progress.gained()));
    lines.extend(xp_text(progress));
    if let Some((week, points)) = best(ledger, &within(Span::Week, month)) {
        lines.push(format!(
            "{SPARKLE} Best week: {points}, {}",
            week_label(week, month.last)
        ));
    }
    if let Some((day, points)) = ledger.best_day(month) {
        lines.push(format!(
            "{SPARKLE} Best day: {points}, {}",
            day.format("%a %-d %b")
        ));
    }
    if let Some(record) = prayers {
        lines.push(format!(
            "{} Prayers: {PRAYED} {} on time · {MADE_UP} {} made up · {MISSED} {} missed",
            Category::Prayers.emoji(),
            record.on_time,
            record.made_up,
            record.missed
        ));
    }
    lines.push(String::new());
    lines.push(
        "Your year so far: `/points year`. To stop these reviews, send `/points reports off`."
            .to_string(),
    );
    Some(lines.join("\n"))
}

/// The year in review, sent on 1 January: `/points year` for the whole
/// year just gone, under its own heading. `None` for a year with nothing
/// earned or lost.
pub fn year_review_text(ledger: &Ledger, which: i32, progress: &Progress) -> Option<String> {
    let whole = year(which)?;
    if !ledger.any_in(whole) {
        return None;
    }
    // The day after the year, so it's the whole year, not "so far".
    let after: NaiveDate = whole.last.succ_opt()?;
    let text = year_text(ledger, which, after, progress);
    let (_, body) = text.split_once('\n')?;
    Some(format!(
        "{FIREWORKS} *Your {which} in review*\n{body}\n\nSee it again any time with `/points {which}`."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::points::Source;
    use crate::points::levels::Stats;
    use crate::prayer::Prayer;
    use crate::prayer::stats::{Entry, Status};
    use crate::testkit::date;

    fn month(day: &str) -> Period {
        Period::of(Span::Month, date(day))
    }

    fn ledger(rows: &[(&str, Source, i64)]) -> Ledger {
        Ledger::from_rows(rows.iter().map(|(d, s, p)| (date(d), *s, *p)))
    }

    #[test]
    fn a_prayer_record_splits_on_time_from_made_up() {
        let entry = |d: &str, prayer, status| Entry {
            date: date(d),
            prayer,
            status,
        };
        let log = [
            entry("2026-10-01", Prayer::Fajr, Status::Prayed),
            entry("2026-10-01", Prayer::Dhuhr, Status::Late),
            entry("2026-10-01", Prayer::Asr, Status::Missed),
            entry("2026-10-02", Prayer::Fajr, Status::Prayed),
        ];
        let october = month("2026-10-01");
        assert_eq!(
            PrayerRecord::of(&Summary::new(october, &log)),
            Some(PrayerRecord {
                on_time: 2,
                made_up: 1,
                missed: 1
            })
        );
        assert_eq!(PrayerRecord::of(&Summary::new(october, &[])), None);
    }

    #[test]
    fn the_monthly_review_compares_with_last_month_and_last_year() {
        let ledger = ledger(&[
            ("2025-10-10", Source::Prayer, 50),
            ("2026-09-15", Source::Prayer, 60),
            ("2026-09-16", Source::Todo, 20),
            ("2026-10-05", Source::Prayer, 40),
            ("2026-10-06", Source::Prayer, 20),
            ("2026-10-13", Source::Todo, 21),
            ("2026-10-14", Source::Overdue, -1),
        ]);
        let progress = Progress {
            before: Stats::from_sources([(Source::Prayer, 300)]),
            after: Stats::from_sources([(Source::Prayer, 360), (Source::Todo, 21)]),
        };
        let record = PrayerRecord {
            on_time: 120,
            made_up: 10,
            missed: 5,
        };
        let text = month_text(&ledger, month("2026-10-31"), &progress, Some(record)).unwrap();
        let expected = "🗓️ *Your October 2026*\n\
                        80 points (🕌 60 · 📝 20)\n\
                        Against September: 80, flat\n\
                        Against October 2025: 50, up 60%\n\
                        🕌 Prayers 60: flat on September, up 20% on October 2025\n\
                        📝 Todos 20: flat on September, up on October 2025\n\
                        🎉 Levels gained: 🕌 Prayers 4 → 5 · 📝 Todos 1 → 2 · 🏅 Overall 5 → 7\n\
                        XP earned: 🕌 +60 · 📝 +21\n\
                        🌟 Best week: 60, w/c 5 Oct\n\
                        🌟 Best day: 40, Mon 5 Oct\n\
                        🕌 Prayers: ✅ 120 on time · 🕐 10 made up · ❌ 5 missed\n\
                        \n\
                        Your year so far: `/points year`. To stop these reviews, send \
                        `/points reports off`.";
        assert_eq!(text, expected);
    }

    #[test]
    fn a_first_month_has_nothing_to_compare_with() {
        let ledger = ledger(&[("2026-11-08", Source::Todo, 6)]);
        let text = month_text(&ledger, month("2026-11-30"), &Progress::none(), None).unwrap();
        assert!(
            text.starts_with("🗓️ *Your November 2026*\n6 points\n🌟 Best week: 6, w/c 2 Nov\n"),
            "{text}"
        );
        assert!(!text.contains("Against"), "{text}");
        assert!(!text.contains("Levels gained"), "{text}");
        assert!(!text.contains("Prayers:"), "no prayer record: {text}");
    }

    #[test]
    fn january_is_compared_with_last_december() {
        let ledger = ledger(&[
            ("2026-12-20", Source::Todo, 10),
            ("2027-01-04", Source::Todo, 5),
        ]);
        let text = month_text(&ledger, month("2027-01-31"), &Progress::none(), None).unwrap();
        assert!(
            text.contains("\nAgainst December 2026: 10, down 50%\n"),
            "{text}"
        );
        assert!(!text.contains("January 2026"), "no history then: {text}");
    }

    #[test]
    fn a_quiet_month_sends_nothing() {
        let ledger = ledger(&[("2026-09-15", Source::Todo, 6)]);
        assert_eq!(
            month_text(&ledger, month("2026-10-31"), &Progress::none(), None),
            None
        );
        assert_eq!(
            month_text(
                &Ledger::default(),
                month("2026-10-31"),
                &Progress::none(),
                None
            ),
            None
        );
    }

    #[test]
    fn the_year_in_review_is_the_whole_year_under_its_own_heading() {
        let ledger = ledger(&[
            ("2026-03-02", Source::Prayer, 8),
            ("2026-12-31", Source::Todo, 3),
        ]);
        let text = year_review_text(&ledger, 2026, &Progress::none()).unwrap();
        assert!(
            text.starts_with("🎆 *Your 2026 in review*\n11 points (🕌 8 · 📝 3)\n"),
            "{text}"
        );
        assert!(!text.contains("so far"), "{text}");
        assert!(!text.contains("◀️"), "nothing in progress: {text}");
        assert!(text.contains("\nDec ■■■■□□□□□□ 3\n"), "{text}");
        assert!(
            text.ends_with("\n\nSee it again any time with `/points 2026`."),
            "{text}"
        );
        assert_eq!(year_review_text(&ledger, 2025, &Progress::none()), None);
    }
}

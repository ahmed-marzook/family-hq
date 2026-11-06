//! Missed prayers, streaks, and what the prayer reports say: the day's
//! wrap-up, and the week's and the month's summaries with their heatmaps.
//! When reports are due and where they go is the shared `reports` module.
//!
//! Pure functions only, like `prayer.rs`: someone's log comes in as a
//! list, and the text goes out. Reading the log and sending the reports is
//! `prayer_tracking`'s job.

use std::str::FromStr;

use chrono::{DateTime, Datelike, NaiveDate, Utc};

use crate::prayer::{DayTimes, Prayer};
use crate::progress;
use crate::reports::{Period, Span};

/// How one prayer went, as `prayer_log.status` stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Checked in while its time lasted.
    Prayed,
    /// Checked in after the bot had marked it missed (made up, qada).
    Late,
    /// Its time ended with no check-in.
    Missed,
}

impl Status {
    /// The exact text stored in the database (and allowed by its CHECK).
    pub fn as_db_str(self) -> &'static str {
        match self {
            Status::Prayed => "prayed",
            Status::Late => "late",
            Status::Missed => "missed",
        }
    }

    /// Prayed at all, on time or made up. Completion counts these.
    pub fn is_prayed(self) -> bool {
        self != Status::Missed
    }

    /// How `/prayers` and the reports mark it.
    pub fn mark(self) -> &'static str {
        match self {
            Status::Prayed => PRAYED,
            Status::Late => MADE_UP,
            Status::Missed => MISSED,
        }
    }
}

impl FromStr for Status {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        [Status::Prayed, Status::Late, Status::Missed]
            .into_iter()
            .find(|status| status.as_db_str() == s)
            .ok_or_else(|| format!("unknown prayer status '{s}'"))
    }
}

/// One row of someone's prayer log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub date: NaiveDate,
    pub prayer: Prayer,
    pub status: Status,
}

/// The prayers of `day` whose time has ended at `now`. Any of them with no
/// check-in gets marked missed.
pub fn ended(day: &DayTimes, now: DateTime<Utc>) -> Vec<Prayer> {
    day.windows
        .iter()
        .filter(|window| window.end <= now)
        .map(|window| window.prayer)
        .collect()
}

/// When the prayer day is over, for its reports: when its last prayer,
/// Isha, ends (at Islamic midnight, often after midnight on the clock).
pub fn day_over(day: &DayTimes) -> DateTime<Utc> {
    // `windows` is in `Prayer::ALL` order, so the last one is Isha.
    day.windows[day.windows.len() - 1].end
}

/// One person's summary of a period, worked out from their log.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub period: Period,
    /// How many prayers (on time or made up) each day of the period, in
    /// order. `None` for a day with nothing in the log: before reminders
    /// started, or still to come.
    pub days: Vec<Option<usize>>,
    /// Prayed, on time or made up.
    pub prayed: usize,
    /// Everything in the log: prayed, made up or missed. Prayers with no
    /// record aren't counted either way.
    pub recorded: usize,
    /// The most on-time prayers in a row within the period. A missed or
    /// made-up prayer, or a gap in the log, ends a run.
    pub longest_streak: usize,
    /// In date order, then prayer order.
    pub missed: Vec<Entry>,
    pub made_up: Vec<Entry>,
}

impl Summary {
    /// `log` can hold entries outside the period; they're ignored.
    pub fn new(period: Period, log: &[Entry]) -> Self {
        // A function returning a closure would need a named type, so this
        // small lookup is a closure itself. It borrows `log`.
        let status = |date: NaiveDate, prayer: Prayer| {
            log.iter()
                .find(|e| e.date == date && e.prayer == prayer)
                .map(|e| e.status)
        };

        let mut summary = Summary {
            period,
            days: Vec::new(),
            prayed: 0,
            recorded: 0,
            longest_streak: 0,
            missed: Vec::new(),
            made_up: Vec::new(),
        };
        let mut run = 0;
        for date in period.days() {
            let mut day_prayed = 0;
            let mut day_recorded = 0;
            for prayer in Prayer::ALL {
                let found = status(date, prayer);
                if found == Some(Status::Prayed) {
                    run += 1;
                    summary.longest_streak = summary.longest_streak.max(run);
                } else {
                    run = 0;
                }
                let Some(found) = found else {
                    continue;
                };
                day_recorded += 1;
                if found.is_prayed() {
                    day_prayed += 1;
                }
                let entry = Entry {
                    date,
                    prayer,
                    status: found,
                };
                match found {
                    Status::Missed => summary.missed.push(entry),
                    Status::Late => summary.made_up.push(entry),
                    Status::Prayed => {}
                }
            }
            summary.prayed += day_prayed;
            summary.recorded += day_recorded;
            summary.days.push((day_recorded > 0).then_some(day_prayed));
        }
        summary
    }

    /// Nothing at all in the log for the period.
    pub fn is_empty(&self) -> bool {
        self.recorded == 0
    }
}

/// Whether `log` has all five of `date`'s prayers prayed, on time or made
/// up: what the day's wrap-up congratulates.
pub fn prayed_all(log: &[Entry], date: NaiveDate) -> bool {
    let prayed = log
        .iter()
        .filter(|e| e.date == date && e.status.is_prayed())
        .count();
    prayed == Prayer::ALL.len()
}

const PRAYED: &str = "✅";
const MADE_UP: &str = "🕐";
const MISSED: &str = "❌";
const PARTY: &str = "🎉";
const CHART: &str = "📊";
const CALENDAR: &str = "🗓️";
const FIRE: &str = "🔥";

/// Heatmap squares, by how many of the day's five were prayed.
const ALL_FIVE: &str = "🟩";
const THREE_OR_FOUR: &str = "🟨";
const ONE_OR_TWO: &str = "🟧";
const NONE_PRAYED: &str = "🟥";
const NO_RECORD: &str = "⬜";
/// Pads a month's first and last rows to whole weeks.
const OUTSIDE: &str = "▫️";

/// The heatmap square for a day (`Summary::days`).
pub fn cell(prayed: Option<usize>) -> &'static str {
    match prayed {
        None => NO_RECORD,
        Some(0) => NONE_PRAYED,
        Some(1 | 2) => ONE_OR_TWO,
        Some(3 | 4) => THREE_OR_FOUR,
        Some(_) => ALL_FIVE,
    }
}

/// The day's wrap-up. With `mentions` (phone numbers), a group's: it
/// @mentions each person who prayed all five. Without, a DM's: "you".
/// The caller only sends it when someone did.
pub fn day_text(date: NaiveDate, mentions: &[String]) -> String {
    let date = date.format("%a %-d %b");
    if mentions.is_empty() {
        format!("{PARTY} You prayed *all 5 prayers* today ({date}). Masha'Allah, keep it up!")
    } else {
        let tags: Vec<String> = mentions.iter().map(|p| format!("@{p}")).collect();
        format!(
            "{PARTY} *All 5 prayers* today ({date}): {}. Masha'Allah!",
            tags.join(", ")
        )
    }
}

/// A week's or a month's summary. Each person is `(name, summary)`; a name
/// of `None` is a DM's, about "you", with no name heading.
pub fn summary_text(span: Span, period: Period, people: &[(Option<&str>, Summary)]) -> String {
    let mut text = match span {
        Span::Week => format!(
            "{CHART} *Prayer week* · {} – {}\n",
            period.first.format("%-d %b"),
            period.last.format("%-d %b")
        ),
        Span::Month => format!(
            "{CALENDAR} *Prayer month* · {}\n",
            period.first.format("%B %Y")
        ),
    };
    for (name, summary) in people {
        text.push('\n');
        if let Some(name) = name {
            text.push_str(&format!("*{name}*\n"));
        }
        text.push_str(&heatmap(span, summary));
        text.push_str(&totals(summary));
        // A week names the days; a month's would be too long, so it counts.
        let with_days = span == Span::Week;
        if summary.missed.is_empty() {
            text.push_str(&format!("{MISSED} Missed: none {PARTY}\n"));
        } else {
            text.push_str(&format!(
                "{MISSED} Missed: {}\n",
                breakdown(&summary.missed, with_days)
            ));
        }
        if !summary.made_up.is_empty() {
            text.push_str(&format!(
                "{MADE_UP} Made up: {}\n",
                breakdown(&summary.made_up, with_days)
            ));
        }
    }
    let rows = match span {
        Span::Week => "Mon to Sun",
        Span::Month => "Each row is Mon to Sun",
    };
    text.push_str(&format!(
        "\n{rows}: {ALL_FIVE} all 5 · {THREE_OR_FOUR} 3–4 · {ONE_OR_TWO} 1–2 · \
         {NONE_PRAYED} none · {NO_RECORD} no record"
    ));
    text
}

/// A week's seven squares on one line, or a month as a calendar: one row
/// per week, Monday first, padded with `OUTSIDE`.
fn heatmap(span: Span, summary: &Summary) -> String {
    let cells: Vec<&str> = summary.days.iter().map(|d| cell(*d)).collect();
    if span == Span::Week {
        return cells.concat() + "\n";
    }
    let before = summary.period.first.weekday().num_days_from_monday() as usize;
    let mut squares = vec![OUTSIDE; before];
    squares.extend(cells);
    // Round up to whole weeks: 31 days starting on a Sunday need 6 rows.
    while !squares.len().is_multiple_of(7) {
        squares.push(OUTSIDE);
    }
    // `chunks(7)` hands out the squares a week at a time.
    squares.chunks(7).map(|week| week.concat() + "\n").collect()
}

/// "✅ 30/33 prayed · 91% · ■■■■■■■■■□\n🔥 Longest streak: 14 in a row\n".
fn totals(summary: &Summary) -> String {
    let (done, of) = (summary.prayed as i64, summary.recorded as i64);
    let streak = match summary.longest_streak {
        1 => "1 prayer".to_string(),
        n => format!("{n} in a row"),
    };
    format!(
        "{PRAYED} {done}/{of} prayed · {}% · {}\n{FIRE} Longest streak: {streak}\n",
        progress::percent(done, of),
        progress::bar(done, of)
    )
}

/// Which prayers, in prayer order: "Fajr ×2 (Tue, Sun), Isha (Sun)", or
/// without the days, "Fajr ×6, Isha".
fn breakdown(entries: &[Entry], with_days: bool) -> String {
    let mut parts = Vec::new();
    for prayer in Prayer::ALL {
        let days: Vec<NaiveDate> = entries
            .iter()
            .filter(|e| e.prayer == prayer)
            .map(|e| e.date)
            .collect();
        if days.is_empty() {
            continue;
        }
        let mut part = prayer.to_string();
        if days.len() > 1 {
            part.push_str(&format!(" ×{}", days.len()));
        }
        if with_days {
            let names: Vec<String> = days.iter().map(|d| d.format("%a").to_string()).collect();
            part.push_str(&format!(" ({})", names.join(", ")));
        }
        parts.push(part);
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prayer::Window;
    use crate::testkit::date;
    use crate::testkit::utc;

    fn entry(day: &str, prayer: Prayer, status: Status) -> Entry {
        Entry {
            date: date(day),
            prayer,
            status,
        }
    }

    /// All five of `day` with `status`.
    fn whole_day(day: &str, status: Status) -> Vec<Entry> {
        Prayer::ALL
            .into_iter()
            .map(|p| entry(day, p, status))
            .collect()
    }

    /// A day whose Isha ends at `isha_end`. The other windows are only
    /// there to fill the array.
    fn day_ending(on: &str, isha_end: &str) -> DayTimes {
        let w = |prayer| Window {
            prayer,
            start: utc("2026-10-27T05:00:00Z"),
            end: utc("2026-10-27T06:00:00Z"),
        };
        DayTimes {
            date: date(on),
            hijri: None,
            windows: [
                w(Prayer::Fajr),
                w(Prayer::Dhuhr),
                w(Prayer::Asr),
                w(Prayer::Maghrib),
                Window {
                    prayer: Prayer::Isha,
                    start: utc("2026-10-27T18:31:00Z"),
                    end: utc(isha_end),
                },
            ],
        }
    }

    #[test]
    fn statuses_round_trip() {
        for status in [Status::Prayed, Status::Late, Status::Missed] {
            assert_eq!(status.as_db_str().parse::<Status>(), Ok(status));
        }
        assert!("skipped".parse::<Status>().is_err());
    }

    #[test]
    fn a_prayer_has_ended_from_the_end_of_its_window() {
        let day = day_ending("2026-10-27", "2026-10-27T23:46:00Z");
        assert_eq!(ended(&day, utc("2026-10-27T05:59:59Z")), []);
        assert_eq!(
            ended(&day, utc("2026-10-27T06:00:00Z")),
            [Prayer::Fajr, Prayer::Dhuhr, Prayer::Asr, Prayer::Maghrib]
        );
        assert_eq!(ended(&day, utc("2026-10-27T23:46:00Z")).len(), 5);
    }

    #[test]
    fn the_prayer_day_is_over_when_isha_ends() {
        let day = day_ending("2026-10-27", "2026-10-27T23:46:00Z");
        assert_eq!(day_over(&day), utc("2026-10-27T23:46:00Z"));
    }

    #[test]
    fn summary_counts_completion_and_finds_the_misses() {
        let week = Period::of(Span::Week, date("2026-10-26"));
        let mut log = whole_day("2026-10-26", Status::Prayed);
        log.push(entry("2026-10-27", Prayer::Fajr, Status::Missed));
        log.push(entry("2026-10-27", Prayer::Dhuhr, Status::Prayed));
        log.push(entry("2026-10-27", Prayer::Asr, Status::Late));
        // Outside the week: ignored.
        log.extend(whole_day("2026-11-02", Status::Missed));

        let summary = Summary::new(week, &log);
        assert_eq!((summary.prayed, summary.recorded), (7, 8));
        assert_eq!(
            summary.days,
            [Some(5), Some(2), None, None, None, None, None]
        );
        assert_eq!(
            summary.missed,
            [entry("2026-10-27", Prayer::Fajr, Status::Missed)]
        );
        assert_eq!(
            summary.made_up,
            [entry("2026-10-27", Prayer::Asr, Status::Late)]
        );
        assert!(!summary.is_empty());
        assert!(Summary::new(week, &[]).is_empty());
    }

    #[test]
    fn a_streak_runs_across_days_and_a_miss_or_a_make_up_ends_it() {
        let week = Period::of(Span::Week, date("2026-10-26"));
        // Mon: all 5. Tue: Fajr and Dhuhr, then Asr missed. That's 7.
        let mut log = whole_day("2026-10-26", Status::Prayed);
        log.push(entry("2026-10-27", Prayer::Fajr, Status::Prayed));
        log.push(entry("2026-10-27", Prayer::Dhuhr, Status::Prayed));
        log.push(entry("2026-10-27", Prayer::Asr, Status::Missed));
        assert_eq!(Summary::new(week, &log).longest_streak, 7);

        // Made up rather than missed: still not on time, so still 7.
        log[7].status = Status::Late;
        assert_eq!(Summary::new(week, &log).longest_streak, 7);

        // Asr on time joins the runs into 8.
        log[7].status = Status::Prayed;
        assert_eq!(Summary::new(week, &log).longest_streak, 8);
    }

    #[test]
    fn a_gap_in_the_log_ends_a_streak() {
        let week = Period::of(Span::Week, date("2026-10-26"));
        // Mon all 5, Tue nothing recorded, Wed all 5: two runs of 5.
        let mut log = whole_day("2026-10-26", Status::Prayed);
        log.extend(whole_day("2026-10-28", Status::Prayed));
        assert_eq!(Summary::new(week, &log).longest_streak, 5);
    }

    #[test]
    fn prayed_all_counts_made_up_prayers() {
        let mut log = whole_day("2026-10-27", Status::Prayed);
        assert!(prayed_all(&log, date("2026-10-27")));
        log[0].status = Status::Late;
        assert!(prayed_all(&log, date("2026-10-27")), "made up still counts");
        log[0].status = Status::Missed;
        assert!(!prayed_all(&log, date("2026-10-27")));
        assert!(!prayed_all(&log, date("2026-10-28")), "another day");
    }

    #[test]
    fn heatmap_squares_by_how_many_were_prayed() {
        assert_eq!(cell(None), "⬜");
        assert_eq!(cell(Some(0)), "🟥");
        assert_eq!(cell(Some(2)), "🟧");
        assert_eq!(cell(Some(3)), "🟨");
        assert_eq!(cell(Some(5)), "🟩");
    }

    #[test]
    fn day_text_says_you_in_a_dm_and_mentions_in_a_group() {
        let tuesday = date("2026-10-27");
        assert_eq!(
            day_text(tuesday, &[]),
            "🎉 You prayed *all 5 prayers* today (Tue 27 Oct). Masha'Allah, keep it up!"
        );
        let both = ["447700900001".to_string(), "447700900002".to_string()];
        assert_eq!(
            day_text(tuesday, &both),
            "🎉 *All 5 prayers* today (Tue 27 Oct): @447700900001, @447700900002. Masha'Allah!"
        );
    }

    #[test]
    fn week_summary_text() {
        let week = Period::of(Span::Week, date("2026-10-26"));
        let mut log = Vec::new();
        for day in 26..=31 {
            log.extend(whole_day(&format!("2026-10-{day}"), Status::Prayed));
        }
        log.extend(whole_day("2026-11-01", Status::Prayed));
        // Fajr missed on Tue and Sun, Isha missed Sun, Asr made up Wed.
        let set = |log: &mut Vec<Entry>, day: &str, prayer, status| {
            let e = log
                .iter_mut()
                .find(|e| e.date == date(day) && e.prayer == prayer)
                .unwrap();
            e.status = status;
        };
        set(&mut log, "2026-10-27", Prayer::Fajr, Status::Missed);
        set(&mut log, "2026-11-01", Prayer::Fajr, Status::Missed);
        set(&mut log, "2026-11-01", Prayer::Isha, Status::Missed);
        set(&mut log, "2026-10-28", Prayer::Asr, Status::Late);

        let text = summary_text(
            Span::Week,
            week,
            &[(Some("Bilal"), Summary::new(week, &log))],
        );
        assert_eq!(
            text,
            "📊 *Prayer week* · 26 Oct – 1 Nov\n\
             \n\
             *Bilal*\n\
             🟩🟨🟩🟩🟩🟩🟨\n\
             ✅ 32/35 prayed · 91% · ■■■■■■■■■□\n\
             🔥 Longest streak: 17 in a row\n\
             ❌ Missed: Fajr ×2 (Tue, Sun), Isha (Sun)\n\
             🕐 Made up: Asr (Wed)\n\
             \n\
             Mon to Sun: 🟩 all 5 · 🟨 3–4 · 🟧 1–2 · 🟥 none · ⬜ no record"
        );
    }

    #[test]
    fn a_dm_summary_has_no_name_and_a_clean_week_says_so() {
        let week = Period::of(Span::Week, date("2026-10-26"));
        let log = whole_day("2026-10-26", Status::Prayed);
        let text = summary_text(Span::Week, week, &[(None, Summary::new(week, &log))]);
        assert!(
            text.contains("1 Nov\n\n🟩⬜⬜⬜⬜⬜⬜\n✅ 5/5 prayed · 100%"),
            "{text}"
        );
        assert!(text.contains("🔥 Longest streak: 5 in a row\n"), "{text}");
        assert!(text.contains("❌ Missed: none 🎉\n"), "{text}");
        assert!(!text.contains("Made up"), "{text}");
    }

    #[test]
    fn month_summary_is_a_calendar_and_counts_misses() {
        // October 2026 starts on a Thursday and has 31 days: 5 rows.
        let october = Period::of(Span::Month, date("2026-10-15"));
        let mut log = whole_day("2026-10-01", Status::Prayed);
        log.push(entry("2026-10-02", Prayer::Fajr, Status::Missed));
        log.push(entry("2026-10-09", Prayer::Fajr, Status::Missed));
        let text = summary_text(Span::Month, october, &[(None, Summary::new(october, &log))]);
        assert!(
            text.starts_with("🗓️ *Prayer month* · October 2026\n\n"),
            "{text}"
        );
        assert!(
            text.contains("\n▫️▫️▫️🟩🟥⬜⬜\n⬜⬜⬜⬜🟥⬜⬜\n"),
            "padded to Monday: {text}"
        );
        assert!(
            text.contains("\n⬜⬜⬜⬜⬜⬜▫️\n"),
            "padded to Sunday: {text}"
        );
        assert!(text.contains("❌ Missed: Fajr ×2\n"), "no days: {text}");
        assert!(text.contains("Each row is Mon to Sun"), "{text}");
    }
}

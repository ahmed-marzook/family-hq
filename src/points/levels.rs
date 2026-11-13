//! Stats and levels (Stage P4): lifetime XP per category, a level for
//! each, and an Overall level that adds them up. Pure functions only, like
//! the rest of `points`: XP comes in, levels and text go out. Reading the
//! XP and remembering the levels people reached is `points::level_ups`.
//!
//! XP is the sum of a person's *positive* awards from every chat, so a
//! skipped todo's −1 lowers that week's points but never lifetime
//! progress. It only drops when an earned award is undone (a prayer
//! unticked), which is right: it shouldn't have counted.

use std::collections::BTreeMap;
use std::fmt;

use crate::points::Source;
use crate::progress;

const PRAYERS: &str = "🕌";
const TODOS: &str = "📝";
const CHART: &str = "📊";
const PARTY: &str = "🎉";
const MEDAL: &str = "🏅";

/// A kind of thing people earn XP for, each with its own level. A new
/// feature (habits) becomes a new variant, and the compiler then points at
/// every `match` that has to handle it. Declared in the order the stats
/// card shows them (`Ord` follows it, so a `BTreeMap` keeps that order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    Prayers,
    Todos,
}

impl Category {
    pub const ALL: [Category; 2] = [Category::Prayers, Category::Todos];

    /// Which category a ledger source's XP counts in.
    pub fn of(source: Source) -> Category {
        match source {
            Source::Prayer => Category::Prayers,
            // Only ever negative, so it lowers the week's todo points but
            // never XP.
            Source::Todo | Source::Overdue => Category::Todos,
        }
    }

    /// XP per level step: going from level L to L+1 costs `pace × L`.
    /// Set so a steady week moves each category at about the same speed
    /// (about 65 prayer XP and 25 todo XP a week). Changing a pace only
    /// moves levels, never the XP itself.
    pub fn pace(self) -> i64 {
        match self {
            Category::Prayers => 35,
            Category::Todos => 15,
        }
    }

    pub fn emoji(self) -> &'static str {
        match self {
            Category::Prayers => PRAYERS,
            Category::Todos => TODOS,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Category::Prayers => "Prayers",
            Category::Todos => "Todos",
        }
    }
}

/// What has a level: each category, and Overall (the sum of them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Track {
    Category(Category),
    Overall,
}

impl Track {
    /// Every track, categories first, so a reply announces a category's
    /// level-up before the Overall one it caused.
    pub fn all() -> impl Iterator<Item = Track> {
        Category::ALL
            .into_iter()
            .map(Track::Category)
            .chain([Track::Overall])
    }

    /// The exact text `levels_reached.category` stores (and its CHECK
    /// allows).
    pub fn as_db_str(self) -> &'static str {
        match self {
            Track::Category(Category::Prayers) => "prayers",
            Track::Category(Category::Todos) => "todos",
            Track::Overall => "overall",
        }
    }

    /// The other way: `None` for a track this build doesn't know (a later
    /// one, or one since removed), which is then left alone.
    pub fn from_db_str(s: &str) -> Option<Track> {
        Track::all().find(|t| t.as_db_str() == s)
    }

    /// The level everyone has with no XP: Lv 1 in a category, so Overall
    /// starts at one per category. Reaching it isn't news.
    pub fn start(self) -> i64 {
        match self {
            Track::Category(_) => 1,
            Track::Overall => Category::ALL.len() as i64,
        }
    }
}

/// Where someone is within a category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level {
    /// 1 to start with.
    pub level: i64,
    /// XP earned since reaching `level`.
    pub into: i64,
    /// XP the whole step to the next level takes (`pace × level`).
    pub needed: i64,
}

/// The total XP that reaching `level` takes: `pace × L × (L − 1) / 2`, the
/// sum of every step before it. Level 1 is free.
pub fn xp_to_reach(level: i64, pace: i64) -> i64 {
    pace * level * (level - 1) / 2
}

/// The level `xp` gives at `pace`. XP below 0 can't happen (only positive
/// awards count), but is treated as 0 rather than trusted.
pub fn level(xp: i64, pace: i64) -> Level {
    let xp = xp.max(0);
    let mut level = 1;
    // A loop rather than the square-root formula: no float rounding at the
    // boundaries, and levels stay small (level 50 is years away).
    while xp_to_reach(level + 1, pace) <= xp {
        level += 1;
    }
    Level {
        level,
        into: xp - xp_to_reach(level, pace),
        needed: pace * level,
    }
}

/// Overall: the sum of the category levels. It rewards being well-rounded
/// over maxing one category, and every category counts from Lv 1, so a
/// new one (habits) lifts everyone's Overall by one.
pub fn overall(levels: impl IntoIterator<Item = Level>) -> i64 {
    levels.into_iter().map(|l| l.level).sum()
}

/// Points per ledger source added up per category, with every category
/// present (0 if nothing came in for it), in the card's order.
pub fn by_category(rows: impl IntoIterator<Item = (Source, i64)>) -> BTreeMap<Category, i64> {
    let mut sums: BTreeMap<Category, i64> = Category::ALL.iter().map(|c| (*c, 0)).collect();
    for (source, points) in rows {
        *sums.entry(Category::of(source)).or_default() += points;
    }
    sums
}

/// One person's lifetime XP in each category. Every category is present
/// (0 if they've earned nothing in it), so levels and the card always
/// show all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    xp: BTreeMap<Category, i64>,
}

impl Stats {
    /// From XP per ledger source, as `repo::points::xp_by_source` reads
    /// it. Sources in the same category add up.
    pub fn from_sources(rows: impl IntoIterator<Item = (Source, i64)>) -> Stats {
        Stats {
            xp: by_category(rows),
        }
    }

    pub fn xp(&self, category: Category) -> i64 {
        self.xp.get(&category).copied().unwrap_or(0)
    }

    pub fn level(&self, category: Category) -> Level {
        level(self.xp(category), category.pace())
    }

    pub fn overall(&self) -> i64 {
        overall(Category::ALL.map(|c| self.level(c)))
    }

    /// The level on `track`.
    pub fn level_on(&self, track: Track) -> i64 {
        match track {
            Track::Category(c) => self.level(c).level,
            Track::Overall => self.overall(),
        }
    }
}

/// Each person's Overall level by user id, from `(user_id, source, xp)`
/// rows as `repo::points::xp_of_earners` reads them. Keyed by id, as the
/// ledger is: a name is only how someone is shown, so matching on it
/// could pool two people's XP if names ever stopped being unique.
/// Someone with XP in no category still gets every category's Lv 1.
pub fn overall_by_person(rows: impl IntoIterator<Item = (i64, Source, i64)>) -> BTreeMap<i64, i64> {
    let mut xp: BTreeMap<i64, Vec<(Source, i64)>> = BTreeMap::new();
    for (user_id, source, points) in rows {
        xp.entry(user_id).or_default().push((source, points));
    }
    xp.into_iter()
        .map(|(user_id, xp)| (user_id, Stats::from_sources(xp).overall()))
        .collect()
}

/// A level someone has just reached for the first time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelUp {
    pub track: Track,
    pub level: i64,
}

/// The tracks where `stats` is above the highest level ever reached
/// (`reached`; a track missing from it counts as its `start`, where
/// everyone begins). Comparing with the *highest* is what makes each level-up
/// announced once: drop below a level (an untick) and earn it back, and
/// it isn't new.
pub fn level_ups(reached: &BTreeMap<Track, i64>, stats: &Stats) -> Vec<LevelUp> {
    Track::all()
        .filter_map(|track| {
            let now = stats.level_on(track);
            let best = reached.get(&track).copied().unwrap_or(track.start());
            (now > best).then_some(LevelUp { track, level: now })
        })
        .collect()
}

/// The line a level-up adds to the reply that earned it:
/// "🎉 Prayers level 6!" / "🏅 Overall level 12". In a group, `who` names
/// the person, since anyone could have sent the message above it.
pub fn level_up_text(up: LevelUp, who: Option<&str>) -> String {
    let who = who.map_or(String::new(), |name| format!("{name}: "));
    match up.track {
        Track::Category(c) => format!("{PARTY} {who}{} level {}!", c.name(), up.level),
        Track::Overall => format!("{MEDAL} {who}Overall level {}", up.level),
    }
}

/// The stats card at the top of `/points` in a DM:
///
/// ```text
/// 📊 *Your stats* · Overall Lv 10
/// 🕌 Prayers Lv 6 ■■■■■■□□□□ 650 / 735
/// 📝 Todos Lv 4 ■■■■■□□□□□ 120 / 150
/// ```
///
/// Each bar is the way through the current level; the numbers are the
/// lifetime XP and what the next level needs in total.
impl fmt::Display for Stats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{CHART} *Your stats* · Overall Lv {}", self.overall())?;
        for c in Category::ALL {
            let xp = self.xp(c);
            let l = self.level(c);
            let next = xp - l.into + l.needed;
            write!(
                f,
                "\n{} {} Lv {} {} {xp} / {next}",
                c.emoji(),
                c.name(),
                l.level,
                progress::bar(l.into, l.needed)
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRAYER_PACE: i64 = 35;

    fn at(level: i64, into: i64, needed: i64) -> Level {
        Level {
            level,
            into,
            needed,
        }
    }

    #[test]
    fn reaching_a_level_costs_the_sum_of_the_steps_before_it() {
        assert_eq!(xp_to_reach(1, PRAYER_PACE), 0);
        assert_eq!(xp_to_reach(2, PRAYER_PACE), 35);
        assert_eq!(xp_to_reach(5, PRAYER_PACE), 350);
        assert_eq!(xp_to_reach(10, PRAYER_PACE), 1_575);
        assert_eq!(xp_to_reach(15, 15), 1_575);
    }

    #[test]
    fn zero_xp_is_level_one() {
        assert_eq!(level(0, PRAYER_PACE), at(1, 0, 35));
        assert_eq!(
            level(-3, PRAYER_PACE),
            at(1, 0, 35),
            "never trusted below 0"
        );
    }

    #[test]
    fn levels_change_exactly_at_each_boundary() {
        for l in 1..=20 {
            let reach = xp_to_reach(l, PRAYER_PACE);
            assert_eq!(level(reach, PRAYER_PACE), at(l, 0, PRAYER_PACE * l));
            if l > 1 {
                // One short of the boundary: the level below, nearly full.
                let below = PRAYER_PACE * (l - 1);
                assert_eq!(level(reach - 1, PRAYER_PACE), at(l - 1, below - 1, below));
            }
        }
    }

    #[test]
    fn the_roadmap_examples_hold() {
        // Prayers 650 XP: Lv 6 starts at 525, Lv 7 at 735.
        assert_eq!(level(650, 35), at(6, 125, 210));
        assert_eq!(level(120, 15), at(4, 30, 60));
    }

    fn stats(prayers: i64, todos: i64) -> Stats {
        Stats::from_sources([(Source::Prayer, prayers), (Source::Todo, todos)])
    }

    #[test]
    fn xp_that_drops_drops_the_level() {
        assert_eq!(stats(35, 0).level(Category::Prayers).level, 2);
        // A prayer unticked takes away its 2 XP: back under the boundary.
        assert_eq!(stats(33, 0).level(Category::Prayers).level, 1);
    }

    #[test]
    fn overall_is_the_sum_of_the_category_levels() {
        assert_eq!(stats(0, 0).overall(), 2, "everyone starts at Lv 1 in each");
        assert_eq!(stats(650, 120).overall(), 10);
        assert_eq!(overall([at(3, 0, 1), at(4, 0, 1)]), 7);
        assert_eq!(stats(650, 120).level_on(Track::Overall), 10);
    }

    #[test]
    fn sources_add_up_into_their_category() {
        let s = Stats::from_sources([(Source::Todo, 10), (Source::Todo, 5)]);
        assert_eq!(s.xp(Category::Todos), 15);
        assert_eq!(s.xp(Category::Prayers), 0);
    }

    #[test]
    fn overall_by_person_adds_up_each_persons_categories() {
        // Sara is 1, Bilal 2.
        let rows = vec![
            (1, Source::Prayer, 650),
            (2, Source::Todo, 15),
            (1, Source::Todo, 120),
        ];
        assert_eq!(overall_by_person(rows), BTreeMap::from([(1, 10), (2, 3)]));
    }

    #[test]
    fn tracks_round_trip_through_the_database_text() {
        for track in Track::all() {
            assert_eq!(Track::from_db_str(track.as_db_str()), Some(track));
        }
        assert_eq!(Track::from_db_str("habits"), None, "not built yet");
    }

    fn reached(list: &[(Track, i64)]) -> BTreeMap<Track, i64> {
        list.iter().copied().collect()
    }

    #[test]
    fn a_level_above_the_highest_reached_is_new() {
        let prayers = Track::Category(Category::Prayers);
        // Nothing recorded yet: Lv 1 is where everyone starts, so Lv 2 is new.
        assert_eq!(
            level_ups(&reached(&[]), &stats(35, 0)),
            vec![
                LevelUp {
                    track: prayers,
                    level: 2
                },
                LevelUp {
                    track: Track::Overall,
                    level: 3
                },
            ]
        );
        assert!(level_ups(&reached(&[]), &stats(34, 14)).is_empty());
    }

    #[test]
    fn earning_a_level_back_is_not_new() {
        let prayers = Track::Category(Category::Prayers);
        let todos = Track::Category(Category::Todos);
        let best = reached(&[(prayers, 2), (todos, 1), (Track::Overall, 3)]);
        // Unticked below Lv 2, then ticked back: the same as before.
        assert!(level_ups(&best, &stats(33, 0)).is_empty());
        assert!(level_ups(&best, &stats(35, 0)).is_empty());
        // Only what's above the highest counts.
        assert_eq!(
            level_ups(&best, &stats(35, 15)),
            vec![
                LevelUp {
                    track: todos,
                    level: 2
                },
                LevelUp {
                    track: Track::Overall,
                    level: 4
                },
            ]
        );
    }

    #[test]
    fn level_up_lines_name_the_person_in_a_group() {
        let up = LevelUp {
            track: Track::Category(Category::Prayers),
            level: 6,
        };
        assert_eq!(level_up_text(up, None), "🎉 Prayers level 6!");
        assert_eq!(level_up_text(up, Some("Sara")), "🎉 Sara: Prayers level 6!");
        let overall = LevelUp {
            track: Track::Overall,
            level: 12,
        };
        assert_eq!(level_up_text(overall, None), "🏅 Overall level 12");
    }

    #[test]
    fn the_stats_card_shows_each_category_and_overall() {
        assert_eq!(
            stats(650, 120).to_string(),
            "📊 *Your stats* · Overall Lv 10\n\
             🕌 Prayers Lv 6 ■■■■■■□□□□ 650 / 735\n\
             📝 Todos Lv 4 ■■■■■□□□□□ 120 / 150"
        );
        assert_eq!(
            stats(0, 0).to_string(),
            "📊 *Your stats* · Overall Lv 2\n\
             🕌 Prayers Lv 1 □□□□□□□□□□ 0 / 35\n\
             📝 Todos Lv 1 □□□□□□□□□□ 0 / 15"
        );
    }
}

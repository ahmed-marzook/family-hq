//! The weekly game (Stage P4, part 2): everyone in a group starts from 0
//! each Monday, and the Sunday summary names a champion per category as
//! well as overall. Pure functions only, like the rest of `points`.
//!
//! Weekly points stay the raw sum, so prayers (up to 10 a day) can outweigh
//! todos. The category champions balance that: more people can win
//! something each week.

use std::collections::BTreeMap;

use crate::points::levels::{Category, Track};
use crate::points::{RULES, Source, TROPHY};

const FLAG: &str = "🏁";

/// The last line of the Sunday summary, so the Monday reset isn't a
/// surprise.
pub const RESET: &str = "Scores start again tomorrow. 🏁";

/// `(name, source, points)` rows added up per person and category, as
/// `(name, category, points)`, sorted by name then category. Sources in
/// the same category (habits and goals, later) count together.
pub fn per_category(
    rows: impl IntoIterator<Item = (String, Source, i64)>,
) -> Vec<(String, Category, i64)> {
    let mut sums: BTreeMap<(String, Category), i64> = BTreeMap::new();
    for (name, source, points) in rows {
        *sums.entry((name, Category::of(source))).or_default() += points;
    }
    sums.into_iter()
        .map(|((name, category), points)| (name, category, points))
        .collect()
}

/// The winner (or winners, on a tie) of one category, or of the week.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Champion {
    pub track: Track,
    /// Everyone on the top score, by name.
    pub names: Vec<String>,
    pub points: i64,
}

/// The top of `(name, points)`, if anyone scored above 0: a week of only
/// skips has no winner.
fn top<'a>(scores: impl IntoIterator<Item = (&'a str, i64)>) -> Option<(Vec<String>, i64)> {
    let scores: Vec<(&str, i64)> = scores.into_iter().filter(|(_, p)| *p > 0).collect();
    let best = scores.iter().map(|(_, p)| *p).max()?;
    let mut names: Vec<String> = scores
        .iter()
        .filter(|(_, p)| *p == best)
        .map(|(n, _)| n.to_string())
        .collect();
    names.sort();
    Some((names, best))
}

/// The week's champions: one per category anyone earned in (in the card's
/// order), then Overall from `totals`. Ties are shared.
pub fn champions(
    totals: &[(String, i64)],
    by_category: &[(String, Category, i64)],
) -> Vec<Champion> {
    let mut champions: Vec<Champion> = Category::ALL
        .into_iter()
        .filter_map(|category| {
            let scores = by_category
                .iter()
                .filter(|(_, c, _)| *c == category)
                .map(|(n, _, p)| (n.as_str(), *p));
            let (names, points) = top(scores)?;
            Some(Champion {
                track: Track::Category(category),
                names,
                points,
            })
        })
        .collect();
    if let Some((names, points)) = top(totals.iter().map(|(n, p)| (n.as_str(), *p))) {
        champions.push(Champion {
            track: Track::Overall,
            names,
            points,
        });
    }
    champions
}

/// "🕌 Most prayers: Khadija · 📝 Most todos: Bilal · 🏆 Overall: Sara",
/// or `None` with no champions.
pub fn champions_text(champions: &[Champion]) -> Option<String> {
    if champions.is_empty() {
        return None;
    }
    let parts: Vec<String> = champions
        .iter()
        .map(|c| {
            let names = c.names.join(" & ");
            match c.track {
                Track::Category(category) => format!(
                    "{} Most {}: {names}",
                    category.emoji(),
                    category.name().to_lowercase()
                ),
                Track::Overall => format!("{TROPHY} Overall: {names}"),
            }
        })
        .collect();
    Some(parts.join(" · "))
}

/// `/points` in a group when nobody has earned anything yet this week (a
/// Monday, usually): last week's winner, so the reset is visible rather
/// than looking like the points were lost. `None` if last week had no
/// winner either.
pub fn new_week_text(last_week: &[(String, i64)]) -> Option<String> {
    let (names, points) = top(last_week.iter().map(|(n, p)| (n.as_str(), *p)))?;
    Some(format!(
        "{FLAG} A new week: everyone starts from 0.\n\
         Last week's winner: {TROPHY} {} ({points}).\n\
         Tick off a todo or a prayer to earn some: {RULES}.",
        names.join(" & ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(list: &[(&str, Source, i64)]) -> Vec<(String, Category, i64)> {
        per_category(list.iter().map(|(n, s, p)| (n.to_string(), *s, *p)))
    }

    fn totals(list: &[(&str, i64)]) -> Vec<(String, i64)> {
        list.iter().map(|(n, p)| (n.to_string(), *p)).collect()
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn per_category_adds_up_each_persons_sources() {
        assert_eq!(
            rows(&[
                ("Sara", Source::Todo, 3),
                ("Bilal", Source::Prayer, 4),
                ("Sara", Source::Todo, -1),
            ]),
            vec![
                ("Bilal".to_string(), Category::Prayers, 4),
                ("Sara".to_string(), Category::Todos, 2),
            ]
        );
    }

    #[test]
    fn a_champion_per_category_and_overall() {
        let by_category = rows(&[
            ("Khadija", Source::Prayer, 20),
            ("Sara", Source::Prayer, 14),
            ("Sara", Source::Todo, 20),
            ("Bilal", Source::Todo, 21),
        ]);
        let week = totals(&[("Khadija", 20), ("Sara", 34), ("Bilal", 21)]);
        let got = champions(&week, &by_category);
        assert_eq!(
            got,
            vec![
                Champion {
                    track: Track::Category(Category::Prayers),
                    names: names(&["Khadija"]),
                    points: 20
                },
                Champion {
                    track: Track::Category(Category::Todos),
                    names: names(&["Bilal"]),
                    points: 21
                },
                Champion {
                    track: Track::Overall,
                    names: names(&["Sara"]),
                    points: 34
                },
            ]
        );
        assert_eq!(
            champions_text(&got).unwrap(),
            "🕌 Most prayers: Khadija · 📝 Most todos: Bilal · 🏆 Overall: Sara"
        );
    }

    #[test]
    fn ties_share_and_skips_never_win() {
        let by_category = rows(&[
            ("Sara", Source::Todo, 3),
            ("Bilal", Source::Todo, 3),
            ("Khadija", Source::Todo, -1),
        ]);
        let week = totals(&[("Sara", 3), ("Bilal", 3), ("Khadija", -1)]);
        assert_eq!(
            champions_text(&champions(&week, &by_category)).unwrap(),
            "📝 Most todos: Bilal & Sara · 🏆 Overall: Bilal & Sara",
            "no prayer champion when nobody prayed"
        );

        let skips = rows(&[("Khadija", Source::Todo, -1)]);
        let week = totals(&[("Khadija", -1)]);
        assert!(champions(&week, &skips).is_empty());
        assert_eq!(champions_text(&[]), None);
    }

    #[test]
    fn a_new_week_names_last_weeks_winner() {
        let text = new_week_text(&totals(&[("Sara", 34), ("Bilal", 21)])).unwrap();
        assert!(
            text.starts_with(
                "🏁 A new week: everyone starts from 0.\nLast week's winner: 🏆 Sara (34)."
            ),
            "{text}"
        );
        assert_eq!(new_week_text(&[]), None);
        assert_eq!(new_week_text(&totals(&[("Bilal", -1)])), None);
    }
}

//! Shared candidate ranking for the popup and sources that cap their results.
use nucleo_matcher::{
    Config, Utf32String,
    pattern::{AtomKind, CaseMatching, Normalization, Pattern},
};
use std::cmp::Reverse;

/// Lower scores are better. Provider order breaks equal-quality matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Score {
    category: u8,
    penalty: usize,
    fuzzy: Reverse<u32>,
    length: usize,
}
pub struct Matcher {
    query: String,
    folded: String,
    characters: Vec<char>,
    pattern: Pattern,
    fuzzy: nucleo_matcher::Matcher,
}
impl Matcher {
    pub fn new(query: &str) -> Self {
        let folded = query.to_lowercase();
        Self {
            query: query.into(),
            characters: folded.chars().collect(),
            folded,
            pattern: Pattern::new(
                query,
                CaseMatching::Ignore,
                Normalization::Smart,
                AtomKind::Fuzzy,
            ),
            fuzzy: nucleo_matcher::Matcher::new(Config::DEFAULT),
        }
    }
    pub fn score(&mut self, candidate: &str) -> Option<Score> {
        let score = |category, penalty, fuzzy| Score {
            category,
            penalty,
            fuzzy: Reverse(fuzzy),
            length: 0,
        };
        if self.query.is_empty() || candidate == self.query {
            return Some(score(0, 0, 0));
        }
        let folded = candidate.to_lowercase();
        if folded == self.folded {
            return Some(score(1, 0, 0));
        }
        if folded.starts_with(&self.folded) {
            return Some(score(2, folded.chars().count() - self.characters.len(), 0));
        }
        // Long identifiers still support exact/prefix matching; bound fuzzy work.
        if self.characters.len() > 128 {
            return None;
        }
        let budget = match self.characters.len() {
            0..=3 => 0,
            4..=7 => 1,
            _ => 2,
        };
        if budget > 0 {
            let prefix: Vec<_> = folded
                .chars()
                .take(self.characters.len() + budget)
                .collect();
            if let Some(distance) = prefix_distance(&self.characters, &prefix, budget) {
                return Some(Score {
                    length: folded.chars().count().abs_diff(self.characters.len()),
                    ..score(3, distance, 0)
                });
            }
        }
        let haystack = Utf32String::from(candidate);
        self.pattern
            .score(haystack.slice(..), &mut self.fuzzy)
            .map(|fuzzy| score(4, 0, fuzzy))
    }
}

/// Bounded optimal-string-alignment distance to a candidate prefix. Adjacent
/// transpositions count once, and an untyped completion suffix has no cost.
fn prefix_distance(query: &[char], candidate: &[char], budget: usize) -> Option<usize> {
    if candidate.len() + budget < query.len() {
        return None;
    }
    let infinity = budget + 1;
    let mut previous: Vec<_> = (0..=candidate.len())
        .map(|column| column.min(infinity))
        .collect();
    let mut before_previous = previous.clone();
    let mut current = vec![infinity; previous.len()];
    for row in 1..=query.len() {
        current[0] = row.min(infinity);
        let first = row.saturating_sub(budget).max(1);
        let last = (row + budget).min(candidate.len());
        if first > 1 {
            current[first - 1] = infinity;
        }
        if last < candidate.len() {
            current[last + 1] = infinity;
        }
        for column in first..=last {
            current[column] = (previous[column] + 1)
                .min(current[column - 1] + 1)
                .min(previous[column - 1] + usize::from(query[row - 1] != candidate[column - 1]));
            if row > 1
                && column > 1
                && query[row - 1] == candidate[column - 2]
                && query[row - 2] == candidate[column - 1]
            {
                current[column] = current[column].min(before_previous[column - 2] + 1);
            }
        }
        if current[first..=last]
            .iter()
            .copied()
            .min()
            .unwrap_or(infinity)
            .min(current[0])
            > budget
        {
            return None;
        }
        std::mem::swap(&mut before_previous, &mut previous);
        std::mem::swap(&mut previous, &mut current);
    }
    previous
        .into_iter()
        .skip(query.len().saturating_sub(budget))
        .min()
        .filter(|distance| *distance <= budget)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_prefix_subsequence_and_typo_matches_have_predictable_priority() {
        let mut matcher = Matcher::new("print");
        assert!(matcher.score("print").unwrap() < matcher.score("PRINT").unwrap());
        assert!(matcher.score("PRINT").unwrap() < matcher.score("println").unwrap());
        assert!(matcher.score("println").unwrap() < matcher.score("pront").unwrap());
        assert!(matcher.score("pront").unwrap() < matcher.score("project_integer").unwrap());
        assert!(matcher.score("unrelated").is_none());
    }
    #[test]
    fn typo_matching_handles_swaps_substitutions_insertions_and_missing_letters() {
        for (query, candidate) in [
            ("pritn", "print"),
            ("pront", "println"),
            ("priint", "print"),
            ("prnt", "print"),
            ("compleiton", "completion"),
            ("compleitom", "completion"),
        ] {
            assert!(
                Matcher::new(query).score(candidate).is_some(),
                "{query} -> {candidate}"
            );
        }
        assert!(
            Matcher::new("pni").score("print").is_none(),
            "short queries must not introduce typo noise"
        );
        assert!(Matcher::new("zzzz").score("print").is_none());
    }
    #[test]
    fn unicode_and_empty_queries_work_and_long_typo_queries_are_bounded() {
        assert!(Matcher::new("世介接口").score("世界接口").is_some());
        assert!(Matcher::new("界接").score("世界接口").is_some());
        assert!(Matcher::new("").score("anything").is_some());
        let long = "a".repeat(129);
        assert!(Matcher::new(&long).score(&long).is_some());
        assert!(
            Matcher::new(&long)
                .score(&(long.clone() + "suffix"))
                .is_some()
        );
        assert!(
            Matcher::new(&(long + "z"))
                .score(&"a".repeat(130))
                .is_none()
        );
    }
    #[test]
    fn single_edits_at_every_position_are_recognized_as_close_typos() {
        for candidate in ["print", "completion", "世界编辑器"] {
            let characters: Vec<_> = candidate.chars().collect();
            for position in 0..characters.len() {
                let mut query = characters.clone();
                query[position] = 'z';
                let query: String = query.into_iter().collect();
                assert!(
                    Matcher::new(&query)
                        .score(candidate)
                        .is_some_and(|score| score.category <= 3),
                    "{query} -> {candidate}"
                );
                let mut query = characters.clone();
                query.remove(position);
                let query: String = query.into_iter().collect();
                assert!(
                    Matcher::new(&query)
                        .score(candidate)
                        .is_some_and(|score| score.category <= 3),
                    "{query} -> {candidate}"
                );
                if position + 1 < characters.len() {
                    let mut query = characters.clone();
                    query.swap(position, position + 1);
                    let query: String = query.into_iter().collect();
                    assert!(
                        Matcher::new(&query)
                            .score(candidate)
                            .is_some_and(|score| score.category <= 3),
                        "{query} -> {candidate}"
                    );
                }
            }
            for position in 0..=characters.len() {
                let mut query = characters.clone();
                query.insert(position, 'z');
                let query: String = query.into_iter().collect();
                assert!(
                    Matcher::new(&query)
                        .score(candidate)
                        .is_some_and(|score| score.category <= 3),
                    "{query} -> {candidate}"
                );
            }
        }
        assert!(Matcher::new("xxxpletion").score("completion").is_none());
    }
}

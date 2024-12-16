use crate::{
    matcher::{Match, MatchCandidate, Matcher},
    CharBag,
};
use gpui::BackgroundExecutor;
use std::{
    borrow::Cow,
    cmp::{self, Ordering},
    iter,
    ops::Range,
    sync::atomic::AtomicBool,
};

#[derive(Clone, Debug)]
pub struct StringMatchCandidate {
    pub id: usize,
    pub string: String,
    pub char_bag: CharBag,
}

impl StringMatchCandidate {
    pub fn new(id: usize, string: &str) -> Self {
        Self {
            id,
            string: string.into(),
            char_bag: string.into(),
        }
    }
}

impl<'a> MatchCandidate for &'a StringMatchCandidate {
    fn has_chars(&self, bag: CharBag) -> bool {
        self.char_bag.is_superset(bag)
    }

    fn to_string(&self) -> Cow<'a, str> {
        self.string.as_str().into()
    }
}

#[derive(Clone, Debug)]
pub struct StringMatch {
    pub candidate_id: usize,
    pub score: f64,
    pub positions: Vec<usize>,
    pub string: String,
}

impl Match for StringMatch {
    fn score(&self) -> f64 {
        self.score
    }

    fn set_positions(&mut self, positions: Vec<usize>) {
        self.positions = positions;
    }
}

impl StringMatch {
    pub fn ranges(&self) -> impl '_ + Iterator<Item = Range<usize>> {
        let mut positions = self.positions.iter().peekable();
        iter::from_fn(move || {
            if let Some(start) = positions.next().copied() {
                let Some(char_len) = self.char_len_at_index(start) else {
                    log::error!(
                        "Invariant violation: Index {start} out of range or not on a utf-8 boundary in string {:?}",
                        self.string
                    );
                    return None;
                };
                let mut end = start + char_len;
                while let Some(next_start) = positions.peek() {
                    if end == **next_start {
                        let Some(char_len) = self.char_len_at_index(end) else {
                            log::error!(
                                "Invariant violation: Index {end} out of range or not on a utf-8 boundary in string {:?}",
                                self.string
                            );
                            return None;
                        };
                        end += char_len;
                        positions.next();
                    } else {
                        break;
                    }
                }

                return Some(start..end);
            }
            None
        })
    }

    /// Gets the byte length of the utf-8 character at a byte offset. If the index is out of range
    /// or not on a utf-8 boundary then None is returned.
    fn char_len_at_index(&self, ix: usize) -> Option<usize> {
        self.string
            .get(ix..)
            .and_then(|slice| slice.chars().next().map(|char| char.len_utf8()))
    }
}

impl PartialEq for StringMatch {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for StringMatch {}

impl PartialOrd for StringMatch {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for StringMatch {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .partial_cmp(&other.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| self.candidate_id.cmp(&other.candidate_id))
    }
}

pub async fn match_strings(
    candidates: &[StringMatchCandidate],
    query: &str,
    smart_case: bool,
    max_results: usize,
    cancel_flag: &AtomicBool,
    executor: BackgroundExecutor,
) -> Vec<StringMatch> {
    if candidates.is_empty() || max_results == 0 {
        return Default::default();
    }

    if query.is_empty() {
        return candidates
            .iter()
            .map(|candidate| scored_candidate_to_string_match(&candidate, 0.))
            .collect();
    }

    let lowercase_query = query.to_lowercase().chars().collect::<Vec<_>>();
    let query = query.chars().collect::<Vec<_>>();

    let lowercase_query = &lowercase_query;
    let query = &query;
    let query_char_bag = CharBag::from(&lowercase_query[..]);

    let num_cpus = executor.num_cpus().min(candidates.len());
    let segment_size = (candidates.len() + num_cpus - 1) / num_cpus;
    let mut segment_results = (0..num_cpus)
        .map(|_| Vec::with_capacity(max_results.min(candidates.len())))
        .collect::<Vec<_>>();

    executor
        .scoped(|scope| {
            for (segment_idx, results) in segment_results.iter_mut().enumerate() {
                let cancel_flag = &cancel_flag;
                scope.spawn(async move {
                    let segment_start = cmp::min(segment_idx * segment_size, candidates.len());
                    let segment_end = cmp::min(segment_start + segment_size, candidates.len());
                    let mut matcher = Matcher::new(
                        query,
                        lowercase_query,
                        query_char_bag,
                        smart_case,
                        max_results,
                    );

                    matcher.match_candidates(
                        &[],
                        &[],
                        candidates[segment_start..segment_end].iter(),
                        results,
                        cancel_flag,
                        scored_candidate_to_string_match,
                    );
                });
            }
        })
        .await;

    let mut results = Vec::new();
    for segment_result in segment_results {
        if results.is_empty() {
            results = segment_result;
        } else {
            util::extend_sorted(&mut results, segment_result, max_results, |a, b| b.cmp(a));
        }
    }
    results
}

pub fn match_strings_synchronously(
    candidates: &[StringMatchCandidate],
    query: &str,
    smart_case: bool,
    max_results: usize,
    cancel_flag: &AtomicBool,
) -> Vec<StringMatch> {
    if candidates.is_empty() || max_results == 0 {
        return Default::default();
    }

    if query.is_empty() {
        return candidates
            .iter()
            .map(|candidate| scored_candidate_to_string_match(&candidate, 0.))
            .collect();
    }

    let lowercase_query = query.to_lowercase().chars().collect::<Vec<_>>();
    let query = query.chars().collect::<Vec<_>>();

    let lowercase_query = &lowercase_query;
    let query = &query;
    let query_char_bag = CharBag::from(&lowercase_query[..]);

    let mut matcher = Matcher::new(
        query,
        lowercase_query,
        query_char_bag,
        smart_case,
        max_results,
    );

    let mut results = Vec::with_capacity(max_results);
    matcher.match_candidates(
        &[],
        &[],
        candidates.iter(),
        &mut results,
        cancel_flag,
        scored_candidate_to_string_match,
    );
    results
}

fn scored_candidate_to_string_match(candidate: &&StringMatchCandidate, score: f64) -> StringMatch {
    StringMatch {
        candidate_id: candidate.id,
        score,
        positions: Vec::new(),
        string: candidate.string.to_string(),
    }
}

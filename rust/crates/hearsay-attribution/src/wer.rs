//! Word error rate and concatenated-permutation WER (cpWER): the transcript-accuracy counterpart of
//! [`crate::eval`]'s diarization metrics. Pure logic — no I/O, no ML — so it is unit-tested without
//! any audio.
//!
//! [`normalize`] turns text into the comparable word list both sides are scored on. [`word_errors`]
//! is the plain WER (speaker-agnostic); [`cpwer`] additionally scores speaker attribution by
//! assigning each hypothesis speaker to the reference speaker that minimizes the total error.

use std::collections::BTreeMap;

/// Disfluencies and backchannels dropped from both reference and hypothesis, because annotators and
/// ASR models disagree on whether to write them down, which would otherwise dominate the score.
const FILLERS: &[&str] = &[
    "uh", "um", "uhm", "erm", "hm", "hmm", "mm", "mmm", "mhm", "mmhmm", "mm-hmm", "mm-hm",
    "uh-huh", "uh-uh",
];

const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

/// Spell a non-negative integer below one million as English words, so an ASR digit string ("25")
/// scores against an annotator's spelled-out words ("twenty five").
fn number_words(n: u64, out: &mut Vec<String>) {
    if n >= 1_000_000 {
        out.push(n.to_string());
        return;
    }
    if n >= 1000 {
        number_words(n / 1000, out);
        out.push("thousand".to_string());
        if !n.is_multiple_of(1000) {
            number_words(n % 1000, out);
        }
        return;
    }
    if n >= 100 {
        out.push(ONES[(n / 100) as usize].to_string());
        out.push("hundred".to_string());
        if !n.is_multiple_of(100) {
            number_words(n % 100, out);
        }
        return;
    }
    if n >= 20 {
        out.push(TENS[(n / 10) as usize].to_string());
        if !n.is_multiple_of(10) {
            out.push(ONES[(n % 10) as usize].to_string());
        }
        return;
    }
    out.push(ONES[n as usize].to_string());
}

/// Normalize `text` into the word list WER is computed on: lowercase, punctuation stripped (inner
/// apostrophes kept), hyphenated compounds split, fillers dropped, digit strings spelled out, and
/// `ok` unified with `okay`.
pub fn normalize(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    for raw in text.split_whitespace() {
        let raw = if raw.chars().all(|c| c.is_ascii_digit() || c == ',') {
            raw.replace(',', "")
        } else {
            raw.to_string()
        };
        let token = raw
            .to_lowercase()
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_string();
        if token.is_empty() || FILLERS.contains(&token.as_str()) {
            continue;
        }
        let cleaned: String = token
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '\'' {
                    c
                } else {
                    ' '
                }
            })
            .collect();
        for part in cleaned.split_whitespace() {
            let part = part.trim_matches('\'');
            if part.is_empty() || FILLERS.contains(&part) {
                continue;
            }
            if let Ok(n) = part.parse::<u64>() {
                number_words(n, &mut words);
            } else if part == "ok" {
                words.push("okay".to_string());
            } else {
                words.push(part.to_string());
            }
        }
    }
    words
}

/// Substitution / deletion / insertion counts of one alignment, against `reference_words`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WerBreakdown {
    pub reference_words: usize,
    pub substitutions: usize,
    pub deletions: usize,
    pub insertions: usize,
}

impl WerBreakdown {
    pub fn errors(&self) -> usize {
        self.substitutions + self.deletions + self.insertions
    }

    /// Errors per reference word. With an empty reference every hypothesis word is an insertion, so
    /// the rate is scored against one word rather than dividing by zero.
    pub fn wer(&self) -> f64 {
        self.errors() as f64 / self.reference_words.max(1) as f64
    }

    fn add(&mut self, other: &WerBreakdown) {
        self.reference_words += other.reference_words;
        self.substitutions += other.substitutions;
        self.deletions += other.deletions;
        self.insertions += other.insertions;
    }
}

#[derive(Clone, Copy, Default)]
struct Cell {
    cost: usize,
    subs: usize,
    dels: usize,
    ins: usize,
}

/// Minimum-edit-distance alignment of `hypothesis` against `reference`, with the error type counts.
/// Ties prefer substitution, then deletion, then insertion, so the counts are deterministic.
pub fn word_errors(reference: &[String], hypothesis: &[String]) -> WerBreakdown {
    let mut previous: Vec<Cell> = (0..=hypothesis.len())
        .map(|j| Cell {
            cost: j,
            ins: j,
            ..Cell::default()
        })
        .collect();
    for (i, ref_word) in reference.iter().enumerate() {
        let mut current = vec![Cell::default(); hypothesis.len() + 1];
        current[0] = Cell {
            cost: i + 1,
            dels: i + 1,
            ..Cell::default()
        };
        for (j, hyp_word) in hypothesis.iter().enumerate() {
            let diagonal = previous[j];
            let substitute = Cell {
                cost: diagonal.cost + usize::from(ref_word != hyp_word),
                subs: diagonal.subs + usize::from(ref_word != hyp_word),
                ..diagonal
            };
            let up = previous[j + 1];
            let delete = Cell {
                cost: up.cost + 1,
                dels: up.dels + 1,
                ..up
            };
            let left = current[j];
            let insert = Cell {
                cost: left.cost + 1,
                ins: left.ins + 1,
                ..left
            };
            let mut best = substitute;
            if delete.cost < best.cost {
                best = delete;
            }
            if insert.cost < best.cost {
                best = insert;
            }
            current[j + 1] = best;
        }
        previous = current;
    }
    let end = previous[hypothesis.len()];
    WerBreakdown {
        reference_words: reference.len(),
        substitutions: end.subs,
        deletions: end.dels,
        insertions: end.ins,
    }
}

/// A [`cpwer`] result: the summed error breakdown and which hypothesis speaker was matched to which
/// reference speaker (`None` on the side that had no counterpart).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cpwer {
    pub breakdown: WerBreakdown,
    pub assignment: Vec<(Option<String>, Option<String>)>,
}

/// Largest speaker count (after padding both sides to equal length) the exact assignment handles.
const MAX_ASSIGNMENT: usize = 20;

/// Concatenated-permutation WER: each speaker's words are one stream in time order, and the
/// hypothesis speakers are matched one-to-one to the reference speakers (padding the shorter side
/// with empty streams) so the summed errors are minimal. A hypothesis that splits one person across
/// two speakers pays deletions on the reference stream and insertions on the extra one; one that
/// merges two people pays the same in the other direction.
///
/// Returns `None` when there are more than [`MAX_ASSIGNMENT`] speakers on either side.
pub fn cpwer(
    reference: &BTreeMap<String, Vec<String>>,
    hypothesis: &BTreeMap<String, Vec<String>>,
) -> Option<Cpwer> {
    let size = reference.len().max(hypothesis.len());
    if size > MAX_ASSIGNMENT {
        return None;
    }
    let empty: Vec<String> = Vec::new();
    let ref_names: Vec<Option<&String>> = (0..size).map(|i| reference.keys().nth(i)).collect();
    let hyp_names: Vec<Option<&String>> = (0..size).map(|j| hypothesis.keys().nth(j)).collect();
    let cost: Vec<Vec<WerBreakdown>> = ref_names
        .iter()
        .map(|r| {
            let ref_words = r.map_or(&empty, |name| &reference[name]);
            hyp_names
                .iter()
                .map(|h| {
                    let hyp_words = h.map_or(&empty, |name| &hypothesis[name]);
                    word_errors(ref_words, hyp_words)
                })
                .collect()
        })
        .collect();

    // Bitmask DP over the hypothesis columns already used: best[mask] after assigning the first
    // `popcount(mask)` reference rows.
    let full = 1usize << size;
    let mut best = vec![usize::MAX; full];
    let mut choice = vec![0usize; full];
    best[0] = 0;
    for mask in 0..full {
        if best[mask] == usize::MAX {
            continue;
        }
        let row = mask.count_ones() as usize;
        if row >= size {
            continue;
        }
        for (col, entry) in cost[row].iter().enumerate() {
            if mask & (1 << col) != 0 {
                continue;
            }
            let next = mask | (1 << col);
            let total = best[mask] + entry.errors();
            if total < best[next] {
                best[next] = total;
                choice[next] = col;
            }
        }
    }

    let mut columns = vec![0usize; size];
    let mut mask = full - 1;
    for row in (0..size).rev() {
        let col = choice[mask];
        columns[row] = col;
        mask &= !(1 << col);
    }
    let mut breakdown = WerBreakdown::default();
    let mut assignment = Vec::with_capacity(size);
    for (row, &col) in columns.iter().enumerate() {
        breakdown.add(&cost[row][col]);
        assignment.push((ref_names[row].cloned(), hyp_names[col].cloned()));
    }
    Some(Cpwer {
        breakdown,
        assignment,
    })
}

/// Latency distribution summary in the unit of the input samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Percentiles {
    pub count: usize,
    pub p50: f64,
    pub p90: f64,
    pub max: f64,
}

/// Nearest-rank p50 / p90 / max of `samples`; `None` when empty.
pub fn percentiles(samples: &[f64]) -> Option<Percentiles> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let rank = |p: f64| {
        let index = (p * sorted.len() as f64).ceil() as usize;
        sorted[index.clamp(1, sorted.len()) - 1]
    };
    Some(Percentiles {
        count: sorted.len(),
        p50: rank(0.5),
        p90: rank(0.9),
        max: sorted[sorted.len() - 1],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        normalize(text)
    }

    fn by_speaker(items: &[(&str, &str)]) -> BTreeMap<String, Vec<String>> {
        let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (speaker, text) in items {
            map.entry((*speaker).to_string())
                .or_default()
                .extend(normalize(text));
        }
        map
    }

    #[test]
    fn normalize_lowercases_strips_punctuation_and_keeps_apostrophes() {
        assert_eq!(
            words("Hello, World! It's fine."),
            vec!["hello", "world", "it's", "fine"]
        );
    }

    #[test]
    fn normalize_drops_fillers_and_backchannels() {
        assert_eq!(words("Uh, so um yeah. Uh-huh. Mm-hmm."), vec!["so", "yeah"]);
    }

    #[test]
    fn normalize_splits_hyphenated_compounds_and_unifies_ok() {
        assert_eq!(
            words("well-known OK ok"),
            vec!["well", "known", "okay", "okay"]
        );
    }

    #[test]
    fn normalize_spells_out_numbers() {
        assert_eq!(words("25"), vec!["twenty", "five"]);
        assert_eq!(
            words("1,250"),
            vec!["one", "thousand", "two", "hundred", "fifty"]
        );
        assert_eq!(words("100"), vec!["one", "hundred"]);
        assert_eq!(words("2000000"), vec!["2000000"]);
    }

    #[test]
    fn identical_text_has_zero_errors() {
        let w = word_errors(&words("a b c"), &words("a b c"));
        assert_eq!(w.errors(), 0);
        assert_eq!(w.wer(), 0.0);
    }

    #[test]
    fn counts_each_error_type() {
        // ref: a b c d   hyp: a x c d e  -> 1 substitution (b->x), 1 insertion (e).
        let w = word_errors(&words("a b c d"), &words("a x c d e"));
        assert_eq!(
            w,
            WerBreakdown {
                reference_words: 4,
                substitutions: 1,
                deletions: 0,
                insertions: 1,
            }
        );
        // ref: a b c d   hyp: a d -> 2 deletions.
        let w = word_errors(&words("a b c d"), &words("a d"));
        assert_eq!((w.substitutions, w.deletions, w.insertions), (0, 2, 0));
        assert_eq!(w.wer(), 0.5);
    }

    #[test]
    fn empty_sides_are_all_deletions_or_insertions() {
        let w = word_errors(&words("a b"), &[]);
        assert_eq!((w.deletions, w.errors()), (2, 2));
        let w = word_errors(&[], &words("a b"));
        assert_eq!((w.insertions, w.reference_words), (2, 0));
        assert_eq!(w.wer(), 2.0);
        assert_eq!(word_errors(&[], &[]).wer(), 0.0);
    }

    #[test]
    fn cpwer_is_zero_under_a_consistent_speaker_relabel() {
        let reference = by_speaker(&[("A", "hello there"), ("B", "general kenobi")]);
        let hypothesis = by_speaker(&[("2", "hello there"), ("1", "general kenobi")]);
        let result = cpwer(&reference, &hypothesis).unwrap();
        assert_eq!(result.breakdown.errors(), 0);
        assert!(result
            .assignment
            .contains(&(Some("A".to_string()), Some("2".to_string()))));
    }

    #[test]
    fn cpwer_charges_a_merged_speaker_as_deletions_and_insertions() {
        // One hypothesis speaker covers both people: it is matched to one reference stream (2 insertions)
        // and the other reference stream has no counterpart (2 deletions).
        let reference = by_speaker(&[("A", "one two"), ("B", "three four")]);
        let hypothesis = by_speaker(&[("1", "one two three four")]);
        let result = cpwer(&reference, &hypothesis).unwrap();
        assert_eq!(result.breakdown.reference_words, 4);
        assert_eq!(result.breakdown.errors(), 4);
    }

    #[test]
    fn cpwer_charges_a_split_speaker_as_deletions_and_insertions() {
        let reference = by_speaker(&[("A", "one two three four")]);
        let hypothesis = by_speaker(&[("1", "one two"), ("2", "three four")]);
        let result = cpwer(&reference, &hypothesis).unwrap();
        assert_eq!(result.breakdown.insertions, 2);
        assert_eq!(result.breakdown.deletions, 2);
    }

    #[test]
    fn cpwer_refuses_an_unbounded_speaker_count() {
        let many: BTreeMap<String, Vec<String>> = (0..=MAX_ASSIGNMENT)
            .map(|i| (i.to_string(), words("a")))
            .collect();
        assert!(cpwer(&many, &BTreeMap::new()).is_none());
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let p = percentiles(&[5.0, 1.0, 3.0, 2.0, 4.0, 10.0, 9.0, 8.0, 7.0, 6.0]).unwrap();
        assert_eq!(p.count, 10);
        assert_eq!(p.p50, 5.0);
        assert_eq!(p.p90, 9.0);
        assert_eq!(p.max, 10.0);
        assert!(percentiles(&[]).is_none());
    }
}

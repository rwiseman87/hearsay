//! Diarization accuracy metrics — pure logic (no ML, no I/O), unit-tested in isolation.
//!
//! Two families, both over [`SpeakerTurn`] lists (the same type the diarizer/refine already produce):
//! - **Speaker count** ([`speaker_count`], [`count_error`]) — the primary gate signal, needs no
//!   time-aligned truth, just the true speaker count.
//! - **Diarization Error Rate** ([`der`]) — the standard time-based score against a reference (RTTM),
//!   with the optimal reference<->hypothesis speaker mapping, a forgiveness collar around reference
//!   boundaries, and overlap scored. Reported as its [`DerBreakdown`] components so a regression is
//!   attributable to missed / false-alarm / confusion.
//!
//! Kept dependency-free and I/O-free so it is unit-tested directly (like `consolidate.rs` /
//! `voiceprint.rs`) and reused by both the committed accuracy gate and the diagnostic probe. There is
//! deliberately no Python scorer (`pyannote.metrics` / `dscore` / `md-eval`) in the loop.

use std::collections::BTreeSet;

use crate::mapping::SpeakerTurn;

/// Distinct speakers named across `turns`.
pub fn speaker_count(turns: &[SpeakerTurn]) -> usize {
    turns
        .iter()
        .map(|t| t.speaker.as_str())
        .collect::<BTreeSet<_>>()
        .len()
}

/// Signed speaker-count error `hypothesis - reference`: positive = over-split (too many speakers),
/// negative = over-merge (too few). Zero is exact. The sign is what makes both failure directions
/// distinguishable on the gate.
pub fn count_error(hypothesis: &[SpeakerTurn], reference: &[SpeakerTurn]) -> i64 {
    speaker_count(hypothesis) as i64 - speaker_count(reference) as i64
}

/// Time-based DER components, in seconds (not a ratio — divide by [`DerBreakdown::der`]).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DerBreakdown {
    /// Reference speech the hypothesis left unlabeled (`max(0, N_ref - N_sys)` per instant).
    pub missed: f64,
    /// Hypothesis speech over reference silence (`max(0, N_sys - N_ref)` per instant).
    pub false_alarm: f64,
    /// Time both speak but the hypothesis speaker is not the mapped reference speaker
    /// (`min(N_ref, N_sys) - N_correct` per instant).
    pub confusion: f64,
    /// Total reference speaker time over the scored region — the DER denominator.
    pub total: f64,
}

impl DerBreakdown {
    /// `(missed + false_alarm + confusion) / total`. Zero reference speech yields `0.0` when there is
    /// no error and `f64::INFINITY` otherwise (all hypothesis speech is false alarm with nothing to
    /// normalize against) — a degenerate input the accuracy gate never feeds.
    pub fn der(&self) -> f64 {
        let error = self.missed + self.false_alarm + self.confusion;
        if self.total <= 0.0 {
            return if error <= 0.0 { 0.0 } else { f64::INFINITY };
        }
        error / self.total
    }
}

/// Diarization Error Rate of `hypothesis` against `reference`, both as speaker turns (seconds,
/// same clock).
///
/// Standard NIST-style scoring: the optimal one-to-one reference<->hypothesis speaker mapping is
/// chosen to maximize correctly-attributed time, overlap is scored, and a `collar_s` forgiveness
/// window around every reference-turn boundary is excluded (`0.0` scores everything). Exact
/// interval sweep — the timeline is cut at every turn boundary and every collar edge, so each piece
/// has constant active-speaker sets and contributes `duration * component` with no frame
/// quantization.
pub fn der(reference: &[SpeakerTurn], hypothesis: &[SpeakerTurn], collar_s: f64) -> DerBreakdown {
    let ref_labels = distinct_labels(reference);
    let hyp_labels = distinct_labels(hypothesis);

    // Regions within `collar_s` of a reference boundary are not scored.
    let collar = collar_s.max(0.0);
    let no_score = collar_intervals(reference, collar);

    // Cut points: every turn boundary, plus both collar edges of every reference boundary.
    let mut cuts: Vec<f64> = Vec::new();
    for turn in reference.iter().chain(hypothesis) {
        cuts.push(turn.start_s);
        cuts.push(turn.end_s);
    }
    for turn in reference {
        for boundary in [turn.start_s, turn.end_s] {
            cuts.push(boundary - collar);
            cuts.push(boundary + collar);
        }
    }
    cuts.sort_by(f64::total_cmp);
    cuts.dedup();

    // `overlap[r][h]` accumulates scored time reference speaker `r` and hypothesis speaker `h` are
    // both active — the weight matrix the optimal mapping maximizes over.
    let mut overlap = vec![vec![0.0f64; hyp_labels.len()]; ref_labels.len()];
    let mut breakdown = DerBreakdown::default();
    let mut sum_min = 0.0f64;

    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let dur = end - start;
        if dur <= 0.0 {
            continue;
        }
        let mid = start + dur / 2.0;
        if is_no_score(mid, &no_score) {
            continue;
        }
        let ref_active = active_indices(mid, reference, &ref_labels);
        let hyp_active = active_indices(mid, hypothesis, &hyp_labels);
        let (n_ref, n_sys) = (ref_active.len() as f64, hyp_active.len() as f64);

        breakdown.total += n_ref * dur;
        breakdown.missed += (n_ref - n_sys).max(0.0) * dur;
        breakdown.false_alarm += (n_sys - n_ref).max(0.0) * dur;
        sum_min += n_ref.min(n_sys) * dur;
        for &r in &ref_active {
            for &h in &hyp_active {
                overlap[r][h] += dur;
            }
        }
    }

    // Confusion = matched-but-wrong = min(N_ref, N_sys) time minus the correctly-mapped time.
    let correct = best_matching(&overlap, 0, &mut vec![false; hyp_labels.len()]);
    breakdown.confusion = (sum_min - correct).max(0.0);
    breakdown
}

/// Distinct speaker labels in first-seen order (index into the overlap matrix).
fn distinct_labels(turns: &[SpeakerTurn]) -> Vec<String> {
    let mut labels: Vec<String> = Vec::new();
    for turn in turns {
        if !labels.iter().any(|l| l == &turn.speaker) {
            labels.push(turn.speaker.clone());
        }
    }
    labels
}

/// Indices (into `labels`) of speakers active at instant `t` (`start <= t < end`).
fn active_indices(t: f64, turns: &[SpeakerTurn], labels: &[String]) -> Vec<usize> {
    let mut active = BTreeSet::new();
    for turn in turns {
        if turn.start_s <= t && t < turn.end_s {
            if let Some(idx) = labels.iter().position(|l| l == &turn.speaker) {
                active.insert(idx);
            }
        }
    }
    active.into_iter().collect()
}

/// `[boundary - collar, boundary + collar]` around every reference boundary, unmerged (membership is
/// tested by point containment, so overlap between windows is harmless).
fn collar_intervals(reference: &[SpeakerTurn], collar: f64) -> Vec<(f64, f64)> {
    if collar <= 0.0 {
        return Vec::new();
    }
    let mut intervals = Vec::new();
    for turn in reference {
        for boundary in [turn.start_s, turn.end_s] {
            intervals.push((boundary - collar, boundary + collar));
        }
    }
    intervals
}

fn is_no_score(t: f64, no_score: &[(f64, f64)]) -> bool {
    no_score.iter().any(|&(lo, hi)| t >= lo && t <= hi)
}

/// Max total weight of an injective reference->hypothesis assignment (each reference speaker mapped
/// to a distinct hypothesis speaker, or left unmapped). Exhaustive over the small speaker counts
/// diarization produces; a reference speaker may skip when no hypothesis speaker is left or helps.
fn best_matching(overlap: &[Vec<f64>], row: usize, used: &mut Vec<bool>) -> f64 {
    if row == overlap.len() {
        return 0.0;
    }
    // Leave this reference speaker unmapped.
    let mut best = best_matching(overlap, row + 1, used);
    for col in 0..used.len() {
        if !used[col] {
            used[col] = true;
            let candidate = overlap[row][col] + best_matching(overlap, row + 1, used);
            used[col] = false;
            if candidate > best {
                best = candidate;
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(speaker: &str, start_s: f64, end_s: f64) -> SpeakerTurn {
        SpeakerTurn {
            speaker: speaker.to_string(),
            start_s,
            end_s,
        }
    }

    #[test]
    fn speaker_count_is_distinct_labels() {
        let turns = vec![
            turn("A", 0.0, 1.0),
            turn("B", 1.0, 2.0),
            turn("A", 2.0, 3.0),
        ];
        assert_eq!(speaker_count(&turns), 2);
        assert_eq!(speaker_count(&[]), 0);
    }

    #[test]
    fn count_error_is_signed() {
        let two = vec![turn("A", 0.0, 1.0), turn("B", 1.0, 2.0)];
        let three = vec![
            turn("A", 0.0, 1.0),
            turn("B", 1.0, 2.0),
            turn("C", 2.0, 3.0),
        ];
        // over-split: hypothesis has one more speaker than truth.
        assert_eq!(count_error(&three, &two), 1);
        // over-merge: hypothesis has one fewer.
        assert_eq!(count_error(&two, &three), -1);
        assert_eq!(count_error(&two, &two), 0);
    }

    #[test]
    fn der_is_zero_for_identical_diarization() {
        let reference = vec![turn("A", 0.0, 2.0), turn("B", 2.0, 4.0)];
        // Same timeline, different label strings — the mapping absorbs the renaming.
        let hypothesis = vec![turn("spk0", 0.0, 2.0), turn("spk1", 2.0, 4.0)];
        let b = der(&reference, &hypothesis, 0.0);
        assert_eq!(b.der(), 0.0);
        assert_eq!(b.total, 4.0);
    }

    #[test]
    fn der_consistent_label_swap_is_free() {
        // Two speakers whose labels are swapped for one another over the whole timeline is a
        // *consistent* renaming, which the optimal mapping absorbs -> DER 0, not confusion.
        let reference = vec![turn("A", 0.0, 2.0), turn("B", 2.0, 4.0)];
        let hypothesis = vec![turn("A", 2.0, 4.0), turn("B", 0.0, 2.0)];
        assert_eq!(der(&reference, &hypothesis, 0.0).der(), 0.0);
    }

    #[test]
    fn der_counts_confusion() {
        // Confusion needs an *inconsistent* mapping: one hypothesis speaker covers two reference
        // speakers, so only the mapped half is correct and the other half is confusion.
        let reference = vec![turn("A", 0.0, 2.0), turn("B", 2.0, 4.0)];
        let hypothesis = vec![turn("X", 0.0, 4.0)];
        let b = der(&reference, &hypothesis, 0.0);
        assert_eq!(b.missed, 0.0);
        assert_eq!(b.false_alarm, 0.0);
        // X maps to A or B (2s each); the unmapped 2s is confusion.
        assert!((b.confusion - 2.0).abs() < 1e-9);
        assert!((b.der() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn der_counts_missed_and_false_alarm() {
        // Reference: A speaks 0-4. Hypothesis: only labels 1-3 (missed 0-1 and 3-4), and adds a
        // spurious speaker 4-5 over reference silence (false alarm).
        let reference = vec![turn("A", 0.0, 4.0)];
        let hypothesis = vec![turn("A", 1.0, 3.0), turn("B", 4.0, 5.0)];
        let b = der(&reference, &hypothesis, 0.0);
        assert!((b.missed - 2.0).abs() < 1e-9);
        assert!((b.false_alarm - 1.0).abs() < 1e-9);
        assert_eq!(b.confusion, 0.0);
        assert!((b.total - 4.0).abs() < 1e-9);
    }

    #[test]
    fn collar_excludes_reference_boundaries() {
        // A 0-2, B 2-4 reference; hypothesis mislabels the instant around the 2.0 boundary. A collar
        // of 0.5 forgives [1.5, 2.5], erasing that error.
        let reference = vec![turn("A", 0.0, 2.0), turn("B", 2.0, 4.0)];
        let hypothesis = vec![turn("A", 0.0, 2.2), turn("B", 2.2, 4.0)];
        let scored_hard = der(&reference, &hypothesis, 0.0);
        assert!(scored_hard.der() > 0.0);
        let forgiven = der(&reference, &hypothesis, 0.5);
        assert_eq!(forgiven.der(), 0.0);
    }

    #[test]
    fn der_optimal_mapping_survives_extra_hypothesis_speaker() {
        // Reference has 2 speakers; hypothesis over-splits B's turn into B1/B2. The mapping keeps the
        // larger B fragment as the match, so only the smaller fragment scores as confusion.
        let reference = vec![turn("A", 0.0, 4.0), turn("B", 4.0, 10.0)];
        let hypothesis = vec![
            turn("A", 0.0, 4.0),
            turn("B1", 4.0, 9.0),
            turn("B2", 9.0, 10.0),
        ];
        let b = der(&reference, &hypothesis, 0.0);
        // A (4s) + B1 (5s) map correctly; the 1s B2 fragment is confusion.
        assert!((b.confusion - 1.0).abs() < 1e-9);
        assert_eq!(b.missed, 0.0);
        assert_eq!(b.false_alarm, 0.0);
    }
}

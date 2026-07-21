//! Post-clustering speaker consolidation — fold a diarizer's over-split clusters back together.
//!
//! sherpa-onnx's pyannote-3.0 + agglomerative pipeline (the cross-platform/Windows diarizer)
//! reliably over-splits: a known-2-speaker 6-minute recording came back as 6 clusters. Its
//! clustering works on short per-window embeddings, so a speaker whose voice shifts across the
//! meeting fragments. This pass runs *after* it, on the far more stable whole-speaker centroids
//! (each averaged over all of that cluster's audio), where the same speaker's fragments sit at
//! cosine ~0.9 and different speakers at ~0.2 — separable in a way the per-window vectors are not.
//!
//! Two rules, in order:
//! 1. **Merge** the pair of clusters with the widest margin over the similarity their evidence
//!    demands, until nothing clears its bar. The bar is not fixed: see [`attenuation`] — a centroid
//!    averaged over two seconds is a noisier estimate of the same voice than one averaged over two
//!    minutes, so holding both to the same cosine is simply the wrong test.
//! 2. **Drop** clusters that resemble no voice in the room — centroid cosine at or below
//!    `non_voice_ceiling` (0, i.e. orthogonal) against *every* other cluster. That is what
//!    non-speech looks like: a notification chime or room noise the ASR puts words on. Real
//!    speakers, however different, share the speech subspace and land well positive. Dropping loses
//!    no transcript: with the cluster's turns gone, the caller's overlap attribution hands its text
//!    to whoever was actually speaking around it.
//!
//! Deliberately absent: any "a cluster under N seconds cannot be a speaker" rule. It reads as a
//! tidy denoiser and is really a cliff that deletes a real participant who happened to speak for
//! N-minus-a-bit seconds, silently and with no signal that it happened. Duration belongs in this
//! pass as *evidence* — how much to trust a centroid — never as a right to exist.
//!
//! Pure (no ML, no I/O): it maps ordinals to ordinals given centroids + speech durations, so it is
//! unit-tested directly. Merged centroids are duration-weighted means of the L2-normalized inputs.

use std::collections::{BTreeSet, HashMap};

use crate::voiceprint::cosine;

/// Thresholds for [`consolidate_speakers`].
#[derive(Clone, Copy, Debug)]
pub struct ConsolidateConfig {
    /// Centroid cosine at which two *well-evidenced* clusters are taken to be the same speaker.
    /// Clusters with less audio behind them are held to a proportionally lower bar; see
    /// [`attenuation`].
    pub merge_threshold: f64,
    /// Shrinkage time constant for that scaling, in seconds. See [`attenuation`].
    pub evidence_s: f64,
    /// A cluster whose best cosine to any other cluster is at or below this resembles no voice
    /// present and is dropped as non-speech. `f64::NEG_INFINITY` disables the rule.
    pub non_voice_ceiling: f64,
}

/// Default merge threshold: measured on the sherpa path, same-speaker fragments land at 0.72-0.91
/// and distinct speakers at 0.19-0.56, so 0.65 sits in the gap with margin on both sides.
pub const DEFAULT_MERGE_THRESHOLD: f64 = 0.65;

/// Default shrinkage time constant (seconds). See [`attenuation`] for how it was fixed.
pub const DEFAULT_EVIDENCE_S: f64 = 3.0;

/// Default non-speech ceiling: zero, the origin — "points away from every voice in the room".
/// Chosen because it is the one value on the scale that is not a tuning choice.
pub const DEFAULT_NON_VOICE_CEILING: f64 = 0.0;

impl Default for ConsolidateConfig {
    fn default() -> Self {
        Self {
            merge_threshold: DEFAULT_MERGE_THRESHOLD,
            evidence_s: DEFAULT_EVIDENCE_S,
            non_voice_ceiling: DEFAULT_NON_VOICE_CEILING,
        }
    }
}

/// How much of [`ConsolidateConfig::merge_threshold`] a cluster backed by `speech_s` seconds of
/// audio is held to, in `(0, 1]`.
///
/// A centroid is a mean of per-chunk embeddings, so its error falls off with the audio behind it
/// (variance `~s²/t`) and its cosine against the same speaker's true centroid is attenuated by
/// roughly `sqrt(t / (t + k))`. Two seconds of a speaker genuinely *does* score lower against
/// themselves than two minutes does — so the bar shrinks with the evidence instead of the cluster
/// being disqualified for having little.
///
/// `k` (`evidence_s`) is the duration at which a centroid is worth `1/sqrt(2)` of a fully-evidenced
/// one. Fixed at 3 s from the measured operating point: the 2.2 s fragment that belongs to a real
/// speaker scores 0.52 against them and must merge (needs `k >= 1.2`), while the two genuinely
/// different speakers nearest each other score 0.56 across 13 s and 109 s and must not (needs
/// `k <= 4.4`). Both applied to a pair, so the bar reflects the weaker of the two centroids.
pub fn attenuation(speech_s: f64, evidence_s: f64) -> f64 {
    if evidence_s <= 0.0 {
        return 1.0;
    }
    let t = speech_s.max(0.0);
    (t / (t + evidence_s)).sqrt()
}

/// The result of [`consolidate_speakers`].
#[derive(Debug, Clone, Default)]
pub struct Consolidation {
    /// Old cluster ordinal -> the ordinal it now belongs to. Every input ordinal is a key; an
    /// unmerged one maps to itself. Callers remap their turns through this, then renumber.
    pub remap: HashMap<i64, i64>,
    /// Ordinals judged non-speech. The caller drops these turns entirely rather than remapping
    /// them, so the text over them reattributes to a real speaker. Never every cluster.
    pub dropped: BTreeSet<i64>,
    /// Merged centroid per surviving ordinal (L2-normalized). Keys are the remap's values, minus
    /// the dropped ones and any cluster that had no centroid to begin with.
    pub centroids: HashMap<i64, Vec<f32>>,
}

/// One in-progress merge group.
struct Group {
    /// Ordinals folded into this group; the smallest is the representative.
    members: Vec<i64>,
    centroid: Vec<f32>,
    /// Total speech seconds across the members — this group's evidence.
    weight: f64,
}

impl Group {
    fn representative(&self) -> i64 {
        self.members.iter().copied().min().unwrap_or_default()
    }
}

/// L2-normalize, or `None` for a zero / empty vector (nothing to point at).
fn normalize(vector: &[f32]) -> Option<Vec<f32>> {
    let norm = vector
        .iter()
        .map(|&v| f64::from(v) * f64::from(v))
        .sum::<f64>()
        .sqrt();
    if norm == 0.0 {
        return None;
    }
    Some(
        vector
            .iter()
            .map(|&v| (f64::from(v) / norm) as f32)
            .collect(),
    )
}

/// Duration-weighted mean of two normalized centroids, renormalized. Falls back to `a` if the sum
/// cancels out (antipodal centroids of equal weight), which cannot happen for real embeddings.
fn blend(a: &Group, b: &Group) -> Vec<f32> {
    if a.centroid.len() != b.centroid.len() {
        return if a.weight >= b.weight {
            a.centroid.clone()
        } else {
            b.centroid.clone()
        };
    }
    let total = a.weight + b.weight;
    // Equal weights when neither side has duration, so a merge never divides by zero.
    let (wa, wb) = if total > 0.0 {
        (a.weight / total, b.weight / total)
    } else {
        (0.5, 0.5)
    };
    let mixed: Vec<f32> = a
        .centroid
        .iter()
        .zip(&b.centroid)
        .map(|(&x, &y)| (f64::from(x) * wa + f64::from(y) * wb) as f32)
        .collect();
    normalize(&mixed).unwrap_or_else(|| a.centroid.clone())
}

/// Merge `src` into `dst` (both indices into `groups`), removing `src`.
fn absorb(groups: &mut Vec<Group>, dst: usize, src: usize) {
    let merged_centroid = blend(&groups[dst], &groups[src]);
    let moved = groups.remove(src);
    // `remove` shifts everything after `src` down by one.
    let dst = if src < dst { dst - 1 } else { dst };
    groups[dst].members.extend(moved.members);
    groups[dst].weight += moved.weight;
    groups[dst].centroid = merged_centroid;
}

/// Fold a diarizer's over-split clusters together on their whole-speaker centroids.
///
/// `centroids` is the raw per-cluster voiceprint by ordinal and `speech_s` each cluster's total
/// speech in seconds (absent = 0) — used to weight how far each centroid is trusted, not to
/// disqualify anyone. A cluster with no centroid, or an all-zero one, cannot be compared and is
/// always left standing on its own.
///
/// Merging is greedy on the pair with the widest margin over its own evidence-scaled bar, and stops
/// as soon as no pair clears one. The non-speech drop then runs over what survives, and never
/// empties the room: if every remaining cluster looks like non-speech, none is dropped.
pub fn consolidate_speakers(
    centroids: &HashMap<i64, Vec<f32>>,
    speech_s: &HashMap<i64, f64>,
    config: ConsolidateConfig,
) -> Consolidation {
    let mut remap: HashMap<i64, i64> = HashMap::new();
    let mut groups: Vec<Group> = Vec::new();

    // Deterministic order: ordinals ascending, so ties resolve the same way every run.
    let mut ordinals: Vec<i64> = centroids.keys().copied().collect();
    ordinals.sort_unstable();
    for ordinal in ordinals {
        remap.insert(ordinal, ordinal);
        // No usable centroid: nothing to compare against, so it keeps its own identity.
        if let Some(centroid) = normalize(&centroids[&ordinal]) {
            groups.push(Group {
                members: vec![ordinal],
                centroid,
                weight: speech_s.get(&ordinal).copied().unwrap_or(0.0).max(0.0),
            });
        }
    }

    // 1. Merge the pair that clears its bar by the widest margin, until none does.
    loop {
        let mut best: Option<(usize, usize, f64)> = None;
        for i in 0..groups.len() {
            for j in (i + 1)..groups.len() {
                let required = config.merge_threshold
                    * attenuation(groups[i].weight, config.evidence_s)
                    * attenuation(groups[j].weight, config.evidence_s);
                let margin = cosine(&groups[i].centroid, &groups[j].centroid) - required;
                if best.is_none_or(|(_, _, top)| margin > top) {
                    best = Some((i, j, margin));
                }
            }
        }
        match best {
            Some((i, j, margin)) if margin >= 0.0 => absorb(&mut groups, i, j),
            _ => break,
        }
    }

    // 2. Drop what resembles no remaining voice. Judged in one pass over the merged groups, so a
    //    cluster's verdict does not depend on which other non-speech cluster went first.
    let non_voice: Vec<usize> = (0..groups.len())
        .filter(|&i| {
            let best = (0..groups.len())
                .filter(|&j| j != i)
                .map(|j| cosine(&groups[i].centroid, &groups[j].centroid))
                .fold(f64::NEG_INFINITY, f64::max);
            // A lone cluster has nothing to be unlike, so it is never non-speech.
            best.is_finite() && best <= config.non_voice_ceiling
        })
        .collect();
    let mut dropped = BTreeSet::new();
    if non_voice.len() < groups.len() {
        for &i in &non_voice {
            dropped.extend(groups[i].members.iter().copied());
        }
        let remove: BTreeSet<usize> = non_voice.iter().copied().collect();
        let mut index = 0;
        groups.retain(|_| {
            let discard = remove.contains(&index);
            index += 1;
            !discard
        });
    }

    let mut merged_centroids = HashMap::new();
    for group in &groups {
        let representative = group.representative();
        for member in &group.members {
            remap.insert(*member, representative);
        }
        merged_centroids.insert(representative, group.centroid.clone());
    }

    Consolidation {
        remap,
        dropped,
        centroids: merged_centroids,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Merge-only: the non-speech rule off, so a test can exercise one rule at a time.
    fn merge_only(merge_threshold: f64, evidence_s: f64) -> ConsolidateConfig {
        ConsolidateConfig {
            merge_threshold,
            evidence_s,
            non_voice_ceiling: f64::NEG_INFINITY,
        }
    }

    fn centroids(entries: &[(i64, Vec<f32>)]) -> HashMap<i64, Vec<f32>> {
        entries.iter().cloned().collect()
    }

    fn durations(entries: &[(i64, f64)]) -> HashMap<i64, f64> {
        entries.iter().copied().collect()
    }

    /// Unit vector at `degrees` from the x-axis, for building centroids at a known cosine.
    fn at(degrees: f64) -> Vec<f32> {
        let radians = degrees.to_radians();
        vec![radians.cos() as f32, radians.sin() as f32]
    }

    #[test]
    fn merges_similar_clusters_and_keeps_distinct_ones() {
        let result = consolidate_speakers(
            &centroids(&[
                (1, vec![1.0, 0.0]),
                (2, vec![0.99, 0.14]),
                (3, vec![0.0, 1.0]),
            ]),
            &durations(&[(1, 60.0), (2, 40.0), (3, 50.0)]),
            merge_only(0.65, 3.0),
        );
        assert_eq!(result.remap[&1], 1);
        assert_eq!(result.remap[&2], 1, "near-identical centroid folds into 1");
        assert_eq!(result.remap[&3], 3, "orthogonal centroid stays its own");
        assert_eq!(result.centroids.len(), 2);
        assert!(result.dropped.is_empty());
    }

    #[test]
    fn leaves_well_evidenced_clusters_below_the_threshold_alone() {
        // 0.6 cosine, both backed by a minute of speech — the full bar applies and they stay apart.
        let result = consolidate_speakers(
            &centroids(&[(1, vec![1.0, 0.0]), (2, vec![0.6, 0.8])]),
            &durations(&[(1, 60.0), (2, 60.0)]),
            merge_only(0.65, 3.0),
        );
        assert_eq!(result.remap[&1], 1);
        assert_eq!(result.remap[&2], 2);
    }

    #[test]
    fn the_same_similarity_merges_when_one_side_is_barely_evidenced() {
        // Identical geometry to the test above; only the evidence differs. 2.2 s of audio is held
        // to 0.65 * sqrt(2.2/5.2) = 0.42, which 0.6 clears — this is the whole point of the pass.
        let result = consolidate_speakers(
            &centroids(&[(1, vec![1.0, 0.0]), (2, vec![0.6, 0.8])]),
            &durations(&[(1, 60.0), (2, 2.2)]),
            merge_only(0.65, 3.0),
        );
        assert_eq!(result.remap[&2], 1);
        assert_eq!(result.centroids.len(), 1);
    }

    #[test]
    fn a_short_cluster_unlike_everyone_is_not_forced_into_a_speaker() {
        // 2 s of audio at 0.3 cosine: under even the attenuated bar (0.65 * sqrt(2/5) = 0.41), so
        // it keeps its own identity. No duration rule exists to absorb it.
        let result = consolidate_speakers(
            &centroids(&[(1, vec![1.0, 0.0]), (2, at(72.5))]),
            &durations(&[(1, 60.0), (2, 2.0)]),
            merge_only(0.65, 3.0),
        );
        assert_eq!(result.remap[&2], 2);
        assert_eq!(result.centroids.len(), 2);
    }

    #[test]
    fn evidence_never_lowers_the_bar_below_a_real_speaker_gap() {
        // The measured worst case: two different speakers at 0.564, the nearer backed by 13.4 s.
        let result = consolidate_speakers(
            &centroids(&[(1, vec![1.0, 0.0]), (2, at(55.66))]),
            &durations(&[(1, 108.6), (2, 13.4)]),
            merge_only(0.65, 3.0),
        );
        assert_eq!(result.remap[&2], 2, "0.564 must not merge at 13.4 s");
    }

    #[test]
    fn drops_a_cluster_orthogonal_to_every_voice() {
        let result = consolidate_speakers(
            &centroids(&[
                (1, vec![1.0, 0.0, 0.0]),
                (2, vec![0.5, 0.866, 0.0]),
                (3, vec![0.0, 0.0, -1.0]),
            ]),
            &durations(&[(1, 108.6), (2, 148.0), (3, 0.5)]),
            ConsolidateConfig::default(),
        );
        assert!(result.dropped.contains(&3));
        assert!(!result.centroids.contains_key(&3));
        assert!(!result.dropped.contains(&1) && !result.dropped.contains(&2));
    }

    #[test]
    fn never_drops_every_cluster() {
        // Two mutually orthogonal clusters: each looks like non-speech to the other, but emptying
        // the room would delete the meeting, so nothing is dropped.
        let result = consolidate_speakers(
            &centroids(&[(1, vec![1.0, 0.0]), (2, vec![0.0, 1.0])]),
            &durations(&[(1, 60.0), (2, 60.0)]),
            ConsolidateConfig::default(),
        );
        assert!(result.dropped.is_empty());
        assert_eq!(result.centroids.len(), 2);
    }

    #[test]
    fn a_lone_cluster_is_never_non_speech() {
        let result = consolidate_speakers(
            &centroids(&[(1, vec![1.0, 0.0])]),
            &durations(&[(1, 0.4)]),
            ConsolidateConfig::default(),
        );
        assert!(result.dropped.is_empty());
        assert_eq!(result.remap[&1], 1);
    }

    #[test]
    fn a_cluster_without_a_usable_centroid_keeps_its_identity() {
        let result = consolidate_speakers(
            &centroids(&[
                (1, vec![1.0, 0.0]),
                (2, vec![0.0, 0.0]),
                (3, vec![0.99, 0.14]),
            ]),
            &durations(&[(1, 60.0), (2, 1.0), (3, 30.0)]),
            merge_only(0.65, 3.0),
        );
        assert_eq!(result.remap[&3], 1);
        assert_eq!(result.remap[&2], 2, "zero centroid is never merged");
        assert!(!result.centroids.contains_key(&2));
        assert!(!result.dropped.contains(&2));
    }

    #[test]
    fn merged_centroid_is_normalized_and_duration_weighted() {
        let result = consolidate_speakers(
            &centroids(&[(1, vec![1.0, 0.0]), (2, vec![0.8, 0.6])]),
            &durations(&[(1, 90.0), (2, 10.0)]),
            merge_only(0.65, 3.0),
        );
        let merged = &result.centroids[&1];
        assert!((cosine(merged, merged) - 1.0).abs() < 1e-6, "normalized");
        assert!(
            cosine(merged, &[1.0, 0.0]) > cosine(merged, &[0.8, 0.6]),
            "pulled toward the longer-speaking cluster"
        );
    }

    #[test]
    fn empty_input_is_a_no_op() {
        let result = consolidate_speakers(
            &HashMap::new(),
            &HashMap::new(),
            ConsolidateConfig::default(),
        );
        assert!(result.remap.is_empty());
        assert!(result.centroids.is_empty());
        assert!(result.dropped.is_empty());
    }

    #[test]
    fn chains_transitively_through_the_widest_margin_first() {
        let result = consolidate_speakers(
            &centroids(&[
                (1, vec![1.0, 0.0]),
                (2, vec![0.99, 0.14]),
                (3, vec![0.71, 0.71]),
            ]),
            &durations(&[(1, 30.0), (2, 30.0), (3, 30.0)]),
            merge_only(0.65, 3.0),
        );
        assert_eq!(result.remap[&2], 1);
        assert_eq!(result.remap[&3], 1);
        assert_eq!(result.centroids.len(), 1);
    }

    #[test]
    fn attenuation_rises_with_evidence_and_is_disabled_at_zero() {
        assert_eq!(attenuation(10.0, 0.0), 1.0, "disabled");
        assert_eq!(attenuation(0.0, 3.0), 0.0, "no evidence, no bar");
        assert!((attenuation(3.0, 3.0) - 0.5_f64.sqrt()).abs() < 1e-12);
        assert!(attenuation(2.0, 3.0) < attenuation(20.0, 3.0));
        assert!(attenuation(600.0, 3.0) > 0.99, "approaches 1");
    }
}

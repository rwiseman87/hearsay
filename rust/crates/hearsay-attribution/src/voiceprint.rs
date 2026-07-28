//! Voiceprint (speaker-embedding centroid) serialization + cross-meeting matching.
//!
//! A centroid is a fixed-length float vector stored as little-endian float32 bytes on a cluster
//! (`clusters.centroid`); once that cluster is named and locked it becomes a recognizable
//! voiceprint, so a later meeting can match a new speaker to it by cosine similarity.

/// Serialize a centroid as little-endian float32 bytes for `clusters.centroid`.
pub fn centroid_to_bytes(centroid: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(centroid.len() * 4);
    for value in centroid {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// Inverse of [`centroid_to_bytes`]. Trailing bytes that do not form a whole f32 are ignored.
pub fn centroid_from_bytes(data: &[u8]) -> Vec<f32> {
    data.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Cosine similarity of two vectors, computed in f64.
///
/// Returns `0.0` if the vectors' lengths differ (e.g. a model change) or either vector has zero
/// norm, so a length mismatch is a safe non-match in every build profile rather than a garbage
/// score.
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return 0.0;
    }
    let na = a
        .iter()
        .map(|&x| f64::from(x) * f64::from(x))
        .sum::<f64>()
        .sqrt();
    let nb = b
        .iter()
        .map(|&x| f64::from(x) * f64::from(x))
        .sum::<f64>()
        .sqrt();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    let dot: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| f64::from(x) * f64::from(y))
        .sum();
    dot / (na * nb)
}

/// Name of the known voiceprint most similar to `centroid` with cosine `>= threshold`.
///
/// `None` if nothing clears the threshold. Voiceprints of a different length (a model change) are
/// skipped rather than compared. On ties the earliest-listed match wins.
pub fn match_identity<'a>(
    centroid: &[f32],
    known: &'a [(String, Vec<f32>)],
    threshold: f64,
) -> Option<&'a str> {
    best_identity(centroid, known, threshold).map(|(name, _score)| name)
}

/// Like [`match_identity`], but also returns the winning cosine score so a caller can rank or dedup
/// competing matches (e.g. keep the highest-scoring ordinal when two clusters match the same
/// person). `None` if nothing clears `threshold`.
pub fn best_identity<'a>(
    centroid: &[f32],
    known: &'a [(String, Vec<f32>)],
    threshold: f64,
) -> Option<(&'a str, f64)> {
    // Hoist the query norm out of the per-voiceprint loop: `cosine` recomputes it for every stored
    // voiceprint even though it is the same across all of them, and each candidate then folds its own
    // norm and the dot product into a single pass. N (all locked speakers ever seen) grows over time,
    // so the redundant passes compound. The per-candidate score is byte-for-byte what `cosine` returns.
    let na = norm(centroid);
    let mut best: Option<(&'a str, f64)> = None;
    for (name, vector) in known {
        if vector.len() != centroid.len() {
            continue;
        }
        let (nb, dot) = norm_and_dot(centroid, vector);
        let score = if na == 0.0 || nb == 0.0 {
            0.0
        } else {
            dot / (na * nb)
        };
        if score >= threshold && score > best.map_or(-1.0, |(_, s)| s) {
            best = Some((name.as_str(), score));
        }
    }
    best
}

/// L2 norm of a vector, computed in f64 (matches [`cosine`]'s convention).
fn norm(v: &[f32]) -> f64 {
    v.iter()
        .map(|&x| f64::from(x) * f64::from(x))
        .sum::<f64>()
        .sqrt()
}

/// Single pass over two equal-length vectors yielding `(‖b‖, a·b)` in f64. The caller hoists `‖a‖`.
fn norm_and_dot(a: &[f32], b: &[f32]) -> (f64, f64) {
    let mut sum_sq_b = 0.0_f64;
    let mut dot = 0.0_f64;
    for (&x, &y) in a.iter().zip(b) {
        let y = f64::from(y);
        sum_sq_b += y * y;
        dot += f64::from(x) * y;
    }
    (sum_sq_b.sqrt(), dot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centroid_bytes_roundtrip() {
        let centroid = vec![0.0_f32, 1.0, -1.0, 0.5];
        let bytes = centroid_to_bytes(&centroid);
        assert_eq!(bytes.len(), centroid.len() * 4);
        assert_eq!(centroid_from_bytes(&bytes), centroid);
    }

    #[test]
    fn cosine_identical_orthogonal_and_zero() {
        assert!((cosine(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]) - 1.0).abs() < 1e-12);
        assert!((cosine(&[1.0, 0.0], &[0.0, 1.0])).abs() < 1e-12);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn cosine_length_mismatch_returns_zero() {
        assert!((cosine(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]) - 1.0).abs() < 1e-12);
        assert_eq!(cosine(&[1.0, 2.0, 3.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine(&[1.0, 2.0], &[1.0, 2.0, 3.0]), 0.0);
        assert_eq!(cosine(&[], &[1.0]), 0.0);
    }

    #[test]
    fn match_identity_picks_best_above_threshold() {
        let known = vec![
            ("alice".to_string(), vec![1.0, 0.0, 0.0]),
            ("bob".to_string(), vec![0.0, 1.0, 0.0]),
        ];
        assert_eq!(match_identity(&[0.9, 0.1, 0.0], &known, 0.5), Some("alice"));
    }

    #[test]
    fn match_identity_none_below_threshold() {
        let known = vec![("alice".to_string(), vec![1.0, 0.0])];
        assert_eq!(match_identity(&[0.0, 1.0], &known, 0.5), None);
    }

    #[test]
    fn match_identity_skips_different_length() {
        let known = vec![
            ("wrong_dim".to_string(), vec![1.0, 0.0, 0.0, 0.0]),
            ("right_dim".to_string(), vec![1.0, 0.0, 0.0]),
        ];
        assert_eq!(
            match_identity(&[1.0, 0.0, 0.0], &known, 0.5),
            Some("right_dim")
        );
    }

    #[test]
    fn best_identity_returns_the_winner_cosine_score() {
        // Guards the hoisted-norm optimization: best_identity must return exactly what the untouched
        // `cosine` computes for the winner, and pick the highest-scoring candidate.
        let known = vec![
            ("alice".to_string(), vec![1.0_f32, 0.0, 0.0]),
            ("bob".to_string(), vec![0.2, 0.9, 0.1]),
        ];
        let query = vec![0.8_f32, 0.3, 0.1];
        let (name, score) = best_identity(&query, &known, 0.0).unwrap();
        let winner = &known.iter().find(|(n, _)| n == name).unwrap().1;
        assert_eq!(score, cosine(&query, winner));
        for (_, vector) in &known {
            assert!(score >= cosine(&query, vector));
        }
    }

    #[test]
    fn match_identity_tie_goes_to_first() {
        let known = vec![
            ("first".to_string(), vec![1.0, 0.0]),
            ("second".to_string(), vec![1.0, 0.0]),
        ];
        assert_eq!(match_identity(&[1.0, 0.0], &known, 0.5), Some("first"));
    }

    use proptest::prelude::*;

    // Finite, bounded floats so cosine stays well-defined (no NaN / inf).
    fn vec_strategy() -> impl Strategy<Value = Vec<f32>> {
        prop::collection::vec(-1000.0f32..1000.0, 1..64)
    }

    proptest! {
        // Cosine is symmetric and stays within [-1, 1] (modulo fp slack) for equal-length inputs.
        #[test]
        fn cosine_is_symmetric_and_bounded(a in vec_strategy(), b in vec_strategy()) {
            let n = a.len().min(b.len());
            let (a, b) = (&a[..n], &b[..n]);
            let ab = cosine(a, b);
            prop_assert!((cosine(b, a) - ab).abs() < 1e-9);
            prop_assert!((-1.0 - 1e-6..=1.0 + 1e-6).contains(&ab), "out of range: {ab}");
        }

        // A non-zero vector is maximally similar to itself.
        #[test]
        fn cosine_self_is_one_for_nonzero(v in vec_strategy()) {
            let norm: f64 = v.iter().map(|&x| f64::from(x) * f64::from(x)).sum();
            prop_assume!(norm > 1e-3);
            prop_assert!((cosine(&v, &v) - 1.0).abs() < 1e-4);
        }
    }
}

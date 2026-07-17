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
    let mut best_name: Option<&'a str> = None;
    let mut best_score = -1.0_f64;
    for (name, vector) in known {
        if vector.len() != centroid.len() {
            continue;
        }
        let score = cosine(centroid, vector);
        if score >= threshold && score > best_score {
            best_score = score;
            best_name = Some(name.as_str());
        }
    }
    best_name
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
    fn match_identity_tie_goes_to_first() {
        let known = vec![
            ("first".to_string(), vec![1.0, 0.0]),
            ("second".to_string(), vec![1.0, 0.0]),
        ];
        assert_eq!(match_identity(&[1.0, 0.0], &known, 0.5), Some("first"));
    }
}

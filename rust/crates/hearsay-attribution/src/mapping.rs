//! Diarization mapping helpers: diarizer speaker labels -> "Speaker N" ordinals, and per-segment
//! speaker assignment by turn overlap.

use std::collections::HashMap;

/// One contiguous span attributed to a single speaker (seconds, track-relative).
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerTurn {
    pub speaker: String,
    pub start_s: f64,
    pub end_s: f64,
}

/// Map each diarizer speaker label to a 1-based "Speaker N" ordinal by first appearance, where
/// "first" is the smallest `start_s` (ties keep the input order).
pub fn order_speakers(turns: &[SpeakerTurn]) -> HashMap<String, u32> {
    let mut ordered: Vec<(&str, f64)> = turns
        .iter()
        .map(|t| (t.speaker.as_str(), t.start_s))
        .collect();
    ordered.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut ordinal: HashMap<String, u32> = HashMap::new();
    for (speaker, _) in ordered {
        if !ordinal.contains_key(speaker) {
            let next = ordinal.len() as u32 + 1;
            ordinal.insert(speaker.to_string(), next);
        }
    }
    ordinal
}

/// Index of the turn most overlapping segment `[start_s, end_s]`, or `None` if none overlaps.
///
/// Turn times are track-relative; `offset_s` shifts them onto the meeting clock the segment
/// timestamps use. On a tie the earliest turn wins. Index-returning so a caller keying turns by
/// something other than the label (e.g. a numeric ordinal) can map the result onto its own list.
fn max_overlap_turn(
    start_s: f64,
    end_s: f64,
    turns: &[SpeakerTurn],
    offset_s: f64,
) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut best_overlap = 0.0_f64;
    for (i, turn) in turns.iter().enumerate() {
        let overlap =
            f64::min(end_s, turn.end_s + offset_s) - f64::max(start_s, turn.start_s + offset_s);
        if overlap > best_overlap {
            best_overlap = overlap;
            best = Some(i);
        }
    }
    best
}

/// Speaker label whose turn most overlaps segment `[start_s, end_s]` (meeting time).
///
/// Turn times are track-relative; `offset_s` shifts them onto the meeting clock the segment
/// timestamps use. `None` if no turn overlaps.
pub fn assign_segment_speaker(
    start_s: f64,
    end_s: f64,
    turns: &[SpeakerTurn],
    offset_s: f64,
) -> Option<&str> {
    max_overlap_turn(start_s, end_s, turns, offset_s).map(|i| turns[i].speaker.as_str())
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
    fn order_speakers_by_first_appearance() {
        // Out of order; "A" first appears at 0, "B" at 2.
        let turns = vec![
            turn("B", 5.0, 6.0),
            turn("A", 0.0, 1.0),
            turn("B", 2.0, 3.0),
        ];
        let ordinals = order_speakers(&turns);
        assert_eq!(ordinals.get("A"), Some(&1));
        assert_eq!(ordinals.get("B"), Some(&2));
        assert_eq!(ordinals.len(), 2);
    }

    #[test]
    fn order_speakers_empty() {
        assert!(order_speakers(&[]).is_empty());
    }

    #[test]
    fn assign_picks_max_overlap() {
        let turns = vec![turn("A", 0.0, 5.0), turn("B", 4.0, 10.0)];
        // [3,6]: A overlap = 5-3 = 2; B overlap = 6-4 = 2; tie -> first (A).
        assert_eq!(assign_segment_speaker(3.0, 6.0, &turns, 0.0), Some("A"));
        // [6,9]: A overlap = 5-6 < 0; B overlap = 9-6 = 3 -> B.
        assert_eq!(assign_segment_speaker(6.0, 9.0, &turns, 0.0), Some("B"));
    }

    #[test]
    fn max_overlap_turn_returns_index_and_breaks_ties_to_earliest() {
        let turns = vec![turn("A", 0.0, 5.0), turn("B", 4.0, 10.0)];
        // [3,6]: A overlap = 2, B overlap = 2; tie -> earliest index 0.
        assert_eq!(max_overlap_turn(3.0, 6.0, &turns, 0.0), Some(0));
        // [6,9]: only B overlaps -> index 1.
        assert_eq!(max_overlap_turn(6.0, 9.0, &turns, 0.0), Some(1));
        // No overlap -> None.
        assert_eq!(max_overlap_turn(100.0, 101.0, &turns, 0.0), None);
    }

    #[test]
    fn assign_applies_offset() {
        let turns = vec![turn("A", 0.0, 2.0)];
        // Turn shifted by +10 -> [10,12], segment [10,12] fully overlaps.
        assert_eq!(assign_segment_speaker(10.0, 12.0, &turns, 10.0), Some("A"));
        // Without the offset the turn is at [0,2], no overlap with [10,12].
        assert_eq!(assign_segment_speaker(10.0, 12.0, &turns, 0.0), None);
    }

    #[test]
    fn assign_none_when_no_overlap() {
        let turns = vec![turn("A", 0.0, 5.0)];
        assert_eq!(assign_segment_speaker(100.0, 101.0, &turns, 0.0), None);
    }
}

//! Scores the Parakeet batch ASR the `hearsay-diarize` sidecar can run (`--asr <model>`): parses its
//! words and speaker turns and attributes each word to a speaker, so the transcript can be compared
//! with the whisper refine on the same metrics.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Turn {
    pub speaker: String,
    pub start_s: f64,
    pub end_s: f64,
}

#[derive(Debug, Deserialize)]
pub struct Word {
    pub word: String,
    pub start_s: f64,
    pub end_s: f64,
}

#[derive(Debug, Deserialize)]
pub struct Asr {
    pub processing_s: f64,
    pub words: Vec<Word>,
}

#[derive(Debug, Deserialize)]
pub struct SidecarOutput {
    pub turns: Vec<Turn>,
    pub asr: Option<Asr>,
}

/// Run `hearsay-diarize <wav> --asr <model>` and parse its JSON.
pub fn run(binary: &Path, wav: &Path, model: &str) -> Result<SidecarOutput, String> {
    let output = Command::new(binary)
        .arg(wav)
        .args(["--asr", model])
        .output()
        .map_err(|e| format!("spawn {}: {e}", binary.display()))?;
    if !output.status.success() {
        return Err(format!(
            "hearsay-diarize --asr {model} failed: {}",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .last()
                .unwrap_or("")
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("parse sidecar output: {e}"))
}

/// The speaker label of the turn that overlaps `[start_s, end_s]` most; with no overlap, the turn
/// nearest the word's midpoint. `None` only when there are no turns.
fn speaker_for(turns: &[Turn], start_s: f64, end_s: f64) -> Option<&str> {
    let mut best: Option<(&Turn, f64)> = None;
    for turn in turns {
        let overlap = end_s.min(turn.end_s) - start_s.max(turn.start_s);
        if overlap > 0.0 && best.is_none_or(|(_, o)| overlap > o) {
            best = Some((turn, overlap));
        }
    }
    if let Some((turn, _)) = best {
        return Some(&turn.speaker);
    }
    let middle = (start_s + end_s) / 2.0;
    turns
        .iter()
        .min_by(|a, b| distance(a, middle).total_cmp(&distance(b, middle)))
        .map(|t| t.speaker.as_str())
}

fn distance(turn: &Turn, point: f64) -> f64 {
    if point < turn.start_s {
        turn.start_s - point
    } else if point > turn.end_s {
        point - turn.end_s
    } else {
        0.0
    }
}

/// Words in time order, and the same words grouped under the speaker each was attributed to.
pub fn attribute(words: &[Word], turns: &[Turn]) -> (Vec<String>, BTreeMap<String, Vec<String>>) {
    let mut ordered: Vec<&Word> = words.iter().collect();
    ordered.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
    let mut merged = Vec::with_capacity(ordered.len());
    let mut by_speaker: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for word in ordered {
        let normalized = hearsay_attribution::normalize(&word.word);
        if normalized.is_empty() {
            continue;
        }
        merged.extend(normalized.iter().cloned());
        let speaker = speaker_for(turns, word.start_s, word.end_s).unwrap_or("?");
        by_speaker
            .entry(speaker.to_string())
            .or_default()
            .extend(normalized);
    }
    (merged, by_speaker)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(speaker: &str, start_s: f64, end_s: f64) -> Turn {
        Turn {
            speaker: speaker.to_string(),
            start_s,
            end_s,
        }
    }

    fn word(text: &str, start_s: f64, end_s: f64) -> Word {
        Word {
            word: text.to_string(),
            start_s,
            end_s,
        }
    }

    #[test]
    fn a_word_goes_to_the_turn_it_overlaps_most() {
        let turns = vec![turn("S1", 0.0, 5.0), turn("S2", 4.0, 10.0)];
        assert_eq!(speaker_for(&turns, 3.0, 4.5), Some("S1"));
        assert_eq!(speaker_for(&turns, 6.0, 7.0), Some("S2"));
    }

    #[test]
    fn a_word_in_a_gap_goes_to_the_nearest_turn() {
        let turns = vec![turn("S1", 0.0, 2.0), turn("S2", 10.0, 12.0)];
        assert_eq!(speaker_for(&turns, 3.0, 3.4), Some("S1"));
        assert_eq!(speaker_for(&turns, 8.0, 8.4), Some("S2"));
        assert_eq!(speaker_for(&[], 1.0, 2.0), None);
    }

    #[test]
    fn attribute_orders_by_time_and_normalizes() {
        let turns = vec![turn("S1", 0.0, 3.0), turn("S2", 3.0, 6.0)];
        let words = vec![
            word("There.", 4.0, 4.5),
            word("Hello,", 0.5, 1.0),
            word("uh", 1.1, 1.2),
            word("25", 5.0, 5.5),
        ];
        let (merged, by_speaker) = attribute(&words, &turns);
        assert_eq!(merged, vec!["hello", "there", "twenty", "five"]);
        assert_eq!(by_speaker["S1"], vec!["hello"]);
        assert_eq!(by_speaker["S2"], vec!["there", "twenty", "five"]);
    }
}

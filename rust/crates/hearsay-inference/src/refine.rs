//! Post-meeting offline refine: re-diarize the Them track (reusing the Swift `hearsay-diarize`
//! FluidAudio sidecar — the same offline diarizer the live path's sidecars come from) and
//! re-transcribe each speaker turn with whisper → accurate `Speaker N` segments. Port of the
//! diarize + re-transcribe core of `src/hearsay/transcript/refine.py`.
//!
//! [`refine_them`] also returns each speaker's voiceprint (from the diarizer's per-speaker mean
//! embedding); persistence, cross-meeting recognition, and carry-forward of locked manual labels
//! all live in `hearsay_db::replace_them_segments`, which both this refine's callers go through.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use crate::asr::WhisperAsr;
use crate::error::InferenceError;

/// Contract-fixed track sample rate (Hz).
const SAMPLE_RATE: u32 = 16_000;

/// One refined Them segment: a diarizer turn re-transcribed, tagged with its 1-based speaker
/// ordinal (`Speaker {ordinal}`).
#[derive(Debug, Clone, PartialEq)]
pub struct RefinedSegment {
    pub ordinal: i64,
    pub text: String,
    pub start_s: f64,
    pub end_s: f64,
}

/// The refine's full output: the re-transcribed `Speaker N` segments + each speaker's L2-normalized
/// voiceprint by 1-based ordinal (for cross-meeting recognition + storage). `centroids` is empty
/// when the diarizer emits no embeddings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RefineOutput {
    pub segments: Vec<RefinedSegment>,
    pub centroids: HashMap<i64, Vec<f32>>,
}

#[derive(Deserialize)]
struct DiarizeOutput {
    turns: Vec<DiarizeTurn>,
    /// Per-speaker mean voiceprint (FluidAudio's speaker database); absent for a model that emits
    /// none, so default to empty rather than fail the parse.
    #[serde(default)]
    speakers: Vec<SpeakerEmbedding>,
}

#[derive(Deserialize)]
struct DiarizeTurn {
    speaker: String,
    start_s: f64,
    end_s: f64,
}

#[derive(Deserialize)]
struct SpeakerEmbedding {
    speaker: String,
    embedding: Vec<f32>,
}

/// Re-diarize + re-transcribe the Them track. `diarize_binary` is the Swift `hearsay-diarize`
/// sidecar; `them_samples` is the 16 kHz mono right channel of `audio.wav`. Blocking (whisper +
/// subprocess) — call via `spawn_blocking` from async code.
pub fn refine_them(
    asr: &WhisperAsr,
    diarize_binary: &Path,
    them_samples: &[f32],
) -> Result<RefineOutput, InferenceError> {
    // hearsay-diarize is file-based: write the Them track to a temp wav.
    let tmp = tempfile::Builder::new().suffix(".wav").tempfile()?;
    write_mono_wav(tmp.path(), them_samples)?;

    let output = Command::new(diarize_binary)
        .arg(tmp.path())
        .output()
        .map_err(|e| InferenceError::Whisper(format!("spawn hearsay-diarize: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // FluidAudio reports a silent / no-remote-speech track as an error; that is benign for a
        // refine (there is simply nothing to re-diarize), so surface it as a distinct variant the
        // caller can treat as a no-op rather than a failure.
        if stderr.contains("noSpeechDetected") {
            return Err(InferenceError::NoSpeech);
        }
        return Err(InferenceError::Whisper(format!(
            "hearsay-diarize failed: {}",
            stderr.trim()
        )));
    }
    let diarized: DiarizeOutput = serde_json::from_slice(&output.stdout)
        .map_err(|e| InferenceError::Whisper(format!("parse diarize output: {e}")))?;

    // Diarizer speaker label -> 1-based ordinal by first appearance (Python `order_speakers`).
    let mut turns = diarized.turns;
    turns.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
    let mut ordinal: HashMap<String, i64> = HashMap::new();
    for turn in &turns {
        if !ordinal.contains_key(&turn.speaker) {
            let next = ordinal.len() as i64 + 1;
            ordinal.insert(turn.speaker.clone(), next);
        }
    }

    // Re-transcribe each turn's slice of the Them track.
    let mut segments = Vec::with_capacity(turns.len());
    for turn in &turns {
        let start = (turn.start_s * SAMPLE_RATE as f64).max(0.0) as usize;
        let end = ((turn.end_s * SAMPLE_RATE as f64) as usize).min(them_samples.len());
        if end <= start {
            continue;
        }
        let text = asr
            .transcribe(&them_samples[start..end])?
            .into_iter()
            .map(|s| s.text)
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string();
        if text.is_empty() {
            continue;
        }
        segments.push(RefinedSegment {
            ordinal: ordinal[&turn.speaker],
            text,
            start_s: turn.start_s,
            end_s: turn.end_s,
        });
    }

    let centroids = build_centroids(&ordinal, &diarized.speakers);
    Ok(RefineOutput {
        segments,
        centroids,
    })
}

/// L2-normalize each speaker's embedding and key it by its 1-based ordinal (unknown speakers or
/// empty embeddings are skipped). Port of `refine.py::_recognize_speakers`'s centroid step.
fn build_centroids(
    ordinal: &HashMap<String, i64>,
    speakers: &[SpeakerEmbedding],
) -> HashMap<i64, Vec<f32>> {
    let mut centroids = HashMap::new();
    for speaker in speakers {
        let Some(&ord) = ordinal.get(&speaker.speaker) else {
            continue;
        };
        if let Some(centroid) = l2_normalize(&speaker.embedding) {
            centroids.insert(ord, centroid);
        }
    }
    centroids
}

/// Unit-length a voiceprint so stored centroids match the cosine convention (norm computed in f64,
/// matching the Python path). `None` for an empty vector; a zero vector is returned unchanged.
fn l2_normalize(vector: &[f32]) -> Option<Vec<f32>> {
    if vector.is_empty() {
        return None;
    }
    let norm = vector
        .iter()
        .map(|&v| f64::from(v) * f64::from(v))
        .sum::<f64>()
        .sqrt();
    if norm > 0.0 {
        Some(
            vector
                .iter()
                .map(|&v| (f64::from(v) / norm) as f32)
                .collect(),
        )
    } else {
        Some(vector.to_vec())
    }
}

/// Refine a recorded meeting's `audio.wav` end-to-end: read the Them (right) channel, load the
/// whisper model, then re-diarize (`diarize_binary`) + re-transcribe. The single ML entry point
/// shared by the manual `/rediarize` route and the orchestrator's auto-refine-at-stop. Blocking
/// (whisper + subprocess) — call via `spawn_blocking` from async code.
pub fn refine_audio_file(
    audio_path: &Path,
    diarize_binary: &Path,
    model: &Path,
) -> Result<RefineOutput, InferenceError> {
    let them = crate::audio::read_them_channel(audio_path)?;
    let asr = WhisperAsr::load(model)?;
    refine_them(&asr, diarize_binary, &them)
}

fn write_mono_wav(path: &Path, samples: &[f32]) -> Result<(), InferenceError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .map_err(|e| InferenceError::Audio(format!("create temp wav: {e}")))?;
    for &sample in samples {
        let pcm = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        writer
            .write_sample(pcm)
            .map_err(|e| InferenceError::Audio(format!("write temp wav: {e}")))?;
    }
    writer
        .finalize()
        .map_err(|e| InferenceError::Audio(format!("finalize temp wav: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_unit_lengths_and_handles_edges() {
        let unit = l2_normalize(&[3.0, 4.0]).unwrap();
        assert!((unit[0] - 0.6).abs() < 1e-6 && (unit[1] - 0.8).abs() < 1e-6);
        // A zero vector is returned unchanged; an empty vector is dropped.
        assert_eq!(l2_normalize(&[0.0, 0.0]), Some(vec![0.0, 0.0]));
        assert_eq!(l2_normalize(&[]), None);
    }

    #[test]
    fn build_centroids_keys_by_ordinal_and_skips_unknown() {
        let ordinal = HashMap::from([("A".to_string(), 1_i64), ("B".to_string(), 2_i64)]);
        let speakers = vec![
            SpeakerEmbedding {
                speaker: "A".into(),
                embedding: vec![3.0, 4.0],
            },
            SpeakerEmbedding {
                speaker: "B".into(),
                embedding: vec![], // empty -> skipped
            },
            SpeakerEmbedding {
                speaker: "C".into(), // not a diarized ordinal -> skipped
                embedding: vec![1.0],
            },
        ];
        let centroids = build_centroids(&ordinal, &speakers);
        assert_eq!(centroids.len(), 1);
        let a = &centroids[&1];
        assert!((a[0] - 0.6).abs() < 1e-6 && (a[1] - 0.8).abs() < 1e-6);
    }
}

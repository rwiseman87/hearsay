//! Post-meeting offline refine: re-diarize the Them track (reusing the Swift `hearsay-diarize`
//! FluidAudio sidecar — the same offline diarizer the live path's sidecars come from) and
//! re-transcribe each speaker turn with whisper → accurate `Speaker N` segments. Port of the
//! diarize + re-transcribe core of `src/hearsay/transcript/refine.py`.
//!
//! Carry-forward of locked manual labels (so a re-diarize never drops a rename) lives in
//! `hearsay_db::replace_them_segments`, which both this refine's callers persist through. Deferred
//! follow-up (as in the Python, tracked in `docs/TODO.md`): storing each speaker's voiceprint (the
//! diarizer also returns embeddings) for cross-meeting recognition.

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

#[derive(Deserialize)]
struct DiarizeOutput {
    turns: Vec<DiarizeTurn>,
}

#[derive(Deserialize)]
struct DiarizeTurn {
    speaker: String,
    start_s: f64,
    end_s: f64,
}

/// Re-diarize + re-transcribe the Them track. `diarize_binary` is the Swift `hearsay-diarize`
/// sidecar; `them_samples` is the 16 kHz mono right channel of `audio.wav`. Blocking (whisper +
/// subprocess) — call via `spawn_blocking` from async code.
pub fn refine_them(
    asr: &WhisperAsr,
    diarize_binary: &Path,
    them_samples: &[f32],
) -> Result<Vec<RefinedSegment>, InferenceError> {
    // hearsay-diarize is file-based: write the Them track to a temp wav.
    let tmp = tempfile::Builder::new().suffix(".wav").tempfile()?;
    write_mono_wav(tmp.path(), them_samples)?;

    let output = Command::new(diarize_binary)
        .arg(tmp.path())
        .output()
        .map_err(|e| InferenceError::Whisper(format!("spawn hearsay-diarize: {e}")))?;
    if !output.status.success() {
        return Err(InferenceError::Whisper(format!(
            "hearsay-diarize failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
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
    Ok(segments)
}

/// Refine a recorded meeting's `audio.wav` end-to-end: read the Them (right) channel, load the
/// whisper model, then re-diarize (`diarize_binary`) + re-transcribe. The single ML entry point
/// shared by the manual `/rediarize` route and the orchestrator's auto-refine-at-stop. Blocking
/// (whisper + subprocess) — call via `spawn_blocking` from async code.
pub fn refine_audio_file(
    audio_path: &Path,
    diarize_binary: &Path,
    model: &Path,
) -> Result<Vec<RefinedSegment>, InferenceError> {
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

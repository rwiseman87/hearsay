//! Post-meeting offline refine: re-diarize the Them track (reusing the Swift `hearsay-diarize`
//! FluidAudio sidecar — the same offline diarizer the live path's sidecars come from) and
//! re-transcribe each speaker turn with whisper → accurate `Speaker N` segments.
//!
//! [`refine_them`] also returns each speaker's voiceprint (from the diarizer's per-speaker mean
//! embedding); persistence, cross-meeting recognition, and carry-forward of locked manual labels
//! all live in `hearsay_db::replace_them_segments`, which both this refine's callers go through.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use hearsay_attribution::{order_speakers, SpeakerTurn};
use serde::Deserialize;

use crate::asr::WhisperAsr;
use crate::diarizer::{DiarTurn, Diarization, Diarizer};
use crate::error::InferenceError;

/// Contract-fixed track sample rate (Hz).
const SAMPLE_RATE: u32 = 16_000;

/// How often the bounded diarize wait polls the child for exit.
const DIARIZE_POLL_INTERVAL: Duration = Duration::from_millis(50);

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

/// The macOS diarizer: drives the Swift `hearsay-diarize` FluidAudio sidecar (file-based; a bounded
/// subprocess) and returns its `turns` + per-speaker embeddings. The default the app wires; keeps the
/// sidecar's current stdout-JSON / `noSpeechDetected`-stderr contract exactly as-is.
pub struct SwiftDiarizer<'a> {
    binary: &'a Path,
    timeout: Duration,
}

impl<'a> SwiftDiarizer<'a> {
    /// `binary` is the Swift `hearsay-diarize` sidecar; `timeout` bounds the subprocess (killed on
    /// expiry) so a hung sidecar can never wedge the refine — and thus meeting stop.
    pub fn new(binary: &'a Path, timeout: Duration) -> Self {
        Self { binary, timeout }
    }
}

impl Diarizer for SwiftDiarizer<'_> {
    fn diarize(&self, them_samples: &[f32]) -> Result<Diarization, InferenceError> {
        // hearsay-diarize is file-based: write the Them track to a temp wav.
        let tmp = tempfile::Builder::new().suffix(".wav").tempfile()?;
        write_mono_wav(tmp.path(), them_samples)?;

        let (stdout, stderr, status) = run_diarize(self.binary, tmp.path(), self.timeout)?;
        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr);
            // FluidAudio reports a silent / no-remote-speech track as an error; that is benign for a
            // refine (there is simply nothing to re-diarize), so surface it as a distinct variant the
            // caller can treat as a no-op rather than a failure.
            if stderr.contains("noSpeechDetected") {
                return Err(InferenceError::NoSpeech);
            }
            return Err(InferenceError::Diarize(format!(
                "hearsay-diarize failed: {}",
                stderr.trim()
            )));
        }
        parse_diarization(&stdout)
    }
}

/// Parse the `hearsay-diarize` sidecar's stdout JSON into a [`Diarization`]: speaker labels mapped to
/// 1-based ordinals by first appearance (canonical `order_speakers`), turns start-sorted, and each
/// speaker's raw voiceprint keyed by the same ordinal (an embedding for a label with no turn is
/// skipped; the refine L2-normalizes it later). Pure (no I/O), so the parse + ordinal mapping is
/// unit-tested against a fixture without the sidecar.
fn parse_diarization(stdout: &[u8]) -> Result<Diarization, InferenceError> {
    let diarized: DiarizeOutput = serde_json::from_slice(stdout)
        .map_err(|e| InferenceError::Diarize(format!("parse diarize output: {e}")))?;

    let ordering: Vec<SpeakerTurn> = diarized
        .turns
        .iter()
        .map(|t| SpeakerTurn {
            speaker: t.speaker.clone(),
            start_s: t.start_s,
            end_s: t.end_s,
        })
        .collect();
    let ordinals = order_speakers(&ordering);

    let mut turns: Vec<DiarTurn> = diarized
        .turns
        .iter()
        .map(|t| DiarTurn {
            speaker: i64::from(ordinals[&t.speaker]),
            start_s: t.start_s,
            end_s: t.end_s,
        })
        .collect();
    turns.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));

    let mut embeddings: HashMap<i64, Vec<f32>> = HashMap::new();
    for speaker in diarized.speakers {
        if let Some(&ord) = ordinals.get(&speaker.speaker) {
            embeddings.insert(i64::from(ord), speaker.embedding);
        }
    }

    Ok(Diarization { turns, embeddings })
}

/// Re-diarize + re-transcribe the Them track through a [`Diarizer`]: each diarizer turn's slice of
/// the Them track is re-transcribed with whisper into an accurate `Speaker N` segment, and each
/// speaker's raw voiceprint is L2-normalized into a stored centroid. The diarizer-agnostic refine
/// entry — the Swift sidecar ([`SwiftDiarizer`], via [`refine_them`]) or `SherpaDiarizer` plugs in.
/// Blocking (whisper) — call via `spawn_blocking` from async code.
pub fn refine_them_with(
    asr: &WhisperAsr,
    diarizer: &dyn Diarizer,
    them_samples: &[f32],
) -> Result<RefineOutput, InferenceError> {
    let diarization = diarizer.diarize(them_samples)?;

    // Re-transcribe each turn's slice of the Them track (turns are start-sorted).
    let mut segments = Vec::with_capacity(diarization.turns.len());
    for turn in &diarization.turns {
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
            ordinal: turn.speaker,
            text,
            start_s: turn.start_s,
            end_s: turn.end_s,
        });
    }

    let centroids = build_centroids(&diarization.embeddings);
    Ok(RefineOutput {
        segments,
        centroids,
    })
}

/// Re-diarize + re-transcribe the Them track with the Swift `hearsay-diarize` sidecar (the macOS
/// default). Thin wrapper over [`refine_them_with`] with a [`SwiftDiarizer`]: `diarize_binary` is the
/// sidecar and `timeout` bounds it. Blocking (whisper + subprocess) — call via `spawn_blocking`.
pub fn refine_them(
    asr: &WhisperAsr,
    diarize_binary: &Path,
    them_samples: &[f32],
    timeout: Duration,
) -> Result<RefineOutput, InferenceError> {
    refine_them_with(
        asr,
        &SwiftDiarizer::new(diarize_binary, timeout),
        them_samples,
    )
}

/// Spawn `hearsay-diarize <wav>` and wait for it with a deadline, killing it on expiry so a hung
/// sidecar can never wedge the refine. stdout/stderr are drained on their own threads so a large
/// payload (per-speaker embeddings) cannot deadlock the wait by filling a pipe buffer.
fn run_diarize(
    binary: &Path,
    wav: &Path,
    timeout: Duration,
) -> Result<(Vec<u8>, Vec<u8>, ExitStatus), InferenceError> {
    let mut child = Command::new(binary)
        .arg(wav)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| InferenceError::Diarize(format!("spawn hearsay-diarize: {e}")))?;

    let mut out_pipe = child.stdout.take().expect("stdout piped");
    let mut err_pipe = child.stderr.take().expect("stderr piped");
    let out_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        buf
    });
    let err_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| InferenceError::Diarize(format!("wait hearsay-diarize: {e}")))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(InferenceError::Diarize(format!(
                "hearsay-diarize timed out after {}s",
                timeout.as_secs()
            )));
        }
        thread::sleep(DIARIZE_POLL_INTERVAL);
    };

    // The child has exited, so both pipes are closed; the reader threads finish promptly.
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    Ok((stdout, stderr, status))
}

/// L2-normalize each speaker's raw voiceprint into a stored centroid, keyed by its 1-based ordinal
/// (empty embeddings are skipped). Port of `refine.py::_recognize_speakers`'s centroid step.
fn build_centroids(embeddings: &HashMap<i64, Vec<f32>>) -> HashMap<i64, Vec<f32>> {
    let mut centroids = HashMap::new();
    for (&ord, embedding) in embeddings {
        if let Some(centroid) = l2_normalize(embedding) {
            centroids.insert(ord, centroid);
        }
    }
    centroids
}

/// Unit-length a voiceprint so stored centroids match the cosine convention (norm computed in f64).
/// `None` for an empty vector; a zero vector is returned unchanged.
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
    timeout: Duration,
) -> Result<RefineOutput, InferenceError> {
    let them = crate::audio::read_them_channel(audio_path)?;
    let asr = WhisperAsr::load(model)?;
    refine_them(&asr, diarize_binary, &them, timeout)
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
    fn build_centroids_normalizes_and_skips_empty() {
        let embeddings = HashMap::from([(1_i64, vec![3.0_f32, 4.0]), (2_i64, vec![])]);
        let centroids = build_centroids(&embeddings);
        // The empty embedding (ordinal 2) is dropped; ordinal 1 is unit-normalized.
        assert_eq!(centroids.len(), 1);
        let a = &centroids[&1];
        assert!((a[0] - 0.6).abs() < 1e-6 && (a[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn parse_diarization_orders_speakers_by_time_and_keys_embeddings() {
        // Turns out of order: A first appears at 1.0, B at 5.0 -> A=1, B=2 (order_speakers sorts by
        // start). The returned turns are start-sorted; embeddings are keyed by the same ordinal.
        let json = br#"{
            "turns": [
                {"speaker": "B", "start_s": 5.0, "end_s": 6.0},
                {"speaker": "A", "start_s": 1.0, "end_s": 2.0},
                {"speaker": "B", "start_s": 8.0, "end_s": 9.0}
            ],
            "speakers": [
                {"speaker": "A", "embedding": [0.1, 0.2]},
                {"speaker": "B", "embedding": [0.3, 0.4]}
            ]
        }"#;
        let d = parse_diarization(json).unwrap();
        assert_eq!(
            d.turns.iter().map(|t| t.start_s).collect::<Vec<_>>(),
            vec![1.0, 5.0, 8.0]
        );
        assert_eq!(d.turns[0].speaker, 1); // A @1.0
        assert_eq!(d.turns[1].speaker, 2); // B @5.0
        assert_eq!(d.turns[2].speaker, 2); // B @8.0
        assert_eq!(d.embeddings[&1], vec![0.1, 0.2]); // A
        assert_eq!(d.embeddings[&2], vec![0.3, 0.4]); // B
    }

    #[test]
    fn parse_diarization_defaults_missing_speakers_and_skips_unturned_embeddings() {
        // No `speakers` field -> empty embeddings, not a parse failure.
        let d = parse_diarization(br#"{"turns":[{"speaker":"S1","start_s":0.0,"end_s":1.0}]}"#)
            .unwrap();
        assert_eq!(d.turns.len(), 1);
        assert_eq!(d.turns[0].speaker, 1);
        assert!(d.embeddings.is_empty());

        // An embedding for a label with no turn is skipped (not keyed to a phantom ordinal).
        let d2 = parse_diarization(
            br#"{"turns":[{"speaker":"S1","start_s":0.0,"end_s":1.0}],
                 "speakers":[{"speaker":"ghost","embedding":[1.0]}]}"#,
        )
        .unwrap();
        assert!(d2.embeddings.is_empty());
    }

    #[test]
    fn parse_diarization_rejects_malformed_json() {
        assert!(matches!(
            parse_diarization(b"not json at all"),
            Err(InferenceError::Diarize(_))
        ));
    }
}

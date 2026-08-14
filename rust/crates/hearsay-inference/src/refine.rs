//! Post-meeting offline refine: re-diarize the Them track (reusing the Swift `hearsay-diarize`
//! FluidAudio sidecar — the same offline diarizer the live path's sidecars come from) and
//! re-transcribe each speaker turn with whisper → accurate `Speaker N` segments.
//!
//! [`refine_them`] also returns each speaker's voiceprint (from the diarizer's per-speaker mean
//! embedding); persistence, cross-meeting recognition, and carry-forward of locked manual labels
//! all live in `hearsay_db::replace_them_segments`, which both this refine's callers go through.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use hearsay_attribution::{l2_normalize, max_overlap_turn, order_speakers, SpeakerTurn};
use serde::Deserialize;

use crate::asr::{AsrSegment, WhisperAsr};
use crate::diarizer::{DiarTurn, Diarization, Diarizer};
use crate::error::InferenceError;

use hearsay_audio::SAMPLE_RATE;

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

/// Re-diarize + re-transcribe the Them track through a [`Diarizer`]: the whole track is transcribed
/// once with whisper, then each ASR segment is attributed to the diarizer turn it most overlaps
/// ([`assemble_refined_segments`]) into `Speaker N` segments, and each speaker's raw voiceprint is
/// L2-normalized into a stored centroid. Transcribing whole-track (rather than slicing the track at
/// turn boundaries and transcribing each turn alone) gives whisper full context and never skips
/// inter-turn audio, so no speech is dropped. The diarizer-agnostic refine entry — the Swift sidecar
/// ([`SwiftDiarizer`], via [`refine_them`]) or `SherpaDiarizer` plugs in. Blocking (whisper) — call
/// via `spawn_blocking` from async code.
pub fn refine_them_with(
    asr: &WhisperAsr,
    diarizer: &dyn Diarizer,
    them_samples: &[f32],
) -> Result<RefineOutput, InferenceError> {
    // Diarize and transcribe are independent reads of the same immutable buffer, so overlap them:
    // whisper runs on a scoped thread (its `WhisperContext` is `Sync`, and each `transcribe` builds a
    // fresh `WhisperState`) while the diarizer runs on the calling thread. On macOS the diarizer is a
    // sleep-polled subprocess and whisper is on Metal, so the diarize duration fills the GPU's
    // otherwise-idle time instead of running before it. The diarizer stays on one thread (the sherpa
    // path is not thread-safe). `thread::scope` blocks until whisper finishes, so both borrows of
    // `them_samples` are safe.
    let (diarization, asr_result) = thread::scope(|scope| {
        let asr_handle = scope.spawn(|| asr.transcribe(them_samples));
        let diarization = diarizer.diarize(them_samples);
        (diarization, asr_handle.join())
    });
    // A diarizer `NoSpeech` / error still waited out the whole (now-discarded) transcription — the
    // scope cannot cancel it mid-run. Propagate it as the no-op the callers expect.
    let diarization = diarization?;
    let asr_segments = match asr_result {
        Ok(segments) => segments?,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    let segments = assemble_refined_segments(&asr_segments, &diarization.turns);

    // Keep a voiceprint only for a speaker that actually appears in the refined segments. Whole-track
    // overlap attribution can leave a speaker whose speech was entirely overlap-dominated with no
    // segment; that speaker has no cluster to store a voiceprint on, so an orphan centroid would only
    // waste a cross-meeting recognition match. This keeps `centroids` ⊆ the segments' speakers.
    let present: HashSet<i64> = segments.iter().map(|s| s.ordinal).collect();
    let mut centroids = build_centroids(&diarization.embeddings);
    centroids.retain(|ordinal, _| present.contains(ordinal));
    Ok(RefineOutput {
        segments,
        centroids,
    })
}

/// Attribute each whole-track ASR segment to the diarizer turn it most overlaps (falling back to the
/// nearest turn in time when a segment overlaps none, so no transcribed text is dropped), then merge
/// consecutive segments attributed to the *same turn* into one `RefinedSegment`.
///
/// Merging is bounded by the turn, not the speaker: same-speaker runs can span many turns (one
/// remote speaker holding the floor for minutes — or a diarizer collapsing several people into one
/// cluster), and merging across them produced a single blob spanning most of a meeting. The final
/// transcript interleaves Me segments by `start_s`, so a blob starting at 0:00 pushed every Me
/// utterance spoken *during* it after it. Turn-bounded segments keep meeting-time granularity (turns
/// end at real speech pauses), and the transcript writer already regroups consecutive same-speaker
/// segments under one header, so an uninterrupted run still renders as one block.
///
/// Empty `turns` yields no segments — there is no speaker to attribute to, which the refine's
/// callers treat as a no-op. Pure — no ML or I/O, so it is unit-tested without whisper or the
/// diarizer sidecar.
fn assemble_refined_segments(
    asr_segments: &[AsrSegment],
    turns: &[DiarTurn],
) -> Vec<RefinedSegment> {
    if turns.is_empty() {
        return Vec::new();
    }
    // Adapt the ordinal-keyed turns to the shared overlap helper; the label is unused — the returned
    // index maps back to the turn's ordinal below.
    let overlap_turns: Vec<SpeakerTurn> = turns
        .iter()
        .map(|t| SpeakerTurn {
            speaker: String::new(),
            start_s: t.start_s,
            end_s: t.end_s,
        })
        .collect();

    let mut segments: Vec<RefinedSegment> = Vec::new();
    let mut last_turn: Option<usize> = None;
    for seg in asr_segments {
        let text = seg.text.trim();
        if text.is_empty() {
            continue;
        }
        let idx = max_overlap_turn(seg.start_s, seg.end_s, &overlap_turns, 0.0)
            .unwrap_or_else(|| nearest_turn(seg.start_s, seg.end_s, turns));
        match segments.last_mut() {
            Some(last) if last_turn == Some(idx) => {
                last.text.push(' ');
                last.text.push_str(text);
                last.end_s = seg.end_s;
            }
            _ => segments.push(RefinedSegment {
                ordinal: turns[idx].speaker,
                text: text.to_string(),
                start_s: seg.start_s,
                end_s: seg.end_s,
            }),
        }
        last_turn = Some(idx);
    }
    segments
}

/// Index of the turn closest in time to segment `[start_s, end_s]` (0 distance if the segment's
/// midpoint falls inside a turn). `turns` must be non-empty. The fallback for a segment overlapping
/// no turn, so its text is still attributed rather than dropped.
fn nearest_turn(start_s: f64, end_s: f64, turns: &[DiarTurn]) -> usize {
    let mid = (start_s + end_s) / 2.0;
    let mut best = 0;
    let mut best_dist = f64::INFINITY;
    for (i, turn) in turns.iter().enumerate() {
        let dist = if mid < turn.start_s {
            turn.start_s - mid
        } else if mid > turn.end_s {
            mid - turn.end_s
        } else {
            0.0
        };
        if dist < best_dist {
            best_dist = dist;
            best = i;
        }
    }
    best
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
/// (empty embeddings are skipped).
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
/// Refine a recorded meeting's `audio.wav` end-to-end with any [`Diarizer`]: read the Them (right)
/// channel, load the whisper model, then re-diarize + re-transcribe. The diarizer-agnostic file
/// entry — the macOS refiner wraps it with [`SwiftDiarizer`] ([`refine_audio_file`]) and the
/// Windows refiner with `SherpaDiarizer`. Blocking (whisper) — call via `spawn_blocking` from
/// async code.
pub fn refine_audio_file_with(
    audio_path: &Path,
    diarizer: &dyn Diarizer,
    model: &Path,
) -> Result<RefineOutput, InferenceError> {
    let them = crate::audio::read_them_channel(audio_path)?;
    let asr = WhisperAsr::load(model)?;
    refine_them_with(&asr, diarizer, &them)
}

/// Refine a recorded meeting's `audio.wav` end-to-end with the Swift `hearsay-diarize` sidecar
/// (`diarize_binary`, bounded by `timeout`) — the macOS entry point shared by the manual
/// `/rediarize` route and the orchestrator's auto-refine-at-stop. Blocking (whisper + subprocess)
/// — call via `spawn_blocking` from async code.
pub fn refine_audio_file(
    audio_path: &Path,
    diarize_binary: &Path,
    model: &Path,
    timeout: Duration,
) -> Result<RefineOutput, InferenceError> {
    refine_audio_file_with(
        audio_path,
        &SwiftDiarizer::new(diarize_binary, timeout),
        model,
    )
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

    fn diar(speaker: i64, start_s: f64, end_s: f64) -> DiarTurn {
        DiarTurn {
            speaker,
            start_s,
            end_s,
        }
    }

    fn asr(text: &str, start_s: f64, end_s: f64) -> AsrSegment {
        AsrSegment {
            text: text.to_string(),
            start_s,
            end_s,
        }
    }

    #[test]
    fn assemble_attributes_each_segment_by_max_overlap() {
        let turns = vec![diar(1, 0.0, 5.0), diar(2, 5.0, 10.0)];
        let segs = vec![asr("hello", 0.5, 4.0), asr("world", 5.5, 9.0)];
        let out = assemble_refined_segments(&segs, &turns);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].ordinal, out[0].text.as_str()), (1, "hello"));
        assert_eq!((out[1].ordinal, out[1].text.as_str()), (2, "world"));
    }

    #[test]
    fn assemble_merges_consecutive_segments_of_the_same_turn() {
        let turns = vec![diar(1, 0.0, 10.0)];
        let segs = vec![asr("hello", 0.0, 2.0), asr("there", 2.0, 4.0)];
        let out = assemble_refined_segments(&segs, &turns);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ordinal, 1);
        assert_eq!(out[0].text, "hello there");
        // The merged segment spans the first start to the last end.
        assert_eq!((out[0].start_s, out[0].end_s), (0.0, 4.0));
    }

    #[test]
    fn assemble_does_not_merge_across_turns_of_the_same_speaker() {
        // One speaker, two turns (a real speech pause between them): the segments stay separate so
        // the final transcript can interleave Me utterances spoken during the pause. A single blob
        // here is the failure mode that pushed a whole meeting's Me lines after one giant segment.
        let turns = vec![diar(1, 0.0, 4.0), diar(1, 6.0, 10.0)];
        let segs = vec![asr("before the pause", 0.0, 4.0), asr("after it", 6.0, 9.0)];
        let out = assemble_refined_segments(&segs, &turns);
        assert_eq!(out.len(), 2);
        assert_eq!(
            (out[0].ordinal, out[0].text.as_str()),
            (1, "before the pause")
        );
        assert_eq!((out[1].ordinal, out[1].text.as_str()), (1, "after it"));
        assert_eq!((out[1].start_s, out[1].end_s), (6.0, 9.0));
    }

    #[test]
    fn assemble_keeps_unoverlapped_text_via_nearest_turn() {
        // A segment overlapping no turn is attributed to the nearest turn, never dropped.
        let turns = vec![diar(1, 0.0, 5.0), diar(2, 50.0, 55.0)];
        let out = assemble_refined_segments(&[asr("stray", 6.0, 7.0)], &turns);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].ordinal, out[0].text.as_str()), (1, "stray"));
    }

    #[test]
    fn assemble_skips_blank_segments_and_empty_turns() {
        let turns = vec![diar(1, 0.0, 5.0)];
        let out = assemble_refined_segments(&[asr("   ", 0.0, 1.0), asr("real", 1.0, 2.0)], &turns);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "real");
        // No turns -> nothing to attribute to.
        assert!(assemble_refined_segments(&[asr("hi", 0.0, 1.0)], &[]).is_empty());
    }
}

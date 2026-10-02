//! Offline refine: one `hearsay-diarize` run diarizes and transcribes the Them track, and each word
//! is attributed to the speaker turn it overlaps.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::ops::Range;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use hearsay_attribution::{l2_normalize, order_speakers, SpeakerTurn};
use serde::Deserialize;

use crate::coverage::{self, Coverage};
use crate::error::InferenceError;

use hearsay_audio::SAMPLE_RATE;

/// How often the bounded sidecar wait polls the child for exit.
const SIDECAR_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The Parakeet model the refine transcribes with (a `hearsay-diarize --asr` value).
pub const ASR_MODEL: &str = "ultra";

/// One diarizer turn: a 1-based speaker ordinal (by first appearance) over `[start_s, end_s)`,
/// track-relative seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct DiarTurn {
    pub speaker: i64,
    pub start_s: f64,
    pub end_s: f64,
}

/// Ordinal speaker turns (start-sorted) and each speaker's raw mean voiceprint by ordinal.
#[derive(Debug, Clone, Default)]
pub struct Diarization {
    pub turns: Vec<DiarTurn>,
    pub embeddings: HashMap<i64, Vec<f32>>,
}

/// One refined Them segment: the words of a diarizer turn, tagged with its 1-based speaker ordinal
/// (`Speaker {ordinal}`).
#[derive(Debug, Clone, PartialEq)]
pub struct RefinedSegment {
    pub ordinal: i64,
    pub text: String,
    pub start_s: f64,
    pub end_s: f64,
}

/// The refine's output: segments plus each speaker's L2-normalized voiceprint by ordinal.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RefineOutput {
    pub segments: Vec<RefinedSegment>,
    pub centroids: HashMap<i64, Vec<f32>>,
    /// Audible-vs-transcribed coverage; the signal that a transcript is truncated, not quiet.
    pub coverage: Option<Coverage>,
}

#[derive(Deserialize)]
struct SidecarOutput {
    turns: Vec<SidecarTurn>,
    /// Per-speaker mean voiceprint; absent when the model emits none.
    #[serde(default)]
    speakers: Vec<SpeakerEmbedding>,
    /// Present only when the sidecar ran with `--asr`.
    #[serde(default)]
    asr: Option<AsrPayload>,
}

#[derive(Deserialize)]
struct SidecarTurn {
    speaker: String,
    start_s: f64,
    end_s: f64,
}

#[derive(Deserialize)]
struct SpeakerEmbedding {
    speaker: String,
    embedding: Vec<f32>,
}

#[derive(Deserialize)]
struct AsrPayload {
    words: Vec<AsrWord>,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
struct AsrWord {
    word: String,
    start_s: f64,
    end_s: f64,
}

/// Write the Them track to a temp wav, run `hearsay-diarize` on it (with `--asr <model>` when `model`
/// is given) under a deadline, and parse its stdout JSON.
fn run_sidecar(
    binary: &Path,
    samples: &[f32],
    model: Option<&str>,
    timeout: Duration,
) -> Result<SidecarOutput, InferenceError> {
    let tmp = tempfile::Builder::new().suffix(".wav").tempfile()?;
    write_mono_wav(tmp.path(), samples)?;
    let (stdout, stderr, status) = run_bounded(binary, tmp.path(), model, timeout)?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        // A silent track is a benign no-op, not a failure.
        if stderr.contains("noSpeechDetected") {
            return Err(InferenceError::NoSpeech);
        }
        return Err(InferenceError::Sidecar(format!(
            "hearsay-diarize failed: {}",
            stderr.trim()
        )));
    }
    parse_sidecar(&stdout)
}

fn parse_sidecar(stdout: &[u8]) -> Result<SidecarOutput, InferenceError> {
    serde_json::from_slice(stdout)
        .map_err(|e| InferenceError::Sidecar(format!("parse hearsay-diarize output: {e}")))
}

/// Map speaker labels to 1-based ordinals by first appearance, start-sort the turns, and key each
/// speaker's raw voiceprint by the same ordinal.
fn to_diarization(output: &SidecarOutput) -> Diarization {
    let ordering: Vec<SpeakerTurn> = output
        .turns
        .iter()
        .map(|t| SpeakerTurn {
            speaker: t.speaker.clone(),
            start_s: t.start_s,
            end_s: t.end_s,
        })
        .collect();
    let ordinals = order_speakers(&ordering);

    let mut turns: Vec<DiarTurn> = output
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
    for speaker in &output.speakers {
        if let Some(&ord) = ordinals.get(&speaker.speaker) {
            embeddings.insert(i64::from(ord), speaker.embedding.clone());
        }
    }
    Diarization { turns, embeddings }
}

/// Diarize the Them track alone (no ASR). Blocking — call via `spawn_blocking`.
pub fn diarize(
    binary: &Path,
    them_samples: &[f32],
    timeout: Duration,
) -> Result<Diarization, InferenceError> {
    Ok(to_diarization(&run_sidecar(
        binary,
        them_samples,
        None,
        timeout,
    )?))
}

/// Re-diarize and re-transcribe the Them track with one `hearsay-diarize --asr <model>` run, bounded
/// by `timeout`. Blocking — call via `spawn_blocking`.
pub fn refine_them(
    binary: &Path,
    model: &str,
    them_samples: &[f32],
    timeout: Duration,
) -> Result<RefineOutput, InferenceError> {
    let output = run_sidecar(binary, them_samples, Some(model), timeout)?;
    let diarization = to_diarization(&output);
    let words = output
        .asr
        .ok_or_else(|| InferenceError::Sidecar("hearsay-diarize returned no asr payload".into()))?
        .words;
    let segments = assemble_refined_segments(&words, &diarization.turns);

    // Keep centroids only for speakers that have a segment (no orphan voiceprints).
    let present: HashSet<i64> = segments.iter().map(|s| s.ordinal).collect();
    let mut centroids = build_centroids(&diarization.embeddings);
    centroids.retain(|ordinal, _| present.contains(ordinal));

    // Measure only words that reached a segment, so words dropped for lack of turns read as missing.
    let speech: Vec<Range<f64>> = if segments.is_empty() {
        Vec::new()
    } else {
        words.iter().map(|w| w.start_s..w.end_s).collect()
    };
    Ok(RefineOutput {
        segments,
        centroids,
        coverage: Some(coverage::measure(them_samples, &speech)),
    })
}

/// Refine a meeting's `audio.wav`: read the Them channel and run [`refine_them`] with [`ASR_MODEL`].
/// Blocking — call via `spawn_blocking`.
pub fn refine_audio_file(
    audio_path: &Path,
    binary: &Path,
    timeout: Duration,
) -> Result<RefineOutput, InferenceError> {
    let them = crate::audio::read_them_channel(audio_path)?;
    refine_them(binary, ASR_MODEL, &them, timeout)
}

/// Attribute each word to the turn it overlaps most (else the nearest), merging consecutive words of
/// the same turn into one segment; merging stops at turn boundaries so Me lines can interleave.
fn assemble_refined_segments(words: &[AsrWord], turns: &[DiarTurn]) -> Vec<RefinedSegment> {
    if turns.is_empty() {
        return Vec::new();
    }
    let mut ordered: Vec<&AsrWord> = words.iter().collect();
    ordered.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));

    let mut segments: Vec<RefinedSegment> = Vec::new();
    let mut last_turn: Option<usize> = None;
    for word in ordered {
        let text = word.word.trim();
        if text.is_empty() {
            continue;
        }
        let idx = best_turn(word.start_s, word.end_s, turns)
            .unwrap_or_else(|| nearest_turn(word.start_s, word.end_s, turns));
        match segments.last_mut() {
            Some(last) if last_turn == Some(idx) => {
                last.text.push(' ');
                last.text.push_str(text);
                last.end_s = word.end_s;
            }
            _ => segments.push(RefinedSegment {
                ordinal: turns[idx].speaker,
                text: text.to_string(),
                start_s: word.start_s,
                end_s: word.end_s,
            }),
        }
        last_turn = Some(idx);
    }
    segments
}

/// Index of the turn overlapping `[start_s, end_s]` most, or `None`; ties go to the shorter turn (a
/// nested interjection), then the earlier one.
fn best_turn(start_s: f64, end_s: f64, turns: &[DiarTurn]) -> Option<usize> {
    let mut best: Option<(usize, f64, f64)> = None;
    for (i, turn) in turns.iter().enumerate() {
        let overlap = end_s.min(turn.end_s) - start_s.max(turn.start_s);
        if overlap <= 0.0 {
            continue;
        }
        let length = turn.end_s - turn.start_s;
        let better = match best {
            None => true,
            Some((_, best_overlap, best_length)) => {
                overlap > best_overlap || (overlap == best_overlap && length < best_length)
            }
        };
        if better {
            best = Some((i, overlap, length));
        }
    }
    best.map(|(i, _, _)| i)
}

/// Index of the turn nearest `[start_s, end_s]` in time (`turns` must be non-empty); ties go to the
/// shorter turn, then the earlier one.
fn nearest_turn(start_s: f64, end_s: f64, turns: &[DiarTurn]) -> usize {
    let mid = (start_s + end_s) / 2.0;
    let mut best = 0;
    let mut best_dist = f64::INFINITY;
    let mut best_length = f64::INFINITY;
    for (i, turn) in turns.iter().enumerate() {
        let dist = if mid < turn.start_s {
            turn.start_s - mid
        } else if mid > turn.end_s {
            mid - turn.end_s
        } else {
            0.0
        };
        let length = turn.end_s - turn.start_s;
        if dist < best_dist || (dist == best_dist && length < best_length) {
            best_dist = dist;
            best_length = length;
            best = i;
        }
    }
    best
}

/// Run `hearsay-diarize` under a deadline (killed on expiry), draining stdout/stderr on threads so a
/// large payload cannot deadlock the wait.
fn run_bounded(
    binary: &Path,
    wav: &Path,
    model: Option<&str>,
    timeout: Duration,
) -> Result<(Vec<u8>, Vec<u8>, ExitStatus), InferenceError> {
    let mut command = Command::new(binary);
    command.arg(wav);
    if let Some(model) = model {
        command.args(["--asr", model]);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| InferenceError::Sidecar(format!("spawn hearsay-diarize: {e}")))?;

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
            .map_err(|e| InferenceError::Sidecar(format!("wait hearsay-diarize: {e}")))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(InferenceError::Sidecar(format!(
                "hearsay-diarize timed out after {}s",
                timeout.as_secs()
            )));
        }
        thread::sleep(SIDECAR_POLL_INTERVAL);
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
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn build_centroids_normalizes_and_skips_empty() {
        let embeddings = HashMap::from([(1_i64, vec![3.0_f32, 4.0]), (2_i64, vec![])]);
        let centroids = build_centroids(&embeddings);
        // The empty embedding (ordinal 2) is dropped; ordinal 1 is unit-normalized.
        assert_eq!(centroids.len(), 1);
        let a = &centroids[&1];
        assert!((a[0] - 0.6).abs() < 1e-6 && (a[1] - 0.8).abs() < 1e-6);
    }

    fn diarization(json: &[u8]) -> Diarization {
        to_diarization(&parse_sidecar(json).unwrap())
    }

    #[test]
    fn diarization_orders_speakers_by_time_and_keys_embeddings() {
        // Turns out of order: A first appears at 1.0, B at 5.0 -> A=1, B=2 (order_speakers sorts by
        // start). The returned turns are start-sorted; embeddings are keyed by the same ordinal.
        let d = diarization(
            br#"{
            "turns": [
                {"speaker": "B", "start_s": 5.0, "end_s": 6.0},
                {"speaker": "A", "start_s": 1.0, "end_s": 2.0},
                {"speaker": "B", "start_s": 8.0, "end_s": 9.0}
            ],
            "speakers": [
                {"speaker": "A", "embedding": [0.1, 0.2]},
                {"speaker": "B", "embedding": [0.3, 0.4]}
            ]
        }"#,
        );
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
    fn diarization_defaults_missing_speakers_and_skips_unturned_embeddings() {
        // No `speakers` field -> empty embeddings, not a parse failure.
        let d = diarization(br#"{"turns":[{"speaker":"S1","start_s":0.0,"end_s":1.0}]}"#);
        assert_eq!(d.turns.len(), 1);
        assert_eq!(d.turns[0].speaker, 1);
        assert!(d.embeddings.is_empty());

        // An embedding for a label with no turn is skipped (not keyed to a phantom ordinal).
        let d2 = diarization(
            br#"{"turns":[{"speaker":"S1","start_s":0.0,"end_s":1.0}],
                 "speakers":[{"speaker":"ghost","embedding":[1.0]}]}"#,
        );
        assert!(d2.embeddings.is_empty());
    }

    #[test]
    fn parse_rejects_malformed_json_and_reads_the_asr_payload() {
        assert!(matches!(
            parse_sidecar(b"not json at all"),
            Err(InferenceError::Sidecar(_))
        ));
        let out = parse_sidecar(
            br#"{"turns":[],"asr":{"model":"ultra","processing_s":1.0,
                 "words":[{"word":"hi","start_s":0.5,"end_s":0.9,"confidence":0.9}]}}"#,
        )
        .unwrap();
        assert_eq!(out.asr.unwrap().words.len(), 1);
        assert!(parse_sidecar(br#"{"turns":[]}"#).unwrap().asr.is_none());
    }

    fn diar(speaker: i64, start_s: f64, end_s: f64) -> DiarTurn {
        DiarTurn {
            speaker,
            start_s,
            end_s,
        }
    }

    fn word(text: &str, start_s: f64, end_s: f64) -> AsrWord {
        AsrWord {
            word: text.to_string(),
            start_s,
            end_s,
        }
    }

    #[test]
    fn assemble_attributes_each_word_by_max_overlap() {
        let turns = vec![diar(1, 0.0, 5.0), diar(2, 5.0, 10.0)];
        let words = vec![word("hello", 0.5, 1.0), word("world", 5.5, 6.0)];
        let out = assemble_refined_segments(&words, &turns);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].ordinal, out[0].text.as_str()), (1, "hello"));
        assert_eq!((out[1].ordinal, out[1].text.as_str()), (2, "world"));
    }

    #[test]
    fn assemble_merges_consecutive_words_of_the_same_turn() {
        let turns = vec![diar(1, 0.0, 10.0)];
        let words = vec![word("hello", 0.0, 0.5), word("there", 0.6, 1.2)];
        let out = assemble_refined_segments(&words, &turns);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ordinal, 1);
        assert_eq!(out[0].text, "hello there");
        // The merged segment spans the first start to the last end.
        assert_eq!((out[0].start_s, out[0].end_s), (0.0, 1.2));
    }

    #[test]
    fn assemble_does_not_merge_across_turns_of_the_same_speaker() {
        // Two turns of one speaker stay separate so Me lines spoken in the pause can interleave.
        let turns = vec![diar(1, 0.0, 4.0), diar(1, 6.0, 10.0)];
        let words = vec![
            word("before", 0.0, 1.0),
            word("it", 6.0, 6.5),
            word("after", 6.5, 7.0),
        ];
        let out = assemble_refined_segments(&words, &turns);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].ordinal, out[0].text.as_str()), (1, "before"));
        assert_eq!((out[1].ordinal, out[1].text.as_str()), (1, "it after"));
        assert_eq!((out[1].start_s, out[1].end_s), (6.0, 7.0));
    }

    #[test]
    fn assemble_keeps_a_short_interjection_with_its_own_speaker() {
        // Speaker 2 says "right" while speaker 1 holds a long turn: word-level attribution keeps the
        // interjection out of speaker 1's segment.
        let turns = vec![diar(1, 0.0, 10.0), diar(2, 4.0, 4.6)];
        let words = vec![
            word("so", 1.0, 1.3),
            word("right", 4.1, 4.5),
            word("anyway", 5.0, 5.5),
        ];
        let out = assemble_refined_segments(&words, &turns);
        let ordinals: Vec<i64> = out.iter().map(|s| s.ordinal).collect();
        assert_eq!(ordinals, vec![1, 2, 1]);
        assert_eq!(out[1].text, "right");
    }

    #[test]
    fn assemble_keeps_unoverlapped_text_via_nearest_turn() {
        // A word overlapping no turn is attributed to the nearest turn, never dropped.
        let turns = vec![diar(1, 0.0, 5.0), diar(2, 50.0, 55.0)];
        let out = assemble_refined_segments(&[word("stray", 6.0, 7.0)], &turns);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].ordinal, out[0].text.as_str()), (1, "stray"));
    }

    #[test]
    fn assemble_gives_a_zero_length_word_to_the_shorter_containing_turn() {
        let turns = vec![diar(1, 0.0, 10.0), diar(2, 4.0, 4.6)];
        let out = assemble_refined_segments(&[word("x", 4.2, 4.2)], &turns);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ordinal, 2);
    }

    #[test]
    fn refine_with_words_but_no_turns_reports_the_words_as_uncovered() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("hearsay-diarize");
        std::fs::write(
            &sidecar,
            "#!/bin/sh\necho '{\"turns\":[],\"asr\":{\"words\":[{\"word\":\"hi\",\"start_s\":0.0,\"end_s\":2.0}]}}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o755)).unwrap();
        let loud = vec![0.5_f32; 2 * SAMPLE_RATE as usize];
        let out = refine_them(&sidecar, ASR_MODEL, &loud, Duration::from_secs(10)).unwrap();
        assert!(out.segments.is_empty());
        assert_eq!(out.coverage.unwrap().fraction(), 0.0);
    }

    #[test]
    fn assemble_orders_words_skips_blanks_and_handles_empty_turns() {
        let turns = vec![diar(1, 0.0, 5.0)];
        let words = vec![
            word("   ", 0.0, 1.0),
            word("two", 2.0, 2.5),
            word("one", 1.0, 1.5),
        ];
        let out = assemble_refined_segments(&words, &turns);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "one two");
        // No turns -> nothing to attribute to.
        assert!(assemble_refined_segments(&[word("hi", 0.0, 1.0)], &[]).is_empty());
    }
}

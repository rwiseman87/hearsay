//! Offline ASR via whisper.cpp (`whisper-rs`). Loads a GGML model once and transcribes 16 kHz mono
//! PCM into timestamped segments. This is the offline path — the accuracy-verification harness and
//! the orchestrator's post-meeting refine. GPU acceleration (Metal / Vulkan / CUDA) is a
//! `whisper-rs` Cargo feature; with none enabled it runs on CPU.

use std::ops::Range;
use std::path::Path;

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::audio::SAMPLE_RATE;
use crate::error::InferenceError;

/// Default whisper transcription language (a whisper language code). English by default; a caller
/// overrides it via [`WhisperAsr::with_language`] (the offline refine keeps this default).
pub const DEFAULT_LANGUAGE: &str = "en";

/// Anti-loop entropy threshold (whisper.cpp `entropy_thold`; its default is 2.4).
///
/// Long conversational audio can drop the greedy decoder into a self-sustaining repetition loop —
/// one confident phrase emitted over and over in 1-second segments, filling every remaining 30-s
/// window to the end of the track (a 17-minute meeting came back as one phrase repeated 473
/// times). The fallback gate that should catch this compares the entropy of the window's last 32
/// tokens against the threshold, and the loops clear the stock 2.4: measured on a real meeting, a
/// single-phrase loop scored 2.45 and a two-phrase ping-pong loop 2.83, while genuine speech
/// windows scored 3.06-3.38. 3.0 sits in that gap — every observed loop now fails the gate, which
/// retries the window at a higher temperature and breaks the attractor, and real windows pass
/// untouched. Verified loop-free on the failing meeting and regression-free on a known-good one.
///
/// The gate is necessary but *not* sufficient: whisper.cpp measures that entropy over the last 32
/// tokens only, so a loop whose repeated unit is itself ~32 tokens or longer fills the window with
/// distinct tokens and scores like ordinary speech no matter how the threshold is set. The
/// long-phrase case is caught after the fact instead — see [`find_loop_runs`].
pub const DEFAULT_ENTROPY_THOLD: f32 = 3.0;

/// How many back-to-back repeats of the same segment cycle count as a decoder loop rather than
/// speech. Three is deliberately low: real speech that trips it survives either way, since a
/// re-decode of genuine repetition reproduces it ([`WhisperAsr::repair_loops`]), so the only cost of
/// a false positive is the re-decode itself.
pub const LOOP_MIN_CYCLES: usize = 3;

/// Longest segment cycle [`find_loop_runs`] looks for. 1 is a single phrase repeating; 2 covers the
/// observed two-phrase "ping-pong" loop.
const LOOP_MAX_PERIOD: usize = 3;

/// Word count past which a repeating unit is a decoder loop no matter what a re-decode says. People
/// do repeat themselves verbatim — "yeah, yeah, yeah", "no, no, no" — but not a whole clause, three
/// times, word for word. Five words puts every observed loop (12 and 20 words) on the loop side and
/// leaves the short interjections alone.
const LOOP_MAX_REAL_WORDS: usize = 5;

/// Shortest run worth re-decoding. Below this the audio is too short for whisper to do better, and a
/// sub-second three-peat is far more likely to be real speech ("yeah, yeah, yeah") than a loop.
const LOOP_MIN_RETRY_S: f64 = 1.0;

/// Shortest untranscribed span treated as a stalled decode rather than a pause; observed
/// conversational gaps run to ~20 s.
const GAP_MIN_S: f64 = 45.0;

/// Recovery passes allowed. With [`MIN_PROGRESS_S`] this bounds the loop.
const MAX_RECOVERY_PASSES: usize = 3;

/// Transcript a pass must add to earn another.
const MIN_PROGRESS_S: f64 = 1.0;

/// RMS below which an untranscribed span counts as real silence, not a miss.
const SILENT_RMS: f64 = 0.005;

/// One transcribed segment. Times are seconds from the start of the given audio.
#[derive(Debug, Clone, PartialEq)]
pub struct AsrSegment {
    pub text: String,
    pub start_s: f64,
    pub end_s: f64,
}

/// What a whole-track decode covered. Whisper returns success even when it stops emitting text
/// part-way, so this is the only way to tell a quiet meeting from a truncated one.
#[derive(Debug, Clone, PartialEq)]
pub struct Coverage {
    pub track_s: f64,
    /// Track time above [`SILENT_RMS`] — the time that should yield transcript.
    pub audible_s: f64,
    /// Audible time that landed inside a segment.
    pub transcribed_s: f64,
    /// Spans re-decoded to recover a stall.
    pub recovered: Vec<Range<f64>>,
    /// Audible spans still untranscribed after recovery.
    pub unrecovered: Vec<Range<f64>>,
}

impl Coverage {
    /// Transcribed share of audible time. A silent track counts as covered; the bar for "too low"
    /// lives once, on `hearsay_db::MIN_REFINE_COVERAGE`.
    pub fn fraction(&self) -> f64 {
        if self.audible_s <= 0.0 {
            return 1.0;
        }
        (self.transcribed_s / self.audible_s).clamp(0.0, 1.0)
    }
}

/// A whole-track decode: the segments plus what they cover ([`Coverage`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Transcription {
    pub segments: Vec<AsrSegment>,
    pub coverage: Coverage,
}

/// A loaded whisper.cpp model. Cheap to clone the handle; `transcribe` creates a fresh state per
/// call so it is safe to reuse across audio.
pub struct WhisperAsr {
    ctx: WhisperContext,
    language: String,
    entropy_thold: f32,
    carry_over: bool,
}

impl WhisperAsr {
    /// Load a GGML whisper model (e.g. `ggml-base.bin`, `ggml-large-v3-turbo.bin`). Transcription
    /// language defaults to [`DEFAULT_LANGUAGE`]; override it with
    /// [`with_language`](Self::with_language).
    pub fn load(model_path: impl AsRef<Path>) -> Result<Self, InferenceError> {
        let ctx = WhisperContext::new_with_params(
            model_path.as_ref(),
            WhisperContextParameters::default(),
        )
        .map_err(|e| InferenceError::Whisper(format!("load model: {e}")))?;
        Ok(WhisperAsr {
            ctx,
            language: DEFAULT_LANGUAGE.to_string(),
            entropy_thold: DEFAULT_ENTROPY_THOLD,
            carry_over: true,
        })
    }

    /// Override the anti-loop entropy threshold ([`DEFAULT_ENTROPY_THOLD`]); the probe harness
    /// uses this to measure candidate thresholds against recorded meetings.
    pub fn with_entropy_thold(mut self, thold: f32) -> Self {
        self.entropy_thold = thold;
        self
    }

    /// Turn off the prompt whisper carries between 30-s windows, for the whole-track pass as well as
    /// the span re-decodes. The probe harness uses this to measure what carry-over is worth.
    pub fn with_carry_over(mut self, carry_over: bool) -> Self {
        self.carry_over = carry_over;
        self
    }

    /// Set the transcription language (a whisper language code, e.g. `"de"`, or `"auto"` to detect);
    /// defaults to [`DEFAULT_LANGUAGE`]. The offline refine keeps the default.
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = language.into();
        self
    }

    /// Transcribe 16 kHz mono `samples` (float in [-1, 1]) into timestamped segments (greedy; the
    /// model's `language`, default English). Timestamps come from whisper's centisecond segment
    /// bounds. Decoder repetition loops are repaired ([`WhisperAsr::repair_loops`]) and stalled
    /// stretches re-decoded ([`WhisperAsr::recover_gaps`]) before the segments are returned.
    pub fn transcribe(&self, samples: &[f32]) -> Result<Vec<AsrSegment>, InferenceError> {
        Ok(self.transcribe_track(samples)?.segments)
    }

    /// [`transcribe`](Self::transcribe) plus the [`Coverage`] it achieved; the refine needs it to
    /// tell a quiet meeting from a truncated decode.
    pub fn transcribe_track(&self, samples: &[f32]) -> Result<Transcription, InferenceError> {
        let decoded = self.decode(samples, None)?;
        let segments = self.repair_loops(samples, decoded)?;
        self.recover_gaps(samples, segments)
    }

    /// Re-decode audible stretches the whole-track pass left empty, prompt-free as
    /// [`redecode_loop`](Self::redecode_loop) does. A poisoned rolling prompt makes every later
    /// window emit timestamps and no text, and `whisper_full` still returns success.
    fn recover_gaps(
        &self,
        samples: &[f32],
        mut segments: Vec<AsrSegment>,
    ) -> Result<Transcription, InferenceError> {
        let track_s = samples.len() as f64 / f64::from(SAMPLE_RATE);
        let mut recovered: Vec<Range<f64>> = Vec::new();

        for _ in 0..MAX_RECOVERY_PASSES {
            let gaps: Vec<Range<f64>> = untranscribed_spans(&segments, track_s)
                .into_iter()
                .filter(|gap| gap.end - gap.start >= GAP_MIN_S)
                .filter(|gap| span_rms(samples, gap) > SILENT_RMS)
                .collect();
            if gaps.is_empty() {
                break;
            }
            let before = transcribed_s(&segments);
            for gap in gaps {
                let filled = self.decode(samples, Some(gap.clone()))?;
                if filled.is_empty() {
                    continue;
                }
                segments.extend(self.repair_loops(samples, filled)?);
                recovered.push(gap);
            }
            segments.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
            if transcribed_s(&segments) - before < MIN_PROGRESS_S {
                break;
            }
        }

        let unrecovered: Vec<Range<f64>> = untranscribed_spans(&segments, track_s)
            .into_iter()
            .filter(|gap| gap.end - gap.start >= GAP_MIN_S)
            .filter(|gap| span_rms(samples, gap) > SILENT_RMS)
            .collect();
        let (audible_s, transcribed_s) = audible_coverage(samples, &segments, track_s);
        let coverage = Coverage {
            track_s,
            audible_s,
            transcribed_s,
            recovered,
            unrecovered,
        };
        Ok(Transcription { segments, coverage })
    }

    /// One whisper pass over `samples`. `retry_span` restricts the decode to `[start_s, end_s)` of
    /// the same buffer *and* drops prompt carry-over between windows (`n_max_text_ctx = 0`) — that
    /// pairing is what a loop retry needs, and nothing else decodes a span. Whisper's segment
    /// timestamps stay absolute to `samples` either way, so a retry's output splices in as-is.
    fn decode(
        &self,
        samples: &[f32],
        retry_span: Option<Range<f64>>,
    ) -> Result<Vec<AsrSegment>, InferenceError> {
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| InferenceError::Whisper(format!("create state: {e}")))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_entropy_thold(self.entropy_thold);
        params.set_language(Some(self.language.as_str()));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        if let Some(span) = retry_span {
            params.set_offset_ms((span.start * 1000.0) as i32);
            params.set_duration_ms(((span.end - span.start) * 1000.0) as i32);
            // The prompt whisper carries from one 30-s window into the next is what feeds a
            // repetition attractor: whisper.cpp only drops it once a fallback gate fires, and the
            // gates miss a long repeated phrase. Decoding the span with no prompt at all costs a
            // little cross-window context and reliably breaks the loop.
            params.set_n_max_text_ctx(0);
        } else if !self.carry_over {
            params.set_n_max_text_ctx(0);
        }

        state
            .full(params, samples)
            .map_err(|e| InferenceError::Whisper(format!("transcribe: {e}")))?;

        let n = state.full_n_segments();
        let mut segments = Vec::with_capacity(n.max(0) as usize);
        for i in 0..n {
            let Some(segment) = state.get_segment(i) else {
                continue;
            };
            let text = segment
                .to_str_lossy()
                .map_err(|e| InferenceError::Whisper(format!("segment text: {e}")))?
                .trim()
                .to_string();
            segments.push(AsrSegment {
                text,
                start_s: segment.start_timestamp() as f64 / 100.0,
                end_s: segment.end_timestamp() as f64 / 100.0,
            });
        }
        Ok(segments)
    }

    /// Replace every decoder repetition loop in `segments` with an isolated, prompt-free re-decode of
    /// the audio it covers.
    ///
    /// A loop is self-inflicted: whisper primes each 30-s window with the text it just produced, so
    /// once a phrase repeats it keeps winning, and the run only ends when the track does. Re-decoding
    /// just that span with the prompt disabled starts the decoder from the audio alone, which breaks
    /// the attractor and recovers the speech the loop wrote over — verified against the meeting that
    /// produced 122 copies of one sentence, where the same span in isolation came back clean.
    ///
    /// Not every loop is prompt-fed: one that forms inside a single 30-s window repeats under the
    /// re-decode too. So a re-decode that still loops is trusted only when the repeating unit is
    /// short enough to be real speech ([`LOOP_MAX_REAL_WORDS`]); a repeating clause is collapsed to
    /// its first cycle instead. No loops means no extra decoding at all.
    fn repair_loops(
        &self,
        samples: &[f32],
        segments: Vec<AsrSegment>,
    ) -> Result<Vec<AsrSegment>, InferenceError> {
        let runs = find_loop_runs(&segments);
        if runs.is_empty() {
            return Ok(segments);
        }

        let mut replacements = Vec::with_capacity(runs.len());
        for run in &runs {
            let looped = &segments[run.range.clone()];
            replacements.push(self.redecode_loop(samples, looped, run.period)?);
        }
        Ok(splice_runs(&segments, &runs, replacements))
    }

    /// The per-run half of [`repair_loops`]: re-decode `looped`'s audio span in isolation and pick
    /// between the re-decode and the original.
    fn redecode_loop(
        &self,
        samples: &[f32],
        looped: &[AsrSegment],
        period: usize,
    ) -> Result<Vec<AsrSegment>, InferenceError> {
        // A loop typically runs to the end of the track, and whisper's last segment can end a shade
        // past the samples it was given; clamp rather than skip, or the commonest case never retries.
        let track_end_s = samples.len() as f64 / f64::from(SAMPLE_RATE);
        let span = looped[0].start_s..looped[looped.len() - 1].end_s.min(track_end_s);
        let retry = if span.end - span.start >= LOOP_MIN_RETRY_S {
            self.decode(samples, Some(span))?
        } else {
            Vec::new()
        };

        if !retry.is_empty() && find_loop_runs(&retry).is_empty() {
            return Ok(retry);
        }
        Ok(unrepaired_run(looped, period).to_vec())
    }
}

/// Merge the segments' (possibly overlapping, possibly unsorted) spans into disjoint ascending ones.
fn merged_spans(segments: &[AsrSegment]) -> Vec<Range<f64>> {
    let mut spans: Vec<Range<f64>> = segments
        .iter()
        .filter(|s| s.end_s > s.start_s)
        .map(|s| s.start_s..s.end_s)
        .collect();
    spans.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut merged: Vec<Range<f64>> = Vec::with_capacity(spans.len());
    for span in spans {
        match merged.last_mut() {
            Some(last) if span.start <= last.end => last.end = last.end.max(span.end),
            _ => merged.push(span),
        }
    }
    merged
}

/// The complement of the segments' spans within `0..track_s` — the stretches that produced no text.
fn untranscribed_spans(segments: &[AsrSegment], track_s: f64) -> Vec<Range<f64>> {
    let mut gaps = Vec::new();
    let mut cursor = 0.0_f64;
    for span in merged_spans(segments) {
        if span.start > cursor {
            gaps.push(cursor..span.start);
        }
        cursor = cursor.max(span.end);
    }
    if cursor < track_s {
        gaps.push(cursor..track_s);
    }
    gaps
}

/// Total transcribed time (overlaps counted once), for measuring a recovery pass's progress.
fn transcribed_s(segments: &[AsrSegment]) -> f64 {
    merged_spans(segments).iter().map(|s| s.end - s.start).sum()
}

/// Root-mean-square level over `span`, clamped to the buffer. 0.0 for an empty span.
fn span_rms(samples: &[f32], span: &Range<f64>) -> f64 {
    let rate = f64::from(SAMPLE_RATE);
    let start = ((span.start * rate) as usize).min(samples.len());
    let end = ((span.end * rate) as usize).clamp(start, samples.len());
    let window = &samples[start..end];
    if window.is_empty() {
        return 0.0;
    }
    let sum: f64 = window.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum / window.len() as f64).sqrt()
}

/// Audible track time and how much of it landed inside a segment, measured on one-second bins so the
/// ratio stays meaningful (a segment spanning a pause cannot inflate it past the audible total).
fn audible_coverage(samples: &[f32], segments: &[AsrSegment], track_s: f64) -> (f64, f64) {
    let spans = merged_spans(segments);
    let bins = track_s.ceil() as usize;
    let mut audible = 0.0;
    let mut transcribed = 0.0;
    for bin in 0..bins {
        let range = bin as f64..((bin + 1) as f64).min(track_s);
        if span_rms(samples, &range) <= SILENT_RMS {
            continue;
        }
        audible += range.end - range.start;
        if spans
            .iter()
            .any(|s| s.start < range.end && s.end > range.start)
        {
            transcribed += range.end - range.start;
        }
    }
    (audible, transcribed)
}

/// What to keep from a loop run whose isolated re-decode came back no better: the whole run when the
/// repeating unit is short enough to be someone really saying it three times, otherwise just the
/// first cycle. Pure — unit-tested without whisper.
fn unrepaired_run(looped: &[AsrSegment], period: usize) -> &[AsrSegment] {
    let unit_words: usize = looped[..period]
        .iter()
        .map(|segment| segment.text.split_whitespace().count())
        .sum();
    if unit_words > LOOP_MAX_REAL_WORDS {
        &looped[..period]
    } else {
        looped
    }
}

/// Put each run's `replacement` in place of the run it was decoded for.
///
/// The two sides are separate decodes of overlapping audio — whisper ends a window on a speech
/// boundary, not on the sample either side asked for — so the seam is settled by coverage, not by
/// index. Pure — unit-tested without whisper.
fn splice_runs(
    segments: &[AsrSegment],
    runs: &[LoopRun],
    replacements: Vec<Vec<AsrSegment>>,
) -> Vec<AsrSegment> {
    let mut repaired: Vec<AsrSegment> = Vec::with_capacity(segments.len());
    let mut next = 0;
    for (i, (run, mut replacement)) in runs.iter().zip(replacements).enumerate() {
        repaired.extend_from_slice(&segments[next..run.range.start]);
        let run_end_s = segments[run.range.end - 1].end_s;
        next = run.range.end;
        // Each run is replaced from its own audio, so the next run's segments have to survive this
        // splice; without the cap a long replacement walks `next` past them and the slice above
        // indexes backwards.
        let limit = runs
            .get(i + 1)
            .map_or(segments.len(), |next_run| next_run.range.start);
        // First the replacement gives way: a last segment reaching past the run was cut off by the
        // span, so the next original carries that utterance in full. Never drop the whole
        // replacement, and never trim when the run ends the track.
        while next < segments.len()
            && replacement.len() > 1
            && replacement.last().is_some_and(|s| s.end_s > run_end_s)
        {
            replacement.pop();
        }
        // Then the originals give way: the replacement transcribed that audio without the loop
        // drowning it out, so it is the better copy.
        if let Some(replacement_end_s) = replacement.last().map(|s| s.end_s) {
            while next < limit && segments[next].end_s <= replacement_end_s {
                next += 1;
            }
        }
        repaired.extend(replacement);
    }
    repaired.extend_from_slice(&segments[next..]);
    repaired
}

/// One run of consecutive segments whose text repeats with a fixed `period`, covering
/// `range.len() / period` back-to-back cycles.
#[derive(Debug, Clone, PartialEq)]
struct LoopRun {
    range: Range<usize>,
    period: usize,
}

/// Every maximal run of `segments` whose text cycles with a period of at most [`LOOP_MAX_PERIOD`] for
/// at least [`LOOP_MIN_CYCLES`] cycles, in order and non-overlapping.
///
/// This is the backstop for the loops whisper's own gates cannot see: its entropy check looks at the
/// last 32 tokens, so it is blind to a repeated unit that long (a ~29-token sentence repeated 122
/// times scored like ordinary speech), and a repetition attractor is *confident*, so the
/// average-logprob gate passes it too. Repetition across whole segments is the signal that survives
/// both. Pure — unit-tested without whisper.
fn find_loop_runs(segments: &[AsrSegment]) -> Vec<LoopRun> {
    let keys: Vec<String> = segments.iter().map(|s| loop_key(&s.text)).collect();
    let mut runs = Vec::new();
    let mut i = 0;
    while i < keys.len() {
        // An empty (punctuation-only) segment repeating says nothing about the decoder.
        let run = if keys[i].is_empty() {
            None
        } else {
            (1..=LOOP_MAX_PERIOD).find_map(|period| {
                let cycles = count_cycles(&keys, i, period);
                (cycles >= LOOP_MIN_CYCLES).then(|| LoopRun {
                    range: i..i + cycles * period,
                    period,
                })
            })
        };
        match run {
            Some(run) => {
                i = run.range.end;
                runs.push(run);
            }
            None => i += 1,
        }
    }
    runs
}

/// How many back-to-back copies of `keys[start..start + period]` follow it, itself included (so 1
/// means "no repeat"). 0 when the cycle does not fit.
fn count_cycles(keys: &[String], start: usize, period: usize) -> usize {
    if start + period > keys.len() {
        return 0;
    }
    let mut cycles = 1;
    while start + (cycles + 1) * period <= keys.len()
        && keys[start..start + period]
            == keys[start + cycles * period..start + (cycles + 1) * period]
    {
        cycles += 1;
    }
    cycles
}

/// Comparison key for loop detection: lowercased words stripped of punctuation. Whisper varies
/// capitalization and trailing punctuation between copies of a looped phrase, and neither difference
/// makes it any less of a loop.
fn loop_key(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Segments one second apart, so a run's index range doubles as its time span.
    fn segments(texts: &[&str]) -> Vec<AsrSegment> {
        texts
            .iter()
            .enumerate()
            .map(|(i, text)| AsrSegment {
                text: (*text).to_string(),
                start_s: i as f64,
                end_s: i as f64 + 1.0,
            })
            .collect()
    }

    fn runs(texts: &[&str]) -> Vec<(Range<usize>, usize)> {
        find_loop_runs(&segments(texts))
            .into_iter()
            .map(|run| (run.range, run.period))
            .collect()
    }

    #[test]
    fn find_loop_runs_catches_a_repeated_phrase() {
        // The failure this exists for: one sentence, over and over. Long enough that whisper's own
        // 32-token entropy gate cannot see it, which is why the repeat count is what we key on.
        let loop_text = "So I'm okay with going with the original plan of like, okay, well, \
                         we're going to fill in the $800,000.";
        let mut texts = vec!["Maybe that closes the gap."];
        texts.extend(std::iter::repeat_n(loop_text, 5));
        texts.push("In the numbers specifically.");
        assert_eq!(runs(&texts), vec![(1..6, 1)]);
    }

    #[test]
    fn find_loop_runs_catches_a_two_phrase_ping_pong() {
        assert_eq!(runs(&["a", "b", "a", "b", "a", "b"]), vec![(0..6, 2)]);
        // A partial trailing cycle is not part of the run.
        assert_eq!(runs(&["a", "b", "a", "b", "a", "b", "a"]), vec![(0..6, 2)]);
    }

    #[test]
    fn find_loop_runs_prefers_the_tightest_cycle() {
        // "a a a a" is period 1 repeated 4x, not period 2 repeated 2x.
        assert_eq!(runs(&["a", "a", "a", "a"]), vec![(0..4, 1)]);
    }

    #[test]
    fn find_loop_runs_ignores_repetition_below_the_bar() {
        // Two in a row is emphasis, not a loop, at either period.
        assert!(runs(&["yeah", "yeah", "right"]).is_empty());
        assert!(runs(&["a", "b", "a", "b", "c"]).is_empty());
        assert!(runs(&[]).is_empty());
    }

    #[test]
    fn find_loop_runs_matches_across_case_and_punctuation() {
        // Whisper varies both between copies of the same looped phrase.
        assert_eq!(
            runs(&["Okay, well...", "okay well", "OKAY -- WELL!"]),
            vec![(0..3, 1)]
        );
    }

    #[test]
    fn find_loop_runs_reports_separate_runs_without_overlapping() {
        let found = runs(&["a", "a", "a", "keep", "b", "b", "b", "b"]);
        assert_eq!(found, vec![(0..3, 1), (4..8, 1)]);
    }

    #[test]
    fn find_loop_runs_skips_empty_segments() {
        // Punctuation-only segments normalize to nothing; repeating them says nothing about the
        // decoder, and collapsing them would only cost a pointless re-decode.
        assert!(runs(&["...", "?", "--", "real"]).is_empty());
    }

    #[test]
    fn unrepaired_run_collapses_a_repeating_clause_but_keeps_interjections() {
        // The re-decode reproduced the repetition, so this is the last word on it. A whole clause
        // three times over is the decoder (an intra-window loop the prompt-free retry cannot fix)...
        let clause = segments(&[
            "I think that's going to be a big part of the process.",
            "I think that's going to be a big part of the process.",
            "I think that's going to be a big part of the process.",
        ]);
        assert_eq!(unrepaired_run(&clause, 1), &clause[..1]);

        // ...while a short interjection three times over is a person, and survives intact.
        let interjection = segments(&["Yeah.", "Yeah.", "Yeah."]);
        assert_eq!(unrepaired_run(&interjection, 1), &interjection[..]);
    }

    #[test]
    fn unrepaired_run_measures_the_whole_cycle() {
        // A period-2 cycle is judged on both of its segments, not just the first.
        let ping_pong = segments(&[
            "Right, so",
            "that is the thing",
            "Right, so",
            "that is the thing",
            "Right, so",
            "that is the thing",
        ]);
        assert_eq!(unrepaired_run(&ping_pong, 2), &ping_pong[..2]);
    }

    #[test]
    fn count_cycles_counts_whole_cycles_only() {
        let keys: Vec<String> = ["a", "b", "a", "b", "a"]
            .iter()
            .map(|t| loop_key(t))
            .collect();
        assert_eq!(count_cycles(&keys, 0, 2), 2); // the trailing lone "a" is not a cycle
        assert_eq!(count_cycles(&keys, 0, 1), 1); // "a" then "b" — no repeat
        assert_eq!(count_cycles(&keys, 4, 2), 0); // the cycle does not fit
    }

    #[test]
    fn loop_key_normalizes_to_bare_lowercase_words() {
        assert_eq!(loop_key("  Okay,   WELL! "), "okay well");
        assert_eq!(loop_key("..."), "");
        assert_eq!(loop_key("$800,000."), "800000");
    }

    /// A track of one-second segments, so an index doubles as a timestamp.
    fn track(n: usize) -> Vec<AsrSegment> {
        (0..n).map(|i| seg(i as f64, i as f64 + 1.0)).collect()
    }

    fn run(range: Range<usize>) -> LoopRun {
        LoopRun { range, period: 1 }
    }

    #[test]
    fn splice_runs_replaces_a_run_in_place() {
        let spliced = splice_runs(&track(5), &[run(1..4)], vec![vec![seg(1.0, 4.0)]]);
        assert_eq!(spliced, vec![seg(0.0, 1.0), seg(1.0, 4.0), seg(4.0, 5.0)]);
    }

    #[test]
    fn splice_runs_trims_a_replacement_that_overshoots_the_run() {
        // The 3.5-5.2 segment was cut off by the span; segment 4 carries that utterance in full.
        let replacement = vec![seg(1.0, 3.5), seg(3.5, 5.2)];
        let spliced = splice_runs(&track(6), &[run(1..4)], vec![replacement]);
        assert_eq!(
            spliced,
            vec![seg(0.0, 1.0), seg(1.0, 3.5), seg(4.0, 5.0), seg(5.0, 6.0)]
        );
    }

    #[test]
    fn splice_runs_drops_originals_the_replacement_already_covers() {
        let spliced = splice_runs(&track(5), &[run(1..4)], vec![vec![seg(1.0, 5.0)]]);
        assert_eq!(spliced, vec![seg(0.0, 1.0), seg(1.0, 5.0)]);
    }

    #[test]
    fn splice_runs_keeps_a_replacement_that_runs_to_the_end_of_the_track() {
        // Nothing follows the run, so the overshoot is the only copy of that audio.
        let replacement = vec![seg(1.0, 3.0), seg(3.0, 4.6)];
        let spliced = splice_runs(&track(4), &[run(1..4)], vec![replacement]);
        assert_eq!(spliced, vec![seg(0.0, 1.0), seg(1.0, 3.0), seg(3.0, 4.6)]);
    }

    #[test]
    fn splice_runs_never_consumes_the_next_runs_first_segment() {
        // A replacement reaching into the next run used to walk the cursor past that run's start,
        // and the splice then indexed backwards — the refine died on "slice index starts at 178 but
        // ends at 176".
        let spliced = splice_runs(
            &track(9),
            &[run(1..4), run(5..8)],
            vec![vec![seg(1.0, 7.5)], vec![seg(5.0, 8.0)]],
        );
        assert_eq!(
            spliced,
            vec![seg(0.0, 1.0), seg(1.0, 7.5), seg(5.0, 8.0), seg(8.0, 9.0)]
        );
    }

    fn seg(start_s: f64, end_s: f64) -> AsrSegment {
        AsrSegment {
            text: "x".to_string(),
            start_s,
            end_s,
        }
    }

    #[test]
    fn merged_spans_unions_overlapping_and_unsorted_segments() {
        let merged = merged_spans(&[seg(5.0, 8.0), seg(0.0, 2.0), seg(1.0, 3.0), seg(9.0, 9.0)]);
        // 0-2 and 1-3 merge; 5-8 stays separate; the zero-length segment is dropped.
        assert_eq!(merged, vec![0.0..3.0, 5.0..8.0]);
    }

    #[test]
    fn untranscribed_spans_finds_leading_interior_and_tail_gaps() {
        let gaps = untranscribed_spans(&[seg(2.0, 4.0), seg(9.0, 10.0)], 20.0);
        assert_eq!(gaps, vec![0.0..2.0, 4.0..9.0, 10.0..20.0]);
    }

    #[test]
    fn untranscribed_spans_is_empty_for_full_coverage() {
        assert!(untranscribed_spans(&[seg(0.0, 10.0)], 10.0).is_empty());
    }

    #[test]
    fn untranscribed_spans_ignores_segments_past_the_track() {
        // A whisper segment can end past the decoded audio; that must not yield a negative gap.
        assert!(untranscribed_spans(&[seg(0.0, 12.0)], 10.0).is_empty());
    }

    #[test]
    fn span_rms_separates_silence_from_speech() {
        let mut samples = vec![0.0_f32; 16_000];
        samples.extend(std::iter::repeat_n(0.5_f32, 16_000));
        assert!(span_rms(&samples, &(0.0..1.0)) <= SILENT_RMS);
        assert!(span_rms(&samples, &(1.0..2.0)) > SILENT_RMS);
        // Past the buffer is empty, not a panic.
        assert_eq!(span_rms(&samples, &(50.0..60.0)), 0.0);
    }

    #[test]
    fn audible_coverage_counts_only_audible_bins() {
        // 3 s: loud, silent, loud. A segment covers only the first second.
        let mut samples = vec![0.5_f32; 16_000];
        samples.extend(std::iter::repeat_n(0.0_f32, 16_000));
        samples.extend(std::iter::repeat_n(0.5_f32, 16_000));
        let (audible, transcribed) = audible_coverage(&samples, &[seg(0.0, 1.0)], 3.0);
        assert_eq!(audible, 2.0, "the silent middle second is not audible time");
        assert_eq!(transcribed, 1.0);
    }

    #[test]
    fn coverage_fraction_treats_a_silent_track_as_covered() {
        let coverage = Coverage {
            track_s: 10.0,
            audible_s: 0.0,
            transcribed_s: 0.0,
            recovered: Vec::new(),
            unrecovered: Vec::new(),
        };
        assert_eq!(coverage.fraction(), 1.0);
    }

    #[test]
    fn coverage_fraction_reports_the_audible_share() {
        let coverage = Coverage {
            track_s: 100.0,
            audible_s: 80.0,
            transcribed_s: 20.0,
            recovered: Vec::new(),
            unrecovered: Vec::new(),
        };
        assert_eq!(coverage.fraction(), 0.25);
    }
}

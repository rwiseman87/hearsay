//! Audible-vs-transcribed coverage of the Them track: the guard against a silently truncated
//! transcript, since an ASR pass returns success even when it drops a stretch of speech.

use std::ops::Range;

use hearsay_audio::SAMPLE_RATE;

/// RMS below which a second of the track counts as silence, not missed speech.
const SILENT_RMS: f64 = 0.005;

/// Shortest audible run with no transcribed speech that is reported as a stall.
const STALL_MIN_S: f64 = 10.0;

/// What a whole-track transcript covered.
#[derive(Debug, Clone, PartialEq)]
pub struct Coverage {
    pub track_s: f64,
    /// Track time above the silence floor: the time that should yield transcript.
    pub audible_s: f64,
    /// Audible time that landed inside transcribed speech.
    pub transcribed_s: f64,
    /// Audible runs of at least [`STALL_MIN_S`] with no transcribed speech.
    pub uncovered: Vec<Range<f64>>,
}

impl Coverage {
    /// Transcribed share of audible time; a silent track counts as covered.
    pub fn fraction(&self) -> f64 {
        if self.audible_s <= 0.0 {
            return 1.0;
        }
        (self.transcribed_s / self.audible_s).clamp(0.0, 1.0)
    }
}

/// Merge possibly overlapping, possibly unsorted spans into disjoint ascending ones.
fn merged(spans: &[Range<f64>]) -> Vec<Range<f64>> {
    let mut sorted: Vec<Range<f64>> = spans.iter().filter(|s| s.end > s.start).cloned().collect();
    sorted.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<Range<f64>> = Vec::with_capacity(sorted.len());
    for span in sorted {
        match out.last_mut() {
            Some(last) if span.start <= last.end => last.end = last.end.max(span.end),
            _ => out.push(span),
        }
    }
    out
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

/// Measure `speech` (transcribed word spans) against the audible time of `samples`, on one-second bins.
pub fn measure(samples: &[f32], speech: &[Range<f64>]) -> Coverage {
    let track_s = samples.len() as f64 / f64::from(SAMPLE_RATE);
    let spans = merged(speech);
    let mut audible_s = 0.0;
    let mut transcribed_s = 0.0;
    let mut uncovered: Vec<Range<f64>> = Vec::new();
    let mut run: Option<Range<f64>> = None;
    let close_run = |run: &mut Option<Range<f64>>, uncovered: &mut Vec<Range<f64>>| {
        if let Some(finished) = run.take() {
            if finished.end - finished.start >= STALL_MIN_S {
                uncovered.push(finished);
            }
        }
    };
    for bin in 0..(track_s.ceil() as usize) {
        let range = bin as f64..((bin + 1) as f64).min(track_s);
        if span_rms(samples, &range) <= SILENT_RMS {
            close_run(&mut run, &mut uncovered);
            continue;
        }
        audible_s += range.end - range.start;
        let heard = spans
            .iter()
            .any(|s| s.start < range.end && s.end > range.start);
        if heard {
            transcribed_s += range.end - range.start;
            close_run(&mut run, &mut uncovered);
        } else {
            match run.as_mut() {
                Some(open) => open.end = range.end,
                None => run = Some(range.clone()),
            }
        }
    }
    close_run(&mut run, &mut uncovered);
    Coverage {
        track_s,
        audible_s,
        transcribed_s,
        uncovered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loud(seconds: usize) -> Vec<f32> {
        vec![0.5_f32; seconds * SAMPLE_RATE as usize]
    }

    fn quiet(seconds: usize) -> Vec<f32> {
        vec![0.0_f32; seconds * SAMPLE_RATE as usize]
    }

    #[test]
    fn merged_unions_overlapping_and_unsorted_spans() {
        let out = merged(&[5.0..8.0, 0.0..2.0, 1.0..3.0, 9.0..9.0]);
        // 0-2 and 1-3 merge; 5-8 stays separate; the zero-length span is dropped.
        assert_eq!(out, vec![0.0..3.0, 5.0..8.0]);
    }

    #[test]
    fn span_rms_separates_silence_from_speech_and_clamps() {
        let mut samples = quiet(1);
        samples.extend(loud(1));
        assert!(span_rms(&samples, &(0.0..1.0)) <= SILENT_RMS);
        assert!(span_rms(&samples, &(1.0..2.0)) > SILENT_RMS);
        assert_eq!(span_rms(&samples, &(50.0..60.0)), 0.0);
    }

    #[test]
    fn only_audible_bins_count() {
        // 3 s: loud, silent, loud. Speech covers only the first second.
        let mut samples = loud(1);
        samples.extend(quiet(1));
        samples.extend(loud(1));
        let c = measure(&samples, &[0.0..1.0]);
        assert_eq!(
            c.audible_s, 2.0,
            "the silent middle second is not audible time"
        );
        assert_eq!(c.transcribed_s, 1.0);
        assert_eq!(c.fraction(), 0.5);
    }

    #[test]
    fn a_silent_track_counts_as_covered() {
        let c = measure(&quiet(5), &[]);
        assert_eq!(c.fraction(), 1.0);
        assert!(c.uncovered.is_empty());
    }

    #[test]
    fn a_long_audible_gap_is_reported_as_a_stall_but_a_short_one_is_not() {
        // 5 s of speech, a 12 s audible gap, 2 s of speech, a 4 s audible gap.
        let samples = loud(5 + 12 + 2 + 4);
        let c = measure(&samples, &[0.0..5.0, 17.0..19.0]);
        assert_eq!(c.uncovered, vec![5.0..17.0]);
        assert_eq!(c.transcribed_s, 7.0);
    }

    #[test]
    fn speech_past_the_track_does_not_inflate_coverage() {
        let c = measure(&loud(10), &[0.0..30.0]);
        assert_eq!(c.fraction(), 1.0);
    }
}

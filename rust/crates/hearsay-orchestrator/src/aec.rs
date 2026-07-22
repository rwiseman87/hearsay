//! Acoustic echo cancellation on the Me stream, using the Them system-audio tap as the far-end
//! reference. See `docs/echo-cancellation.md`.
//!
//! When the user is on speakers, the mic picks up the system audio and that echo lands in Me. The
//! [`FrameAligner`] turns the two variable-length, gappy capture streams into 160-sample near/far
//! frame pairs on one absolute sample clock; [`EchoCanceller`] drives it and, under the `aec`
//! feature, runs SpeexDSP on each pair. Without the feature it forwards Me unchanged.
//!
//! [`FrameAligner`] is pure and unit-tested; it compiles under `test` even without the `aec` feature
//! so the alignment logic — the correctness-critical part — is covered in the default `cargo test`.

/// SpeexDSP frame size: 10 ms at 16 kHz.
#[cfg(any(feature = "aec", test))]
const FRAME: usize = 160;

/// Contract-fixed capture sample rate (Hz).
#[cfg(any(feature = "aec", test))]
const SAMPLE_RATE: f64 = 16_000.0;

/// Only re-anchor a stream to a chunk's `t0_s` once it diverges past this — a real delivery gap, not
/// per-chunk clock jitter. 0.2 s at 16 kHz, matching the recorder's `RESYNC_GAP`.
#[cfg(any(feature = "aec", test))]
const RESYNC_GAP: usize = 3200;

/// The most Me the aligner holds waiting for a far-end reference before emitting it with a
/// zero-filled (silent) reference. A silent far-end produces no tap chunks, so this bound is what
/// stops Me stalling when nothing is playing — and cancelling against silence is a near-passthrough,
/// which is correct because a silent far-end means no echo. 0.2 s at 16 kHz.
#[cfg(any(feature = "aec", test))]
const MAX_REF_HOLD: u64 = 3200;

/// Cap on a single forward zero-fill, so a non-monotonic `host_ts` (sleep/resume, a garbage
/// timestamp) can't drive a multi-GB allocation. 5 min of 16 kHz mono — far beyond any real gap, so
/// it never fires in normal capture. Mirrors the recorder's `MAX_FORWARD_JUMP_FRAMES`.
#[cfg(any(feature = "aec", test))]
const MAX_FORWARD_FILL: usize = 5 * 60 * 16_000;

/// The most far-end reference buffered ahead of the emission frontier. Far only runs this far ahead
/// when the Me stream has stalled (mic device loss mid-meeting) — frames are emitted at Me's pace,
/// so nothing consumes the reference until Me returns. Those stalled frames can only ever emit with
/// a zero-filled near side, whose cancelled output is silence regardless of the reference, so the
/// oldest reference past the cap is dropped rather than held: a mic stall costs a fixed 128 KB
/// instead of ~230 MB/h. 2 s at 16 kHz — 10x `RESYNC_GAP`, so legitimate cross-stream delivery skew
/// never trips it.
#[cfg(any(feature = "aec", test))]
const MAX_FAR_BUFFER: u64 = 2 * 16_000;

/// One stream's samples, placed contiguously from `base` on the shared absolute sample clock. Gaps
/// are zero-filled so `near` and `far` stay index-aligned; consumed samples are dropped from the
/// front as `base` advances.
#[cfg(any(feature = "aec", test))]
struct StreamBuf {
    /// Absolute sample index of `buf[0]`.
    base: u64,
    buf: Vec<f32>,
    /// Logical continuation point of the last pushed chunk (absolute index), for gap resync.
    cursor: Option<u64>,
}

#[cfg(any(feature = "aec", test))]
impl StreamBuf {
    fn new() -> Self {
        StreamBuf {
            base: 0,
            buf: Vec::new(),
            cursor: None,
        }
    }

    /// One past the last buffered sample (absolute index).
    fn end(&self) -> u64 {
        self.base + self.buf.len() as u64
    }

    /// Place `samples` (starting at meeting time `t0_s`) into the buffer. Written contiguously from
    /// the running cursor; a chunk only jumps to `round(t0_s * rate)` when that diverges past
    /// `RESYNC_GAP`. Samples that fall before `base` (already consumed) or overlap buffered data are
    /// dropped; a forward gap is zero-filled.
    fn push(&mut self, t0_s: f64, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let target = (t0_s * SAMPLE_RATE).round().max(0.0) as u64;
        let start = match self.cursor {
            None => {
                self.base = target;
                target
            }
            Some(cur) => {
                if target.abs_diff(cur) as usize > RESYNC_GAP {
                    target
                } else {
                    cur
                }
            }
        };
        self.cursor = Some(start + samples.len() as u64);

        let mut start = start;
        let mut samples = samples;
        if start < self.base {
            let skip = (self.base - start) as usize;
            if skip >= samples.len() {
                return;
            }
            samples = &samples[skip..];
            start = self.base;
        }
        let end = self.end();
        if start > end {
            let fill = ((start - end) as usize).min(MAX_FORWARD_FILL);
            self.buf.resize(self.buf.len() + fill, 0.0);
        } else if start < end {
            let over = (end - start) as usize;
            if over >= samples.len() {
                return;
            }
            samples = &samples[over..];
        }
        self.buf.extend_from_slice(samples);
    }

    /// The 160-sample frame at absolute index `idx`, zero-filled where the buffer does not cover it
    /// (before `base` or past `end`).
    fn frame_at(&self, idx: u64) -> [f32; FRAME] {
        let mut out = [0.0f32; FRAME];
        for (k, slot) in out.iter_mut().enumerate() {
            let abs = idx + k as u64;
            if abs >= self.base {
                if let Some(&v) = self.buf.get((abs - self.base) as usize) {
                    *slot = v;
                }
            }
        }
        out
    }

    /// Drop all samples with absolute index below `idx`, advancing `base`.
    fn consume_to(&mut self, idx: u64) {
        if idx <= self.base {
            return;
        }
        if idx >= self.end() {
            self.buf.clear();
        } else {
            self.buf.drain(0..(idx - self.base) as usize);
        }
        self.base = idx;
    }
}

/// A near/far frame pair ready for cancellation, both starting at absolute sample index `start`.
#[cfg(any(feature = "aec", test))]
struct AlignedFrame {
    start: u64,
    near: [f32; FRAME],
    far: [f32; FRAME],
}

/// Aligns the Me (near) and Them (far) streams into contiguous 160-sample frame pairs. Frames are
/// emitted at Me's pace: a Me frame is released once the far buffer covers it, or once Me has run
/// [`MAX_REF_HOLD`] ahead of the far end (then the far frame is zero-filled).
#[cfg(any(feature = "aec", test))]
struct FrameAligner {
    near: StreamBuf,
    far: StreamBuf,
    /// Absolute index of the next frame to emit; `None` until the first Me sample arrives.
    next: Option<u64>,
}

#[cfg(any(feature = "aec", test))]
impl FrameAligner {
    fn new() -> Self {
        FrameAligner {
            near: StreamBuf::new(),
            far: StreamBuf::new(),
            next: None,
        }
    }

    fn push_near(&mut self, t0_s: f64, samples: &[f32]) {
        if self.next.is_none() && self.near.cursor.is_none() && !samples.is_empty() {
            self.next = Some((t0_s * SAMPLE_RATE).round().max(0.0) as u64);
        }
        self.near.push(t0_s, samples);
    }

    fn push_far(&mut self, t0_s: f64, samples: &[f32]) {
        self.far.push(t0_s, samples);
        // Keep only the trailing MAX_FAR_BUFFER of reference. Trimming moves `base` forward, so a
        // frame over the dropped region reads a zero-filled far side — indistinguishable output,
        // since far only outruns the frontier this much when Me is stalled (zero near).
        self.far
            .consume_to(self.far.end().saturating_sub(MAX_FAR_BUFFER));
    }

    /// Emit every frame that is ready: near covers `[next, next+FRAME)` and either the far buffer
    /// reaches `next+FRAME` or Me has held `MAX_REF_HOLD` past the far end.
    fn drain(&mut self) -> Vec<AlignedFrame> {
        let mut out = Vec::new();
        let Some(mut next) = self.next else {
            return out;
        };
        while self.near.end() >= next + FRAME as u64 {
            let far_ready = self.far.end() >= next + FRAME as u64;
            let hold_tripped = self.near.end().saturating_sub(next) >= MAX_REF_HOLD;
            if !far_ready && !hold_tripped {
                break;
            }
            out.push(AlignedFrame {
                start: next,
                near: self.near.frame_at(next),
                far: self.far.frame_at(next),
            });
            next += FRAME as u64;
            self.near.consume_to(next);
            self.far.consume_to(next);
        }
        self.next = Some(next);
        out
    }
}

/// MDF adaptive-filter tail in samples: the longest playout + acoustic echo delay the canceller
/// can model. The `AecConfig` default (1600, 100 ms) covers wired speakers (10-40 ms of playout
/// latency) but not Bluetooth / AirPlay output, which buffers 150-300 ms — past the tail the
/// filter never converges and the echo passes through untouched. 300 ms at 16 kHz; the cost is
/// linear in the tail and trivial at 16 kHz mono.
#[cfg(feature = "aec")]
const FILTER_TAIL: i32 = 4800;

#[cfg(feature = "aec")]
mod cancel {
    use super::{FrameAligner, FILTER_TAIL, FRAME, SAMPLE_RATE};
    use aec_rs::{Aec, AecConfig};

    /// The Speex echo state holds raw C pointers, so it is not `Send` by default. `demux` owns the
    /// canceller exclusively and Tokio never polls that future from two threads at once, so
    /// transferring it between worker threads is sound.
    struct SendAec(Aec);
    unsafe impl Send for SendAec {}

    pub(crate) struct EchoCanceller {
        aligner: FrameAligner,
        aec: SendAec,
    }

    impl EchoCanceller {
        pub(crate) fn new() -> Self {
            EchoCanceller {
                aligner: FrameAligner::new(),
                aec: SendAec(Aec::new(&AecConfig {
                    frame_size: FRAME,
                    filter_length: FILTER_TAIL,
                    sample_rate: SAMPLE_RATE as u32,
                    enable_preprocess: true,
                })),
            }
        }

        /// Buffer a Them chunk as the far-end reference; return any Me now ready to forward.
        pub(crate) fn push_far(&mut self, t0_s: f64, samples: &[f32]) -> Vec<(f64, Vec<f32>)> {
            self.aligner.push_far(t0_s, samples);
            self.drain_cancel()
        }

        /// Buffer a Me chunk (near end); return the cleaned Me now ready to forward.
        pub(crate) fn process_me(&mut self, t0_s: f64, samples: &[f32]) -> Vec<(f64, Vec<f32>)> {
            self.aligner.push_near(t0_s, samples);
            self.drain_cancel()
        }

        fn drain_cancel(&mut self) -> Vec<(f64, Vec<f32>)> {
            let frames = self.aligner.drain();
            if frames.is_empty() {
                return Vec::new();
            }
            let start0 = frames[0].start;
            let mut cleaned = Vec::with_capacity(frames.len() * FRAME);
            for f in &frames {
                let near = to_i16(&f.near);
                let far = to_i16(&f.far);
                let mut out = [0i16; FRAME];
                self.aec.0.cancel_echo(&near, &far, &mut out);
                cleaned.extend(out.iter().map(|&s| s as f32 / 32768.0));
            }
            vec![(start0 as f64 / SAMPLE_RATE, cleaned)]
        }
    }

    fn to_i16(frame: &[f32; FRAME]) -> [i16; FRAME] {
        let mut out = [0i16; FRAME];
        for (o, &s) in out.iter_mut().zip(frame.iter()) {
            *o = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        }
        out
    }
}

#[cfg(feature = "aec")]
pub(crate) use cancel::EchoCanceller;

/// No-op canceller when the `aec` feature is off: Me passes through unchanged, Them is ignored.
#[cfg(not(feature = "aec"))]
pub(crate) struct EchoCanceller;

#[cfg(not(feature = "aec"))]
impl EchoCanceller {
    pub(crate) fn new() -> Self {
        EchoCanceller
    }

    pub(crate) fn push_far(&mut self, _t0_s: f64, _samples: &[f32]) -> Vec<(f64, Vec<f32>)> {
        Vec::new()
    }

    pub(crate) fn process_me(&mut self, t0_s: f64, samples: &[f32]) -> Vec<(f64, Vec<f32>)> {
        vec![(t0_s, samples.to_vec())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(n: usize, start: f32) -> Vec<f32> {
        (0..n).map(|i| (start + i as f32) / 100_000.0).collect()
    }

    #[test]
    fn frames_emit_once_far_catches_up() {
        let mut a = FrameAligner::new();
        // Near arrives first; far not yet present and under the hold bound -> nothing ready.
        a.push_near(0.0, &ramp(FRAME, 0.0));
        assert!(a.drain().is_empty(), "no far yet, within hold");
        // Far arrives covering the frame -> exactly one aligned frame.
        a.push_far(0.0, &ramp(FRAME, 1000.0));
        let frames = a.drain();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].start, 0);
        assert_eq!(frames[0].near[0], ramp(FRAME, 0.0)[0]);
        assert_eq!(frames[0].far[10], ramp(FRAME, 1000.0)[10]);
    }

    #[test]
    fn near_passes_through_when_far_silent_past_hold() {
        let mut a = FrameAligner::new();
        // Push more than MAX_REF_HOLD of near with no far at all.
        let n = (MAX_REF_HOLD as usize) + 3 * FRAME;
        a.push_near(0.0, &ramp(n, 0.0));
        let frames = a.drain();
        // Everything up to the hold frontier is released with a zero (silent) far reference.
        assert!(
            !frames.is_empty(),
            "hold bound must release Me without a reference"
        );
        assert!(frames.iter().all(|f| f.far.iter().all(|&s| s == 0.0)));
        // Frames are contiguous.
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(f.start, (i * FRAME) as u64);
        }
    }

    #[test]
    fn far_stopping_leaves_later_frames_with_silent_reference() {
        let mut a = FrameAligner::new();
        // Far covers only frame 0, then stops; near keeps flowing well past the hold bound.
        a.push_far(0.0, &vec![0.25f32; FRAME]);
        a.push_near(0.0, &vec![0.5f32; MAX_REF_HOLD as usize + 2 * FRAME]);
        let frames = a.drain();
        assert!(
            frames[0].far.iter().all(|&s| s == 0.25),
            "frame 0 uses the real reference"
        );
        assert!(
            frames[1..].iter().all(|f| f.far.iter().all(|&s| s == 0.0)),
            "once far stops, later frames fall back to a silent reference"
        );
        // Near is preserved and every emitted frame is contiguous.
        assert!(frames.iter().all(|f| f.near.iter().all(|&s| s == 0.5)));
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(f.start, (i * FRAME) as u64);
        }
    }

    #[test]
    fn per_chunk_jitter_does_not_punch_holes() {
        let mut a = FrameAligner::new();
        // Two near chunks whose t0_s rounding is off by a hair (< RESYNC_GAP): the second continues
        // from the cursor rather than re-anchoring, so no gap or overlap appears.
        a.push_near(0.0, &vec![0.5f32; FRAME]);
        let jittered = (FRAME as f64 / SAMPLE_RATE) + 0.0005; // ~8 samples of jitter, under RESYNC_GAP
        a.push_near(jittered, &vec![0.5f32; FRAME]);
        a.push_far(0.0, &vec![0.1f32; 2 * FRAME]);
        let frames = a.drain();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].start, 0);
        assert_eq!(
            frames[1].start, FRAME as u64,
            "no jitter hole between contiguous chunks"
        );
        assert!(frames.iter().all(|f| f.near.iter().all(|&s| s == 0.5)));
    }

    #[test]
    fn far_backlog_is_bounded_while_me_stalls() {
        let mut a = FrameAligner::new();
        // One Me frame, then the mic stalls while the tap keeps flowing for a minute.
        a.push_near(0.0, &vec![0.5f32; FRAME]);
        a.push_far(0.0, &vec![0.1f32; 1600]);
        a.drain();
        for i in 1..600 {
            a.push_far(i as f64 * 0.1, &vec![0.1f32; 1600]);
            a.drain();
        }
        assert!(
            (a.far.buf.len() as u64) <= MAX_FAR_BUFFER,
            "far backlog {} exceeds the cap",
            a.far.buf.len()
        );

        // Me resumes at the far frontier (both streams share the clock): the retained reference is
        // exactly what the resumed frames need.
        let resume_idx = 600 * 1600u64; // 60 s
        a.push_near(60.0, &vec![0.5f32; FRAME]);
        a.push_far(60.0, &vec![0.1f32; 1600]);
        let frames = a.drain();
        let resumed = frames
            .iter()
            .find(|f| f.start == resume_idx)
            .expect("frame at the resume index");
        assert!(resumed.near.iter().all(|&s| s == 0.5), "resumed Me intact");
        assert!(
            resumed.far.iter().all(|&s| s == 0.1),
            "resumed frame pairs with the retained (untrimmed) reference"
        );
        // A frame deep in the stall gap reads a zero-filled far side — its near side is zero-filled
        // too, so the trim changed nothing observable.
        let stalled = frames
            .iter()
            .find(|f| f.start == 10 * FRAME as u64)
            .expect("frame in the stall gap");
        assert!(stalled.near.iter().all(|&s| s == 0.0));
        assert!(stalled.far.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn real_gap_reanchors_to_timestamp() {
        let mut a = FrameAligner::new();
        a.push_near(0.0, &vec![0.5f32; FRAME]);
        // A delivery gap larger than RESYNC_GAP: the next chunk re-anchors to its t0_s, and the gap
        // is zero-filled so the real frame lands at its true index.
        let resume = 2.0 * RESYNC_GAP as f64 / SAMPLE_RATE;
        a.push_near(resume, &vec![0.5f32; FRAME]);
        a.push_far(0.0, &vec![0.1f32; 4 * RESYNC_GAP]);
        let frames = a.drain();
        assert_eq!(frames.first().unwrap().start, 0);
        let resume_idx = 2 * RESYNC_GAP as u64;
        let resumed = frames
            .iter()
            .find(|f| f.start == resume_idx)
            .expect("frame at re-anchor");
        assert!(
            resumed.near.iter().all(|&s| s == 0.5),
            "resumed near preserved at true index"
        );
        // The zero-filled gap between the two real near windows is silent.
        let gap = frames.iter().find(|f| f.start == FRAME as u64).unwrap();
        assert!(gap.near.iter().all(|&s| s == 0.0));
    }
}

/// End-to-end check that Speex actually removes echo (requires the `aec` feature: the real binding).
#[cfg(all(test, feature = "aec"))]
mod cancel_tests {
    use super::*;

    /// A broadband deterministic reference (a sum of incommensurate sines) — echo cancellers adapt
    /// far better on broadband excitation than on a single tone.
    fn reference(idx: f64) -> f32 {
        (0.4 * ((idx * 0.11).sin() + (idx * 0.37).sin() + (idx * 0.93).sin()) / 3.0) as f32
    }

    #[test]
    fn attenuates_a_pure_echo_of_the_reference() {
        let mut ec = EchoCanceller::new();
        let frames = 600usize; // 6 s — ample for the MDF filter to converge
        let tail = 120usize; // measure only the adapted tail (last ~1.2 s)
        let mut echo_energy = 0.0f64;
        let mut residual_energy = 0.0f64;
        for i in 0..frames {
            let t0 = i as f64 * FRAME as f64 / SAMPLE_RATE;
            let mut far = [0.0f32; FRAME];
            let mut near = [0.0f32; FRAME];
            for k in 0..FRAME {
                let s = reference((i * FRAME + k) as f64);
                far[k] = s;
                near[k] = 0.6 * s; // near is a pure, scaled echo of the reference (no local speech)
            }
            ec.push_far(t0, &far);
            let cleaned = ec.process_me(t0, &near);
            if i >= frames - tail {
                for &x in &near {
                    echo_energy += (x as f64) * (x as f64);
                }
                for (_, buf) in &cleaned {
                    for &x in buf {
                        residual_energy += (x as f64) * (x as f64);
                    }
                }
            }
        }
        let ratio = residual_energy / echo_energy.max(1e-12);
        assert!(
            ratio < 0.5,
            "expected the echo to be attenuated; residual/echo ratio = {ratio}"
        );
    }

    /// Deterministic white-ish noise. The delayed-echo test cannot use [`reference`]: delaying a
    /// periodic (line-spectrum) signal is only a phase shift, which a filter of *any* tail length
    /// can reproduce — an aperiodic reference is what makes a past-the-tail delay uncancellable.
    fn noise_signal(len: usize) -> Vec<f32> {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                // Full 32 high bits, so the noise is zero-mean: a DC bias is cancellable at any
                // tail length and would dominate the energy ratio.
                (f64::from((state >> 32) as u32) / f64::from(u32::MAX) - 0.5) as f32 * 0.5
            })
            .collect()
    }

    #[test]
    fn the_filter_tail_covers_bluetooth_playout_delay() {
        // 150 ms of playout latency (a Bluetooth / AirPlay speaker): past the 100 ms `AecConfig`
        // default tail, inside FILTER_TAIL. Drives the raw echo state with the preprocessor OFF —
        // the chained residual suppressor is nonlinear and crushes a stationary noise residual even
        // when the filter modeled nothing, which would mask a too-short tail (this test fails at
        // filter_length 1600, the crate default).
        let aec = aec_rs::Aec::new(&aec_rs::AecConfig {
            frame_size: FRAME,
            filter_length: FILTER_TAIL,
            sample_rate: SAMPLE_RATE as u32,
            enable_preprocess: false,
        });
        let delay = 2400usize;
        let frames = 1200usize; // 12 s — the longer tail adapts more slowly than the pure-echo case
        let tail = 200usize;
        let signal = noise_signal(frames * FRAME);
        let to_i16 = |s: f32| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        let mut echo_energy = 0.0f64;
        let mut residual_energy = 0.0f64;
        for i in 0..frames {
            let mut far = [0i16; FRAME];
            let mut near = [0i16; FRAME];
            for k in 0..FRAME {
                let idx = i * FRAME + k;
                far[k] = to_i16(signal[idx]);
                near[k] = if idx >= delay {
                    to_i16(0.6 * signal[idx - delay])
                } else {
                    0
                };
            }
            let mut out = [0i16; FRAME];
            aec.cancel_echo(&near, &far, &mut out);
            if i >= frames - tail {
                for &x in &near {
                    echo_energy += f64::from(x) * f64::from(x);
                }
                for &x in &out {
                    residual_energy += f64::from(x) * f64::from(x);
                }
            }
        }
        let ratio = residual_energy / echo_energy.max(1e-12);
        assert!(
            ratio < 0.5,
            "expected the delayed echo to be attenuated; residual/echo ratio = {ratio}"
        );
    }

    #[test]
    fn forwards_me_without_stalling_when_the_reference_is_silent() {
        let mut ec = EchoCanceller::new();
        // No far pushed at all (system audio idle). Me must still be forwarded via the hold bound
        // rather than stalling forever waiting for a reference that never comes.
        let mut forwarded = 0usize;
        let mut energy = 0.0f64;
        for i in 0..40 {
            let t0 = i as f64 * FRAME as f64 / SAMPLE_RATE;
            let mut near = [0.0f32; FRAME];
            for (k, s) in near.iter_mut().enumerate() {
                *s = reference((i * FRAME + k) as f64);
            }
            for (_, buf) in ec.process_me(t0, &near) {
                forwarded += buf.len();
                for &x in &buf {
                    energy += (x as f64) * (x as f64);
                }
            }
        }
        assert!(
            forwarded > 0,
            "Me stalled: nothing forwarded with a silent reference"
        );
        assert!(energy > 0.0, "forwarded Me is entirely silent");
    }
}

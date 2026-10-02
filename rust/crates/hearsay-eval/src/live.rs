//! Drives a live Swift sidecar over its stdio protocol (`<u32 LE n><n x f32 LE>` frames in, NDJSON out)
//! and records every emitted segment with its wall-clock arrival.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::SAMPLE_RATE;

/// Samples per stdin frame (40 ms), well under the sidecar's frame cap.
const FRAME_SAMPLES: usize = 640;
/// Silence appended after the audio so the sidecar finalizes the last utterance.
const TAIL_SILENCE_S: usize = 3;
const READY_TIMEOUT: Duration = Duration::from_secs(300);
const EXIT_TIMEOUT: Duration = Duration::from_secs(300);

/// One segment a sidecar emitted.
#[derive(Debug, Clone, Serialize)]
pub struct LiveSegment {
    /// `partial` or `final`.
    pub kind: String,
    pub text: String,
    /// Audio time the segment covers, seconds from the first fed sample.
    pub start_s: f64,
    pub end_s: f64,
    /// Diarizer speaker slot (`hearsay-live` only).
    pub speaker: Option<i64>,
    /// Wall-clock seconds from the first fed sample to this segment's arrival.
    pub arrival_wall_s: f64,
}

#[derive(Debug, Serialize)]
pub struct LiveRun {
    /// Seconds from spawn until the sidecar reported its models loaded.
    pub ready_s: f64,
    /// Wall-clock seconds spent feeding audio (about the audio length at `speed` 1.0).
    pub feed_wall_s: f64,
    pub segments: Vec<LiveSegment>,
    pub exit_success: bool,
}

/// Feed `samples` (16 kHz mono) to `binary` and collect what it emits.
///
/// `speed` paces the feed: 1.0 is real time, 2.0 twice as fast, and 0.0 feeds as fast as the pipe
/// accepts. Latency is only meaningful at the pace the product runs at, so use 1.0 for that.
pub fn run_sidecar(binary: &Path, samples: &[f32], speed: f64) -> Result<LiveRun, String> {
    let spawned = Instant::now();
    let mut child = Command::new(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", binary.display()))?;
    let mut stdin = child.stdin.take().ok_or("sidecar stdin not piped")?;
    let stdout = child.stdout.take().ok_or("sidecar stdout not piped")?;

    let (ready_tx, ready_rx) = mpsc::channel::<Instant>();
    let reader = thread::spawn(move || {
        let mut lines: Vec<(Instant, serde_json::Value)> = Vec::new();
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let at = Instant::now();
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if value.get("ready").and_then(|v| v.as_bool()) == Some(true) {
                let _ = ready_tx.send(at);
            } else {
                lines.push((at, value));
            }
        }
        lines
    });

    let ready_at = ready_rx
        .recv_timeout(READY_TIMEOUT)
        .map_err(|_| "sidecar never reported ready".to_string())?;
    let ready_s = ready_at.duration_since(spawned).as_secs_f64();

    let feed_start = Instant::now();
    let mut padded = samples.to_vec();
    padded.resize(samples.len() + TAIL_SILENCE_S * SAMPLE_RATE, 0.0);
    for (index, frame) in padded.chunks(FRAME_SAMPLES).enumerate() {
        if speed > 0.0 {
            let due = feed_start
                + Duration::from_secs_f64(
                    (index * FRAME_SAMPLES) as f64 / SAMPLE_RATE as f64 / speed,
                );
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                thread::sleep(wait);
            }
        }
        let mut bytes = Vec::with_capacity(4 + frame.len() * 4);
        bytes.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        for sample in frame {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        stdin
            .write_all(&bytes)
            .map_err(|e| format!("write to sidecar: {e}"))?;
    }
    let feed_wall_s = feed_start.elapsed().as_secs_f64();
    drop(stdin);

    let deadline = Instant::now() + EXIT_TIMEOUT;
    let status = loop {
        match child.try_wait().map_err(|e| format!("wait: {e}"))? {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => thread::sleep(Duration::from_millis(100)),
        }
    };
    let lines = reader
        .join()
        .map_err(|_| "reader thread panicked".to_string())?;

    let segments = lines
        .into_iter()
        .filter_map(|(at, value)| {
            Some(LiveSegment {
                kind: value.get("kind")?.as_str()?.to_string(),
                text: value.get("text")?.as_str()?.to_string(),
                start_s: value.get("start_s")?.as_f64()?,
                end_s: value.get("end_s")?.as_f64()?,
                speaker: value.get("speaker").and_then(|v| v.as_i64()),
                arrival_wall_s: at.saturating_duration_since(feed_start).as_secs_f64(),
            })
        })
        .collect();
    Ok(LiveRun {
        ready_s,
        feed_wall_s,
        segments,
        exit_success: status.is_some_and(|s| s.success()),
    })
}

/// Seconds each final lagged the audio it covers: its wall-clock arrival, converted to audio time at
/// `speed`, minus where its audio ended. Meaningful only when the feed was paced (`speed` > 0).
pub fn final_delays_s(segments: &[LiveSegment], speed: f64) -> Vec<f64> {
    segments
        .iter()
        .filter(|s| s.kind == "final")
        .map(|s| s.arrival_wall_s * speed - s.end_s)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(kind: &str, end_s: f64, arrival_wall_s: f64) -> LiveSegment {
        LiveSegment {
            kind: kind.to_string(),
            text: String::new(),
            start_s: 0.0,
            end_s,
            speaker: None,
            arrival_wall_s,
        }
    }

    #[test]
    fn final_delay_is_arrival_in_audio_time_minus_segment_end() {
        let segments = vec![
            segment("partial", 1.0, 1.5),
            segment("final", 2.0, 3.5),
            segment("final", 10.0, 5.5),
        ];
        // At 2x pace, wall 3.5s is audio 7.0s and wall 5.5s is audio 11.0s.
        assert_eq!(final_delays_s(&segments, 2.0), vec![5.0, 1.0]);
        assert_eq!(final_delays_s(&segments, 1.0), vec![1.5, -4.5]);
    }
}

//! The production [`Transcriber`]: a `tokio::process` sidecar spoken to over stdio, byte-for-byte
//! with `src/hearsay/transcript/live_base.py`. Feed frames are `<u32 LE sample count><f32 LE
//! samples>` on stdin; the sidecar emits one NDJSON [`SidecarSegment`] per line on stdout.
//!
//! Used once `hearsay-inference` ships the sidecar binaries. The framing + parsing are unit-tested
//! here; end-to-end spawning is exercised via a real sidecar (or the scripted fake in
//! [`crate::testing`]).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::error::OrchestratorError;
use crate::traits::Transcriber;
use crate::types::SidecarSegment;

/// Owns one streaming sidecar process for a meeting.
pub struct ProcessTranscriber {
    binary: PathBuf,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    reader: Option<JoinHandle<()>>,
    broken: bool,
}

impl ProcessTranscriber {
    /// A transcriber backed by the sidecar at `binary` (e.g. `hearsay-live` / `hearsay-me`).
    pub fn new(binary: PathBuf) -> Self {
        ProcessTranscriber {
            binary,
            child: None,
            stdin: None,
            reader: None,
            broken: false,
        }
    }
}

/// Frame one PCM chunk for a sidecar's stdin: `<u32 LE count><count * f32 LE>`.
fn encode_feed(samples: &[f32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + samples.len() * 4);
    buf.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    for s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    buf
}

/// Read NDJSON segments from the sidecar's stdout, forwarding each parsed line to `tx`. Undecodable
/// lines are skipped (matching the Python read loop). The channel closes when stdout hits EOF.
async fn read_loop(stdout: tokio::process::ChildStdout, tx: mpsc::UnboundedSender<SidecarSegment>) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        match serde_json::from_str::<SidecarSegment>(&line) {
            Ok(seg) => {
                if tx.send(seg).is_err() {
                    break; // pipeline dropped the receiver
                }
            }
            Err(_) => continue,
        }
    }
}

#[async_trait]
impl Transcriber for ProcessTranscriber {
    async fn start(
        &mut self,
    ) -> Result<mpsc::UnboundedReceiver<SidecarSegment>, OrchestratorError> {
        let mut child = Command::new(&self.binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| OrchestratorError::Backend("sidecar stdout not piped".into()))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let reader = tokio::spawn(read_loop(stdout, tx));
        self.child = Some(child);
        self.stdin = stdin;
        self.reader = Some(reader);
        Ok(rx)
    }

    async fn feed(&mut self, samples: Vec<f32>) {
        if self.broken {
            return;
        }
        let Some(stdin) = self.stdin.as_mut() else {
            return;
        };
        if let Err(err) = stdin.write_all(&encode_feed(&samples)).await {
            // The sidecar exited/crashed; stop feeding a dead pipe so it never fails the meeting.
            // Segments it already emitted are kept.
            self.broken = true;
            tracing::warn!(
                sidecar = %self.binary.display(),
                error = %err,
                "sidecar pipe closed mid-meeting; live transcription stopped",
            );
        }
    }

    async fn close(&mut self) {
        // Drop stdin -> EOF: the sidecar finalizes its tail then exits.
        self.stdin.take();
        if let Some(reader) = self.reader.take() {
            let _ = reader.await; // drains the finalized tail into the segment channel
        }
        if let Some(mut child) = self.child.take() {
            match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
                Ok(Ok(status)) if !status.success() => {
                    tracing::warn!(sidecar = %self.binary.display(), ?status, "sidecar exited non-zero");
                }
                Ok(_) => {}
                Err(_) => {
                    let _ = child.kill().await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_feed_frames_count_then_samples_le() {
        let bytes = encode_feed(&[0.0, 1.0]);
        // 4-byte LE count == 2, then two LE f32.
        assert_eq!(&bytes[0..4], &2u32.to_le_bytes());
        assert_eq!(&bytes[4..8], &0.0f32.to_le_bytes());
        assert_eq!(&bytes[8..12], &1.0f32.to_le_bytes());
        assert_eq!(bytes.len(), 4 + 2 * 4);
    }

    #[test]
    fn encode_feed_empty_is_just_a_zero_count() {
        assert_eq!(encode_feed(&[]), 0u32.to_le_bytes());
    }
}

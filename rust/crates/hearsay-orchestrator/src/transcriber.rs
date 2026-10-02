//! The production [`Transcriber`]: a `tokio::process` sidecar spoken to over stdio, byte-for-byte
//! with the Swift live sidecars' stdin contract (`hearsay-{live,me}` via `SidecarIO`). Feed
//! frames are `<u32 LE sample count><f32 LE samples>` on stdin; the sidecar emits one NDJSON
//! [`SidecarSegment`] per line on stdout.
//!
//! Used in production to drive the Swift live sidecars (shipped from the `helper/` package). The framing + parsing are unit-tested
//! here; end-to-end spawning is exercised via a real sidecar (or the scripted fake in
//! [`crate::testing`]).

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::error::OrchestratorError;
use crate::traits::{Transcriber, RESPAWN_BACKOFF};
use crate::types::SidecarSegment;

/// Deadline for each stage of a sidecar shutdown (draining stdout, then reaping the child). A
/// wedged sidecar that never closes stdout or never exits must not hang meeting stop.
const SIDECAR_CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// Capacity of the sidecar's segment (`emit`) channel. The read loop awaits on a full channel, so a
/// stalled consumer (e.g. a DB write backlog) backpressures the sidecar through its blocked stdout
/// write instead of letting segments — including finals, which must never be dropped — accumulate
/// without bound. Segments are far lower-rate than the PCM hand-off, so a few hundred slots
/// is generous headroom a real burst never reaches; it only fills under a sustained stall.
pub const SEGMENT_CHANNEL_CAPACITY: usize = 256;

/// Owns one streaming sidecar process for a meeting.
pub struct ProcessTranscriber {
    binary: PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
    respawn_backoff: Vec<Duration>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    reader: Option<JoinHandle<()>>,
    broken: bool,
    close_timeout: Duration,
    /// Set by [`spawn_warming`](Self::spawn_warming): the process is already spawned (its models
    /// loading in the background, or already loaded) and this holds the segment receiver produced
    /// then, so [`start`](Transcriber::start) adopts the running process instead of respawning —
    /// keeping the model load off the meeting-start path. `None` for a cold transcriber, which
    /// spawns on `start`.
    warmed_rx: Option<mpsc::Receiver<SidecarSegment>>,
    /// The one-shot the read loop fires on the sidecar's models-ready marker, handed to the pipeline
    /// via [`ready_signal`](Transcriber::ready_signal) so it can surface a warm-up notice until the
    /// load finishes. Set by both the cold [`start`](Transcriber::start) and
    /// [`spawn_warming`](Self::spawn_warming); `None` once taken (or if the sidecar was already
    /// serving when adopted).
    ready_rx: Option<oneshot::Receiver<()>>,
    /// Set true by the read loop on the sidecar's models-ready marker. Unlike [`ready_rx`](Self::
    /// ready_rx) (a one-shot consumed at adoption), this is a poll-anytime flag so a
    /// [`spawn_warming`](Self::spawn_warming) process can be checked *while still pooled* — the
    /// [`SidecarPool`](../../hearsay_core) reads it via [`is_ready`](Self::is_ready) to gate "Start"
    /// on the models actually being loaded.
    ready_flag: Arc<AtomicBool>,
}

impl ProcessTranscriber {
    /// A transcriber backed by the sidecar at `binary` (e.g. `hearsay-live` / `hearsay-me`).
    pub fn new(binary: PathBuf) -> Self {
        ProcessTranscriber {
            binary,
            args: Vec::new(),
            env: Vec::new(),
            respawn_backoff: RESPAWN_BACKOFF.to_vec(),
            child: None,
            stdin: None,
            reader: None,
            broken: false,
            close_timeout: SIDECAR_CLOSE_TIMEOUT,
            warmed_rx: None,
            ready_rx: None,
            ready_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Whether this sidecar has printed its models-ready marker (poll-anytime; safe while pooled).
    /// Used by the warm pool to answer the API's "are the sidecars ready to record?" gate.
    pub fn is_ready(&self) -> bool {
        self.ready_flag.load(Ordering::SeqCst)
    }

    /// Whether the sidecar process is still running (non-blocking). A pooled sidecar that exited —
    /// e.g. a warm whose model load failed — is *dead*: it will never become ready, so the pool
    /// evicts and re-spawns it rather than leaving it to wedge the "Start" gate forever.
    pub fn is_alive(&mut self) -> bool {
        match self.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// Override the per-stage [`close`](Transcriber::close) deadline (drain, then reap). Production
    /// keeps the [`SIDECAR_CLOSE_TIMEOUT`] default; tests set a short bound to exercise the
    /// wedged-sidecar path without a real wall-clock wait.
    pub fn with_close_timeout(mut self, timeout: Duration) -> Self {
        self.close_timeout = timeout;
        self
    }

    /// Pass `args` to the sidecar on every spawn, including respawns.
    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    /// Set an environment variable on the sidecar on every spawn, including respawns.
    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    /// Override the restart delays (and so the retry budget); production uses [`RESPAWN_BACKOFF`].
    pub fn with_respawn_backoff(mut self, backoff: Vec<Duration>) -> Self {
        self.respawn_backoff = backoff;
        self
    }

    /// Spawn the sidecar process, wiring its stderr into tracing and its stdout into a segment
    /// channel. `ready_tx` (if set) fires once the sidecar prints its models-ready marker. Shared by
    /// the cold [`start`](Transcriber::start) path and [`spawn_warming`](Self::spawn_warming).
    fn spawn_process(
        &mut self,
        ready_tx: Option<oneshot::Sender<()>>,
    ) -> Result<mpsc::Receiver<SidecarSegment>, OrchestratorError> {
        let mut child = Command::new(&self.binary)
            .args(&self.args)
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Backstop: if this transcriber is dropped without close() (e.g. a panic mid-pipeline,
            // or a warmed-but-never-used pair dropped at shutdown), the sidecar is reaped rather
            // than left running against a dead core.
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| OrchestratorError::Backend("sidecar stdout not piped".into()))?;
        // Forward the sidecar's stderr into tracing so crash diagnostics are not lost (ipc.md
        // reserves stderr for NDJSON logs). The task ends when the sidecar closes stderr on exit.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(stderr_loop(stderr, self.binary.clone()));
        }
        let (tx, rx) = mpsc::channel(SEGMENT_CHANNEL_CAPACITY);
        let reader = tokio::spawn(read_loop(
            stdout,
            tx,
            self.binary.clone(),
            ready_tx,
            self.ready_flag.clone(),
        ));
        self.child = Some(child);
        self.stdin = stdin;
        self.reader = Some(reader);
        Ok(rx)
    }

    /// Spawn the sidecar now so its FluidAudio/CoreML models load (and the ANE warms) in the
    /// background — off the meeting-start critical path — and stash the segment receiver + the ready
    /// one-shot so [`start`](Transcriber::start) adopts this running process instead of spawning a
    /// second one. Returns as soon as the process is spawned; it does **not** wait for the load, so
    /// a meeting that starts mid-load adopts this same process rather than racing a competing one on
    /// the ANE (which would slow *both*). On a spawn failure the caller leaves the pool empty and the
    /// next meeting cold-starts.
    pub fn spawn_warming(&mut self) -> Result<(), OrchestratorError> {
        let (ready_tx, ready_rx) = oneshot::channel();
        let rx = self.spawn_process(Some(ready_tx))?;
        self.warmed_rx = Some(rx);
        self.ready_rx = Some(ready_rx);
        Ok(())
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

/// Forward the sidecar's stderr into `tracing` (line by line) so a dying sidecar's diagnostics are
/// not lost. Ends when the sidecar closes stderr on exit.
async fn stderr_loop(stderr: tokio::process::ChildStderr, binary: PathBuf) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        tracing::info!(sidecar = %binary.display(), "{line}");
    }
}

/// A non-segment readiness marker a sidecar prints once its models are loaded (`{"ready": true}`),
/// so a multi-minute first-run model download is distinguishable from a hung sidecar.
#[derive(serde::Deserialize)]
struct ReadyMarker {
    ready: bool,
}

/// Read NDJSON segments from the sidecar's stdout, forwarding each parsed line to `tx`. Non-segment
/// lines are skipped, except the readiness marker, which is logged
/// and (for a warming spawn) fires `ready_tx` so [`ProcessTranscriber::warm`] can unblock. The
/// channel closes when stdout hits EOF.
async fn read_loop(
    stdout: tokio::process::ChildStdout,
    tx: mpsc::Sender<SidecarSegment>,
    binary: PathBuf,
    mut ready_tx: Option<oneshot::Sender<()>>,
    ready_flag: Arc<AtomicBool>,
) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        match serde_json::from_str::<SidecarSegment>(&line) {
            Ok(seg) => {
                // Await on a full channel: this parks the reader (and, transitively, the sidecar's
                // stdout write) under a stalled consumer instead of dropping the segment — finals
                // must never be lost. `Err` means the pipeline dropped the receiver.
                if tx.send(seg).await.is_err() {
                    break;
                }
            }
            Err(_) => {
                if serde_json::from_str::<ReadyMarker>(&line).is_ok_and(|m| m.ready) {
                    tracing::info!(sidecar = %binary.display(), "sidecar models ready");
                    // Poll-anytime flag for the warm pool's "Start" gate, plus the one-shot the
                    // pipeline awaits for its warm-up notice.
                    ready_flag.store(true, Ordering::SeqCst);
                    if let Some(ready_tx) = ready_tx.take() {
                        let _ = ready_tx.send(());
                    }
                }
            }
        }
    }
}

#[async_trait]
impl Transcriber for ProcessTranscriber {
    async fn start(&mut self) -> Result<mpsc::Receiver<SidecarSegment>, OrchestratorError> {
        // Pre-warmed: the process is already spawned (from spawn_warming), with its models loaded or
        // still loading in the background. Adopt its stashed segment receiver without respawning; its
        // stdin/child/reader are already set, so feed()/close() work unchanged. `ready_rx` (set at
        // spawn-warming time) is left in place so ready_signal() can still gate the warm-up notice —
        // crucially, only this one process ever loads (no second pair racing it on the ANE).
        if let Some(rx) = self.warmed_rx.take() {
            return Ok(rx);
        }
        // Cold start: no prewarm was available. Spawn now and expose a ready one-shot so the pipeline
        // can show a warm-up notice until this sidecar's ~10 s model load finishes.
        let (ready_tx, ready_rx) = oneshot::channel();
        let rx = self.spawn_process(Some(ready_tx))?;
        self.ready_rx = Some(ready_rx);
        Ok(rx)
    }

    fn ready_signal(&mut self) -> Option<oneshot::Receiver<()>> {
        // A prewarmed sidecar may have finished loading while it sat in the pool, so its ready marker
        // has already arrived. Probe without consuming: if it is already ready, report no warm-up
        // (None) so the meeting shows no notice; if it is still loading, hand the receiver to the
        // pipeline to gate the notice on; if the sidecar died before signaling ready, there is
        // nothing to wait for (None) — the broken pipe surfaces downstream.
        let mut ready_rx = self.ready_rx.take()?;
        match ready_rx.try_recv() {
            Ok(()) => None,
            Err(oneshot::error::TryRecvError::Empty) => Some(ready_rx),
            Err(oneshot::error::TryRecvError::Closed) => None,
        }
    }

    async fn feed(&mut self, samples: Vec<f32>) {
        if self.broken {
            return;
        }
        let Some(stdin) = self.stdin.as_mut() else {
            return;
        };
        if let Err(err) = stdin.write_all(&encode_feed(&samples)).await {
            // The sidecar exited/crashed; stop feeding a dead pipe (the stream loop respawns it).
            // Segments it already emitted are kept.
            self.broken = true;
            tracing::warn!(
                sidecar = %self.binary.display(),
                error = %err,
                "sidecar pipe closed mid-meeting",
            );
        }
    }

    fn can_respawn(&self) -> bool {
        true
    }

    fn is_broken(&self) -> bool {
        self.broken
    }

    fn respawn_backoff(&self) -> &[Duration] {
        &self.respawn_backoff
    }

    async fn respawn(&mut self) -> Result<mpsc::Receiver<SidecarSegment>, OrchestratorError> {
        self.stdin.take();
        if let Some(reader) = self.reader.take() {
            reader.abort();
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
        }
        self.broken = false;
        self.ready_flag.store(false, Ordering::SeqCst);
        self.spawn_process(None)
    }

    async fn close(&mut self) {
        // Drop stdin -> EOF: the sidecar finalizes its tail then exits.
        self.stdin.take();
        // Bound the stdout drain: a wedged sidecar that never closes stdout must not hang stop. On
        // expiry, kill the child to force stdout EOF (which ends the detached reader task) and
        // move on to reaping it below.
        if let Some(reader) = self.reader.take() {
            if tokio::time::timeout(self.close_timeout, reader)
                .await
                .is_err()
            {
                if let Some(child) = self.child.as_mut() {
                    let _ = child.kill().await;
                }
                tracing::warn!(
                    sidecar = %self.binary.display(),
                    "sidecar stdout did not close within the deadline; killed",
                );
            }
        }
        if let Some(mut child) = self.child.take() {
            match tokio::time::timeout(self.close_timeout, child.wait()).await {
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

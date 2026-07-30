//! macOS capture via the Swift `hearsay-helper` — the only process that touches the guarded Core
//! Audio tap (Them) + mic (Me). The core spawns it, listens on the two Unix sockets it connects
//! back to (`control.sock` NDJSON + `media.sock` binary frames, per `shared/protocol/ipc.md`),
//! sends `start_capture`, and pumps the 16 kHz PCM frames it streams into [`CaptureChunk`]s,
//! reusing the `hearsay-ipc` codec. `--synthetic` drives the whole path with generated audio (no
//! TCC prompts) for testing. Also hosts [`probe_permissions`], the side-effect-free TCC read behind
//! the Permissions panel.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command as ProcessCommand};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use hearsay_ipc::{
    decode, expected_payload_len, parse_message, to_line, Command, Event, FrameType, Inbound,
    JsonObj, MediaFrame, SampleFormat, Stream as IpcStream, HEADER_SIZE,
};
use hearsay_orchestrator::{AudioChunk, AudioSource, CaptureChunk, OrchestratorError, Stream};

use crate::PermissionsSnapshot;

/// Contract-fixed capture sample rate (Hz), mono per stream.
const SAMPLE_RATE: u32 = 16_000;
/// Reject a media frame whose declared payload exceeds this — 1 s of f32 (16k * 4 B), a generous
/// ceiling over the contract's 320-640 samples/frame — so a malformed/hostile helper header cannot
/// force a giant pre-read allocation (`shared/protocol/ipc.md`).
const MAX_FRAME_PAYLOAD_BYTES: usize = SAMPLE_RATE as usize * 4;
/// How long to wait for the helper to connect + say hello.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The first `start_capture` blocks on the macOS TCC permission prompts, so allow ample time.
const START_CAPTURE_TIMEOUT: Duration = Duration::from_secs(120);
/// How often [`media_pump`] logs per-stream capture telemetry. Instrumentation for the silent
/// mid-meeting stream death the producer-side watchdogs miss: from the receiving end it separates a
/// tap stuck delivering silence (audio frames keep arriving at `peak` 0) from a stalled producer
/// (audio frames stop while heartbeats continue) from a wedged uplink (both frame kinds stop).
const TELEMETRY_INTERVAL: Duration = Duration::from_secs(5);

fn backend(msg: impl Into<String>) -> OrchestratorError {
    OrchestratorError::Backend(msg.into())
}

/// macOS capture via the Swift `hearsay-helper`.
pub struct SwiftHelperSource {
    helper_path: PathBuf,
    synthetic: bool,
    tap_mode: String,
    running: Option<Running>,
}

struct Running {
    child: Child,
    _run_dir: tempfile::TempDir,
    control_writer: OwnedWriteHalf,
    next_id: i64,
    tasks: Vec<JoinHandle<()>>,
}

impl SwiftHelperSource {
    /// A capture source backed by the `hearsay-helper` binary at `helper_path`.
    pub fn new(helper_path: PathBuf) -> Self {
        SwiftHelperSource {
            helper_path,
            synthetic: false,
            tap_mode: "global_except_self".to_string(),
            running: None,
        }
    }

    /// Run the helper in `--synthetic` mode (generated audio, no capture / TCC prompts).
    pub fn synthetic(mut self, synthetic: bool) -> Self {
        self.synthetic = synthetic;
        self
    }
}

#[async_trait]
impl AudioSource for SwiftHelperSource {
    async fn start(&mut self) -> Result<mpsc::Receiver<CaptureChunk>, OrchestratorError> {
        if !self.helper_path.exists() {
            return Err(backend(format!(
                "helper binary not found at {} (build it with `make swift-build`)",
                self.helper_path.display()
            )));
        }
        let run_dir = tempfile::tempdir()?;
        let control_listener = UnixListener::bind(run_dir.path().join("control.sock"))?;
        let media_listener = UnixListener::bind(run_dir.path().join("media.sock"))?;

        let mut cmd = ProcessCommand::new(&self.helper_path);
        cmd.arg("serve").arg("--socket-dir").arg(run_dir.path());
        if self.synthetic {
            cmd.arg("--synthetic");
        }
        // Reap the helper (mic + tap hot) if any start()-internal step below fails and `child` is
        // dropped before it reaches `Running`, and as a backstop if `Running` is dropped without a
        // clean `stop()`. The probe path already does this; start() must too.
        cmd.kill_on_drop(true);
        let child = cmd.spawn()?;

        // The helper connects back to both sockets (control first, then media).
        let (control_conn, _) = tokio::time::timeout(CONNECT_TIMEOUT, control_listener.accept())
            .await
            .map_err(|_| backend("helper did not connect to control.sock in time"))??;
        let (media_conn, _) = tokio::time::timeout(CONNECT_TIMEOUT, media_listener.accept())
            .await
            .map_err(|_| backend("helper did not connect to media.sock in time"))??;

        let (control_read, mut control_writer) = control_conn.into_split();
        let mut control_lines = BufReader::new(control_read).lines();

        wait_for_event(&mut control_lines, "hello", CONNECT_TIMEOUT).await?;

        let mut args = JsonObj::new();
        args.insert("tap_mode".into(), self.tap_mode.clone().into());
        args.insert("sample_rate".into(), SAMPLE_RATE.into());
        let start = Command {
            id: 1,
            cmd: "start_capture".into(),
            args,
        };
        control_writer
            .write_all(&to_line(&start).map_err(|e| backend(e.to_string()))?)
            .await?;
        wait_for_reply(&mut control_lines, 1, START_CAPTURE_TIMEOUT).await?;

        let (tx, rx) = mpsc::channel::<CaptureChunk>(1024);
        let media_task = tokio::spawn(media_pump(media_conn, tx));
        let control_task = tokio::spawn(drain_control(control_lines));

        self.running = Some(Running {
            child,
            _run_dir: run_dir,
            control_writer,
            next_id: 2,
            tasks: vec![media_task, control_task],
        });
        Ok(rx)
    }

    async fn stop(&mut self) {
        let Some(mut running) = self.running.take() else {
            return;
        };
        // Best-effort graceful shutdown: stop_capture, then shutdown; the helper exits, closing the
        // media socket (which ends the pump + the CaptureChunk channel).
        for cmd_name in ["stop_capture", "shutdown"] {
            let command = Command {
                id: running.next_id,
                cmd: cmd_name.into(),
                args: JsonObj::new(),
            };
            running.next_id += 1;
            if let Ok(line) = to_line(&command) {
                let _ = running.control_writer.write_all(&line).await;
            }
        }
        let _ = running.control_writer.shutdown().await;

        if tokio::time::timeout(Duration::from_secs(5), running.child.wait())
            .await
            .is_err()
        {
            let _ = running.child.kill().await;
        }
        // The helper has now exited (gracefully or killed), so the media socket is closed and the
        // pump reads any socket-buffered tail audio to EOF and ends on its own. Join the tasks
        // (bounded) instead of aborting mid-drain, so that tail audio is not discarded. On the
        // unexpected chance a task does not end, the timeout detaches it rather than hanging stop.
        for task in running.tasks {
            let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        }
    }
}

/// Read `media.sock` frames and forward each audio frame as a [`CaptureChunk`]. Ends (closing the
/// channel) on socket EOF — i.e. when the helper exits. Per `shared/protocol/ipc.md`, a malformed
/// or truncated frame is dropped-and-resynced (never a silent desync of every later frame, never a
/// torn-down capture on one bad header), and each stream's `seq` is tracked so dropped frames are
/// logged.
async fn media_pump(conn: UnixStream, tx: mpsc::Sender<CaptureChunk>) {
    let mut reader = BufReader::new(conn);
    // Last `seq` seen per stream (index = wire stream code). ipc.md: `seq` is per-stream monotonic;
    // a gap means the helper dropped frames.
    let mut last_seq: [Option<u32>; 2] = [None, None];
    let mut telemetry: [StreamTelemetry; 2] = Default::default();
    // Emit is throttled inside the frame loop, not driven by a `select!` timer: `next_frame` wraps a
    // non-cancellation-safe `read_exact`, so racing it against a timer would drop partially-read bytes
    // every tick and desync the stream — manufacturing the very gap this is meant to catch. Heartbeats
    // keep frames arriving per stream even in silence, so the throttle still fires ~every interval; a
    // full uplink wedge stops all frames, and the telemetry line simply ceasing is itself the signal.
    let mut last_emit = Instant::now();
    while let Some(frame) = next_frame(&mut reader).await {
        let now = Instant::now();
        let idx = frame.stream.to_code() as usize;
        if let Some(dropped) = seq_gap(last_seq.get(idx).copied().flatten(), frame.seq) {
            tracing::warn!(
                stream = frame.stream.as_str(),
                dropped,
                seq = frame.seq,
                "media seq gap: helper dropped frames"
            );
        }
        if let Some(slot) = last_seq.get_mut(idx) {
            *slot = Some(frame.seq);
        }

        match frame.frame_type {
            FrameType::Audio => {
                let samples = samples_f32(&frame);
                if let Some(t) = telemetry.get_mut(idx) {
                    t.note_audio(&samples, now);
                }
                let chunk = CaptureChunk {
                    stream: map_stream(frame.stream),
                    chunk: AudioChunk {
                        host_ts: frame.host_ts,
                        samples,
                    },
                };
                if tx.send(chunk).await.is_err() {
                    break; // the orchestrator dropped the receiver
                }
            }
            FrameType::Heartbeat => {
                if let Some(t) = telemetry.get_mut(idx) {
                    t.heartbeats += 1;
                }
            }
            _ => {}
        }

        if now.duration_since(last_emit) >= TELEMETRY_INTERVAL {
            for (idx, t) in telemetry.iter_mut().enumerate() {
                t.emit_and_reset(stream_name(idx), now);
            }
            last_emit = now;
        }
    }
}

/// Per-stream capture liveness [`media_pump`] accumulates between [`TELEMETRY_INTERVAL`] emits.
/// Separates the two silent-death modes each producer-side watchdog misses: an audio frame carrying
/// only zeros (the tap's cadence watchdog can't see it) vs. audio frames ceasing while heartbeats
/// continue (the mic's amplitude watchdog can't see it). `last_audio` / `last_nonzero` persist across
/// resets so a stalled or silent stream shows an ever-growing age.
#[derive(Default)]
struct StreamTelemetry {
    audio_frames: u64,
    heartbeats: u64,
    samples: u64,
    peak: f32,
    last_audio: Option<Instant>,
    last_nonzero: Option<Instant>,
    active: bool,
}

impl StreamTelemetry {
    fn note_audio(&mut self, samples: &[f32], now: Instant) {
        self.active = true;
        self.audio_frames += 1;
        self.samples += samples.len() as u64;
        self.last_audio = Some(now);
        for &s in samples {
            let mag = s.abs();
            if mag > self.peak {
                self.peak = mag;
            }
            if s != 0.0 {
                self.last_nonzero = Some(now);
            }
        }
    }

    /// Log one line if the stream has ever carried audio, then reset the interval counters (keeping
    /// the persistent `last_*` / `active` state). `since_*` is `-1` before the first such event.
    fn emit_and_reset(&mut self, stream: &str, now: Instant) {
        if !self.active {
            return;
        }
        let age = |at: Option<Instant>| at.map_or(-1.0, |i| now.duration_since(i).as_secs_f32());
        tracing::info!(
            target: "hearsay_capture::telemetry",
            stream,
            audio_frames = self.audio_frames,
            heartbeats = self.heartbeats,
            samples = self.samples,
            peak = self.peak,
            since_audio_s = age(self.last_audio),
            since_nonzero_s = age(self.last_nonzero),
            "capture telemetry"
        );
        self.audio_frames = 0;
        self.heartbeats = 0;
        self.samples = 0;
        self.peak = 0.0;
    }
}

/// Wire stream name for a telemetry index (= [`IpcStream::to_code`]).
fn stream_name(idx: usize) -> &'static str {
    IpcStream::from_code(idx as u8)
        .map(|s| s.as_str())
        .unwrap_or("?")
}

/// Dropped-frame count implied by a `seq` gap: `None` when contiguous (or first-seen), else how
/// many frames were skipped. Wraps with `seq` (u32, monotonic from 0), so a wrap boundary is
/// treated as contiguous.
fn seq_gap(prev: Option<u32>, got: u32) -> Option<u32> {
    let expected = prev?.wrapping_add(1);
    (got != expected).then(|| got.wrapping_sub(expected))
}

/// Read the next well-formed media frame, resyncing to the frame magic if the byte stream is
/// misaligned. A garbage/unknown header no longer kills capture and a decode failure no longer
/// silently desyncs every later frame (`ipc.md`: drop/log-and-resync). Returns `None` at EOF.
async fn next_frame<R: tokio::io::AsyncRead + Unpin>(reader: &mut R) -> Option<MediaFrame> {
    let mut header = [0u8; HEADER_SIZE];
    if !fill(reader, &mut header).await {
        return None;
    }
    loop {
        // Slide one byte at a time until the window is a valid, correctly-sized frame header.
        let mut skipped = 0usize;
        let payload_len = loop {
            if let Some(n) = header_payload_len(&header) {
                break n;
            }
            skipped += 1;
            header.copy_within(1.., 0);
            if !fill(reader, &mut header[HEADER_SIZE - 1..]).await {
                return None;
            }
        };
        if skipped > 0 {
            tracing::warn!(skipped, "media stream desynced; resynced to frame magic");
        }
        let mut buf = vec![0u8; HEADER_SIZE + payload_len];
        buf[..HEADER_SIZE].copy_from_slice(&header);
        if payload_len > 0 && !fill(reader, &mut buf[HEADER_SIZE..]).await {
            return None;
        }
        match decode(&buf) {
            Ok(frame) => return Some(frame),
            // `header_payload_len` validated magic/version/type/stream/format and the payload was
            // read to length, so this is unreachable in practice; treat it defensively as a desync
            // and rescan from a fresh header rather than tearing capture down.
            Err(err) => {
                tracing::warn!(error = %err, "media frame decode failed after header validation; resyncing");
                if !fill(reader, &mut header).await {
                    return None;
                }
            }
        }
    }
}

/// Validate a 28-byte window as a frame header and return its payload length, or `None` if it is
/// not a well-formed, correctly-sized header (so the reader slides forward to resync). Checks the
/// magic/version prefix and every enum field before trusting the length, and rejects an oversized
/// payload so a garbage length cannot force a giant read.
fn header_payload_len(header: &[u8; HEADER_SIZE]) -> Option<usize> {
    IpcStream::from_code(header[3]).ok()?; // magic/version/type/format checked by expected_payload_len
    let payload_len = expected_payload_len(header).ok()?;
    (payload_len <= MAX_FRAME_PAYLOAD_BYTES).then_some(payload_len)
}

/// Read exactly `buf.len()` bytes; `false` on EOF or error (end the pump). At a frame boundary EOF
/// is the helper exiting cleanly; mid-frame it is a truncated final frame — both end capture quietly.
/// A genuine socket error (not EOF) ends capture too, but is logged so it isn't mistaken for a clean
/// exit.
async fn fill<R: tokio::io::AsyncRead + Unpin>(reader: &mut R, buf: &mut [u8]) -> bool {
    match reader.read_exact(buf).await {
        Ok(_) => true,
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => false,
        Err(err) => {
            tracing::warn!(error = %err, "media socket read error; ending capture");
            false
        }
    }
}

/// Drain (and debug-log) control events after the handshake so the helper never blocks on a full
/// control socket.
async fn drain_control(mut lines: Lines<BufReader<OwnedReadHalf>>) {
    while let Ok(Some(line)) = lines.next_line().await {
        if let Ok(Inbound::Event(event)) = parse_message(line.as_bytes()) {
            match event.event.as_str() {
                // Capture-health transitions are rare and load-bearing for diagnosing a silent stream
                // death (a stuck tap / dead mic), so surface them + their payload at INFO. The
                // high-rate `level` (and anything else) stay at DEBUG so they don't flood the log.
                "tap_health" | "mic_health" | "status" => {
                    tracing::info!(event = %event.event, data = ?event.data, "helper health event");
                }
                _ => tracing::debug!(event = %event.event, "helper event"),
            }
        }
    }
}

/// Convert a decoded frame's PCM payload to normalized `f32`.
fn samples_f32(frame: &hearsay_ipc::MediaFrame) -> Vec<f32> {
    match frame.format {
        SampleFormat::Int16 => frame
            .payload
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
            .collect(),
        SampleFormat::Float32 => frame
            .payload
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
    }
}

fn map_stream(stream: IpcStream) -> Stream {
    match stream {
        IpcStream::Me => Stream::Me,
        IpcStream::Them => Stream::Them,
    }
}

/// Read control lines until the named event arrives (or timeout).
async fn wait_for_event(
    lines: &mut Lines<BufReader<OwnedReadHalf>>,
    name: &str,
    timeout: Duration,
) -> Result<Event, OrchestratorError> {
    tokio::time::timeout(timeout, async {
        loop {
            let line = lines
                .next_line()
                .await?
                .ok_or_else(|| backend("control channel closed before hello"))?;
            if let Ok(Inbound::Event(event)) = parse_message(line.as_bytes()) {
                if event.event == name {
                    return Ok(event);
                }
            }
        }
    })
    .await
    .map_err(|_| backend(format!("timed out waiting for '{name}' event")))?
}

/// How long the permissions probe waits for the helper to connect, say hello, and answer.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// The TCC states the helper reports; anything else is coerced to "unknown" by the API layer.
const VALID_STATES: [&str; 3] = ["granted", "denied", "undetermined"];

/// Briefly spawn the helper, read its `hello` (build version) + `check_permissions` reply, then
/// shut it down. `check_permissions` reads TCC status side-effect-free (it never starts capture, so
/// no permission prompt fires). Any failure degrades to an unavailable snapshot rather than raising,
/// so the Permissions panel always renders.
pub async fn probe_permissions(helper_path: PathBuf) -> PermissionsSnapshot {
    match probe_inner(&helper_path).await {
        Ok(snapshot) => snapshot,
        Err(err) => {
            tracing::warn!(error = %err, "permissions probe failed");
            PermissionsSnapshot::default()
        }
    }
}

async fn probe_inner(helper_path: &Path) -> Result<PermissionsSnapshot, OrchestratorError> {
    if !helper_path.exists() {
        tracing::info!(path = %helper_path.display(), "permissions probe: helper binary missing");
        return Ok(PermissionsSnapshot::default());
    }
    let run_dir = tempfile::tempdir()?;
    let control_listener = UnixListener::bind(run_dir.path().join("control.sock"))?;
    let media_listener = UnixListener::bind(run_dir.path().join("media.sock"))?;

    let mut cmd = ProcessCommand::new(helper_path);
    cmd.arg("serve").arg("--socket-dir").arg(run_dir.path());
    cmd.kill_on_drop(true);
    // Kept alive to scope end; `kill_on_drop` reaps the helper when this drops.
    let _child = cmd.spawn()?;

    // The helper connects back to both sockets (control first, then media) before it says hello.
    let (control_conn, _) = tokio::time::timeout(PROBE_TIMEOUT, control_listener.accept())
        .await
        .map_err(|_| backend("helper did not connect to control.sock in time"))??;
    let _media = tokio::time::timeout(PROBE_TIMEOUT, media_listener.accept())
        .await
        .map_err(|_| backend("helper did not connect to media.sock in time"))??;

    let (control_read, mut control_writer) = control_conn.into_split();
    let mut control_lines = BufReader::new(control_read).lines();

    let hello = wait_for_event(&mut control_lines, "hello", PROBE_TIMEOUT).await?;
    let helper_version = hello
        .data
        .get("helper_version")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let check = Command {
        id: 1,
        cmd: "check_permissions".into(),
        args: JsonObj::new(),
    };
    control_writer
        .write_all(&to_line(&check).map_err(|e| backend(e.to_string()))?)
        .await?;
    let result = wait_for_reply_result(&mut control_lines, 1, PROBE_TIMEOUT).await?;

    // Best-effort graceful shutdown; `kill_on_drop` reaps the child regardless.
    let shutdown = Command {
        id: 2,
        cmd: "shutdown".into(),
        args: JsonObj::new(),
    };
    if let Ok(line) = to_line(&shutdown) {
        let _ = control_writer.write_all(&line).await;
    }

    let state = |key: &str| -> Option<String> {
        result
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| VALID_STATES.contains(s))
            .map(str::to_string)
    };
    Ok(PermissionsSnapshot {
        available: true,
        helper_version,
        microphone: state("microphone"),
        audio_capture: state("audio_capture"),
        screen_recording: state("screen_recording"),
        accessibility: state("accessibility"),
        calendar: state("calendar"),
    })
}

/// Read control lines until the reply to command `id` arrives (or timeout), returning its `result`
/// object. Errors if the reply is `ok = false`.
async fn wait_for_reply_result(
    lines: &mut Lines<BufReader<OwnedReadHalf>>,
    id: i64,
    timeout: Duration,
) -> Result<JsonObj, OrchestratorError> {
    tokio::time::timeout(timeout, async {
        loop {
            let line = lines
                .next_line()
                .await?
                .ok_or_else(|| backend("control channel closed before reply"))?;
            if let Ok(Inbound::Reply(reply)) = parse_message(line.as_bytes()) {
                if reply.id == id {
                    if reply.ok {
                        return Ok(reply.result.unwrap_or_default());
                    }
                    let msg = reply
                        .error
                        .map(|e| format!("{}: {}", e.code, e.message))
                        .unwrap_or_else(|| "unknown error".to_string());
                    return Err(backend(format!("check_permissions failed: {msg}")));
                }
            }
        }
    })
    .await
    .map_err(|_| backend("timed out waiting for check_permissions reply"))?
}

/// Read control lines until the reply to command `id` arrives (or timeout). Errors if the reply is
/// not `ok`.
async fn wait_for_reply(
    lines: &mut Lines<BufReader<OwnedReadHalf>>,
    id: i64,
    timeout: Duration,
) -> Result<(), OrchestratorError> {
    tokio::time::timeout(timeout, async {
        loop {
            let line = lines
                .next_line()
                .await?
                .ok_or_else(|| backend("control channel closed before reply"))?;
            if let Ok(Inbound::Reply(reply)) = parse_message(line.as_bytes()) {
                if reply.id == id {
                    if reply.ok {
                        return Ok(());
                    }
                    let msg = reply
                        .error
                        .map(|e| format!("{}: {}", e.code, e.message))
                        .unwrap_or_else(|| "unknown error".to_string());
                    return Err(backend(format!("start_capture failed: {msg}")));
                }
            }
        }
    })
    .await
    .map_err(|_| backend("timed out waiting for start_capture reply"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use hearsay_ipc::encode;

    fn audio_frame(stream: IpcStream, seq: u32, samples: &[f32]) -> Vec<u8> {
        let payload: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        encode(&MediaFrame {
            frame_type: FrameType::Audio,
            stream,
            format: SampleFormat::Float32,
            seq,
            host_ts: seq as u64,
            payload,
            flags: 0,
        })
        .unwrap()
    }

    #[test]
    fn seq_gap_detects_drops_and_ignores_contiguous_and_wrap() {
        assert_eq!(seq_gap(None, 5), None); // first-seen
        assert_eq!(seq_gap(Some(4), 5), None); // contiguous
        assert_eq!(seq_gap(Some(4), 7), Some(2)); // two frames dropped
        assert_eq!(seq_gap(Some(u32::MAX), 0), None); // wrap is contiguous
    }

    #[test]
    fn stream_telemetry_separates_real_audio_from_silence_and_resets() {
        let t0 = Instant::now();
        let mut t = StreamTelemetry::default();

        // Real audio: `peak` tracks the loudest magnitude and `last_nonzero` advances.
        t.note_audio(&[0.0, 0.5, -0.9], t0);
        assert!(t.active);
        assert_eq!(t.audio_frames, 1);
        assert_eq!(t.samples, 3);
        assert!((t.peak - 0.9).abs() < 1e-6);
        assert_eq!(t.last_nonzero, Some(t0));

        // An all-zero audio frame (a tap stuck delivering silence) still counts and advances
        // `last_audio`, but never `last_nonzero` — the distinction the tap's watchdog can't draw.
        let t1 = t0 + Duration::from_secs(1);
        t.note_audio(&[0.0, 0.0], t1);
        assert_eq!(t.audio_frames, 2);
        assert_eq!(t.last_audio, Some(t1));
        assert_eq!(t.last_nonzero, Some(t0));

        // Reset clears the interval counters but keeps the persistent liveness stamps, so a stalled
        // stream reports an ever-growing age across intervals.
        t.heartbeats += 1;
        t.emit_and_reset("them", t1 + Duration::from_secs(5));
        assert_eq!((t.audio_frames, t.heartbeats, t.samples), (0, 0, 0));
        assert_eq!(t.peak, 0.0);
        assert!(t.active);
        assert_eq!(t.last_audio, Some(t1));
        assert_eq!(t.last_nonzero, Some(t0));
    }

    #[test]
    fn header_payload_len_accepts_valid_rejects_garbage() {
        let audio = audio_frame(IpcStream::Them, 0, &[0.0, 1.0]);
        let header: &[u8; HEADER_SIZE] = audio[..HEADER_SIZE].try_into().unwrap();
        assert_eq!(header_payload_len(header), Some(8)); // 2 * f32

        let mut bad_magic = *header;
        bad_magic[0] = 0x00;
        assert_eq!(header_payload_len(&bad_magic), None);
        let mut bad_stream = *header;
        bad_stream[3] = 9; // unknown stream code
        assert_eq!(header_payload_len(&bad_stream), None);
    }

    #[tokio::test]
    async fn next_frame_decodes_a_stream_of_frames() {
        let mut bytes = Vec::new();
        bytes.extend(audio_frame(IpcStream::Me, 0, &[0.1]));
        bytes.extend(audio_frame(IpcStream::Them, 0, &[0.2, 0.3]));
        let mut reader = tokio::io::BufReader::new(&bytes[..]);

        let f0 = next_frame(&mut reader).await.unwrap();
        assert_eq!(f0.stream, IpcStream::Me);
        assert_eq!(f0.seq, 0);
        let f1 = next_frame(&mut reader).await.unwrap();
        assert_eq!(f1.stream, IpcStream::Them);
        assert_eq!(samples_f32(&f1), vec![0.2, 0.3]);
        assert!(next_frame(&mut reader).await.is_none()); // clean EOF
    }

    #[tokio::test]
    async fn next_frame_resyncs_past_leading_garbage_and_false_magic() {
        // Junk, including a lone 0xA7 not followed by the version byte, then a real frame.
        let mut bytes = vec![0x00, 0xFF, 0xA7, 0x13, 0x02];
        bytes.extend(audio_frame(IpcStream::Them, 3, &[0.5]));
        let mut reader = tokio::io::BufReader::new(&bytes[..]);

        let frame = next_frame(&mut reader)
            .await
            .expect("resyncs to the real frame magic");
        assert_eq!(frame.stream, IpcStream::Them);
        assert_eq!(frame.seq, 3);
        assert_eq!(samples_f32(&frame), vec![0.5]);
    }
}

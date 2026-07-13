//! Cross-platform audio capture behind the orchestrator's [`AudioSource`] trait.
//!
//! **macOS ([`SwiftHelperSource`]):** reuse the proven Swift `hearsay-helper` — the only process that
//! touches the guarded Core Audio tap (Them) + mic (Me). The core spawns it, listens on the two Unix
//! sockets it connects back to (`control.sock` NDJSON + `media.sock` binary frames, per
//! `shared/protocol/ipc.md`), sends `start_capture`, and pumps the 16 kHz PCM frames it streams into
//! [`CaptureChunk`]s. Port of `src/hearsay/helper/supervisor.py` + `media_channel.py` +
//! `control_channel.py`, reusing the `hearsay-ipc` codec. `--synthetic` drives the whole path with
//! generated audio (no TCC prompts) for testing.
//!
//! **Windows (later):** WASAPI loopback (Them) + mic (Me) via cpal, behind the same trait.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command as ProcessCommand};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use hearsay_ipc::{
    decode, expected_payload_len, parse_message, to_line, Command, Event, Inbound, JsonObj,
    SampleFormat, Stream as IpcStream, HEADER_SIZE,
};
use hearsay_orchestrator::{AudioChunk, AudioSource, CaptureChunk, OrchestratorError, Stream};

/// Contract-fixed capture sample rate (Hz), mono per stream.
const SAMPLE_RATE: u32 = 16_000;
/// How long to wait for the helper to connect + say hello.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The first `start_capture` blocks on the macOS TCC permission prompts, so allow ample time.
const START_CAPTURE_TIMEOUT: Duration = Duration::from_secs(120);

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
        for task in running.tasks {
            task.abort();
        }
    }
}

/// Read `media.sock` frames and forward each audio frame as a [`CaptureChunk`]. Ends (closing the
/// channel) on socket EOF — i.e. when the helper exits.
async fn media_pump(conn: UnixStream, tx: mpsc::Sender<CaptureChunk>) {
    let mut reader = BufReader::new(conn);
    let mut header = [0u8; HEADER_SIZE];
    loop {
        if reader.read_exact(&mut header).await.is_err() {
            break; // EOF
        }
        let payload_len = match expected_payload_len(&header) {
            Ok(n) => n,
            Err(_) => break,
        };
        let mut buf = vec![0u8; HEADER_SIZE + payload_len];
        buf[..HEADER_SIZE].copy_from_slice(&header);
        if payload_len > 0 && reader.read_exact(&mut buf[HEADER_SIZE..]).await.is_err() {
            break;
        }
        let frame = match decode(&buf) {
            Ok(frame) => frame,
            Err(_) => continue,
        };
        if frame.frame_type == hearsay_ipc::FrameType::Audio {
            let chunk = CaptureChunk {
                stream: map_stream(frame.stream),
                chunk: AudioChunk {
                    host_ts: frame.host_ts,
                    samples: samples_f32(&frame),
                },
            };
            if tx.send(chunk).await.is_err() {
                break; // the orchestrator dropped the receiver
            }
        }
    }
}

/// Drain (and debug-log) control events after the handshake so the helper never blocks on a full
/// control socket.
async fn drain_control(mut lines: Lines<BufReader<OwnedReadHalf>>) {
    while let Ok(Some(line)) = lines.next_line().await {
        if let Ok(Inbound::Event(event)) = parse_message(line.as_bytes()) {
            tracing::debug!(event = %event.event, "helper event");
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

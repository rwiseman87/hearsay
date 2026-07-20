//! Windows capture via WASAPI, in-process (no helper): Me = the default capture endpoint (mic),
//! Them = system audio through one of two loopback paths selected by [`LoopbackMode`] (classic
//! device loopback by default; process-loopback-exclude-self as the alternate — see
//! `docs/windows-port.md` for why). One blocking capture thread per stream, each COM-initialized,
//! both stamping chunks from the shared QPC clock (`IAudioCaptureClient` buffer timestamps,
//! 100 ns units -> ns `host_ts`), so Me and Them align by timestamp exactly as on macOS.
//!
//! Resilience mirrors the macOS tap-rebuild watchdog: a read/wait error rebuilds the client, and
//! the default device is re-checked periodically (WASAPI streams do not follow a default-device
//! change) so switching headsets mid-meeting reroutes capture. A thread that cannot rebuild gives
//! up and exits; both threads exiting closes the chunk channel, which is the orchestrator's
//! capture-death signal.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use hearsay_orchestrator::{AudioChunk, AudioSource, CaptureChunk, OrchestratorError, Stream};
use wasapi::{
    deinitialize, initialize_mta, AudioCaptureClient, AudioClient, DeviceEnumerator, Direction,
    Handle, SampleType, StreamMode, WaveFormat,
};

use crate::LoopbackMode;

/// Contract-fixed capture sample rate (Hz), mono per stream.
const SAMPLE_RATE: usize = 16_000;
/// Requested shared-mode engine buffer (100 ns units): 100 ms of headroom over the ~10 ms engine
/// period, so a briefly stalled reader drops nothing.
const BUFFER_DURATION_HNS: i64 = 1_000_000;
/// Event-wait timeout. A loopback stream delivers nothing while no audio plays, so the wait times
/// out routinely; the timeout keeps the loop responsive to stop and to default-device polling.
const EVENT_TIMEOUT_MS: u32 = 250;
/// Read buffer: 1 s of mono f32 — far above the ~10 ms shared-mode packet size, so a burst after a
/// stall still fits.
const READ_BUF_BYTES: usize = SAMPLE_RATE * 4;
/// How often the default device is re-checked (a WASAPI stream does not follow a default change).
const DEVICE_CHECK_INTERVAL: Duration = Duration::from_secs(1);
/// Consecutive failed rebuilds before a stream thread gives up (ending capture — the orchestrator
/// then finalizes the meeting through the normal capture-death path).
const MAX_REBUILD_FAILURES: u32 = 30;
/// Pause between failed rebuild attempts.
const REBUILD_DELAY: Duration = Duration::from_secs(1);
/// How long `start()` waits for both capture threads to report their first client built.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// Windows capture: mic + system-audio loopback behind the [`AudioSource`] trait.
pub struct WasapiSource {
    loopback_mode: LoopbackMode,
    running: Option<Running>,
}

struct Running {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl WasapiSource {
    /// A capture source using `loopback_mode` for the Them stream.
    pub fn new(loopback_mode: LoopbackMode) -> Self {
        WasapiSource {
            loopback_mode,
            running: None,
        }
    }
}

#[async_trait]
impl AudioSource for WasapiSource {
    async fn start(&mut self) -> Result<mpsc::Receiver<CaptureChunk>, OrchestratorError> {
        let (tx, rx) = mpsc::channel::<CaptureChunk>(1024);
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        let mut readies = Vec::new();

        for (stream, loopback) in [(Stream::Me, None), (Stream::Them, Some(self.loopback_mode))] {
            let (ready_tx, ready_rx) = oneshot::channel::<Result<(), String>>();
            let thread_stop = stop.clone();
            let thread_tx = tx.clone();
            let name = match stream {
                Stream::Me => "wasapi-me",
                Stream::Them => "wasapi-them",
            };
            let handle = std::thread::Builder::new()
                .name(name.to_string())
                .spawn(move || capture_thread(stream, loopback, thread_stop, ready_tx, thread_tx))
                .map_err(|e| OrchestratorError::Backend(format!("spawn {name} thread: {e}")))?;
            threads.push(handle);
            readies.push((name, ready_rx));
        }
        // `tx` clones live only in the threads: when both exit, the channel closes (capture death).
        drop(tx);

        for (name, ready) in readies {
            let outcome = tokio::time::timeout(START_TIMEOUT, ready).await;
            let error = match outcome {
                Ok(Ok(Ok(()))) => continue,
                Ok(Ok(Err(e))) => format!("{name}: {e}"),
                Ok(Err(_)) => format!("{name}: capture thread exited before reporting ready"),
                Err(_) => format!("{name}: timed out waiting for capture to start"),
            };
            // Unwind the partial start: stop whatever spawned, then join off the async runtime.
            stop.store(true, Ordering::Relaxed);
            let _ = tokio::task::spawn_blocking(move || {
                for handle in threads {
                    let _ = handle.join();
                }
            })
            .await;
            return Err(OrchestratorError::Backend(format!(
                "wasapi capture failed to start: {error}"
            )));
        }

        self.running = Some(Running { stop, threads });
        Ok(rx)
    }

    async fn stop(&mut self) {
        let Some(running) = self.running.take() else {
            return;
        };
        running.stop.store(true, Ordering::Relaxed);
        // Joining parks the caller for up to one event timeout; do it off the async runtime.
        let _ = tokio::task::spawn_blocking(move || {
            for handle in running.threads {
                let _ = handle.join();
            }
        })
        .await;
    }
}

/// One initialized capture stream: the client (kept alive; owns the stream), its capture
/// interface, the event handle, and the default-device id it was built against (`None` for
/// process loopback, which is not tied to an endpoint).
struct Session {
    client: AudioClient,
    capture: AudioCaptureClient,
    event: Handle,
    device_id: Option<String>,
}

/// The per-stream capture thread: COM init, build the first client (reporting the result through
/// `ready`), then pump packets until stop / channel close / unrecoverable rebuild failure.
fn capture_thread(
    stream: Stream,
    loopback: Option<LoopbackMode>,
    stop: Arc<AtomicBool>,
    ready: oneshot::Sender<Result<(), String>>,
    tx: mpsc::Sender<CaptureChunk>,
) {
    if let Err(err) = initialize_mta().ok() {
        let _ = ready.send(Err(format!("COM init failed: {err}")));
        return;
    }
    match build_session(loopback) {
        Ok(session) => {
            let _ = ready.send(Ok(()));
            run_capture(stream, loopback, session, &stop, &tx);
        }
        Err(err) => {
            let _ = ready.send(Err(err));
        }
    }
    deinitialize();
}

/// Build, initialize, and start one capture client for the stream kind. 16 kHz mono f32 is
/// requested directly: the mic and device-loopback paths enable the engine's format converter
/// (`AUTOCONVERTPCM` + `SRC_DEFAULT_QUALITY`); process loopback specifies the format outright (its
/// client cannot report a mix format) and the engine mixes into it.
fn build_session(loopback: Option<LoopbackMode>) -> Result<Session, String> {
    let format = WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE, 1, None);
    let (mut client, device_id, autoconvert) = match loopback {
        // Them via process loopback: everything except our own process tree (`include_tree =
        // false` maps to EXCLUDE_TARGET_PROCESS_TREE), the global-except-self analog.
        Some(LoopbackMode::Process) => {
            let client = AudioClient::new_application_loopback_client(std::process::id(), false)
                .map_err(|e| format!("process-loopback activation: {e}"))?;
            (client, None, false)
        }
        // Them via classic loopback: capture the default render endpoint's mix.
        Some(LoopbackMode::Device) => {
            let enumerator =
                DeviceEnumerator::new().map_err(|e| format!("device enumerator: {e}"))?;
            let device = enumerator
                .get_default_device(&Direction::Render)
                .map_err(|e| format!("default render device: {e}"))?;
            let id = device.get_id().ok();
            let client = device
                .get_iaudioclient()
                .map_err(|e| format!("render audio client: {e}"))?;
            (client, id, true)
        }
        // Me: the default capture endpoint (mic).
        None => {
            let enumerator =
                DeviceEnumerator::new().map_err(|e| format!("device enumerator: {e}"))?;
            let device = enumerator
                .get_default_device(&Direction::Capture)
                .map_err(|e| format!("default capture device: {e}"))?;
            let id = device.get_id().ok();
            let client = device
                .get_iaudioclient()
                .map_err(|e| format!("capture audio client: {e}"))?;
            (client, id, true)
        }
    };
    let mode = StreamMode::EventsShared {
        autoconvert,
        buffer_duration_hns: BUFFER_DURATION_HNS,
    };
    client
        .initialize_client(&format, &Direction::Capture, &mode)
        .map_err(|e| format!("initialize capture client (16 kHz mono f32): {e}"))?;
    let event = client
        .set_get_eventhandle()
        .map_err(|e| format!("event handle: {e}"))?;
    let capture = client
        .get_audiocaptureclient()
        .map_err(|e| format!("capture interface: {e}"))?;
    client
        .start_stream()
        .map_err(|e| format!("start stream: {e}"))?;
    Ok(Session {
        client,
        capture,
        event,
        device_id,
    })
}

/// Pump packets until stop, channel close, or an unrecoverable rebuild failure. Every drained
/// packet becomes one [`CaptureChunk`] stamped with the packet's QPC timestamp in nanoseconds.
fn run_capture(
    stream: Stream,
    loopback: Option<LoopbackMode>,
    mut session: Session,
    stop: &AtomicBool,
    tx: &mpsc::Sender<CaptureChunk>,
) {
    let mut buf = vec![0u8; READ_BUF_BYTES];
    let mut last_device_check = Instant::now();
    let mut rebuild_failures = 0u32;

    while !stop.load(Ordering::Relaxed) {
        // A timeout is routine (idle loopback delivers nothing); use the tick to notice a
        // default-device change, which a live WASAPI stream does not follow on its own.
        let fired = session.event.wait_for_event(EVENT_TIMEOUT_MS).is_ok();

        if last_device_check.elapsed() >= DEVICE_CHECK_INTERVAL {
            last_device_check = Instant::now();
            if default_device_changed(loopback, session.device_id.as_deref()) {
                tracing::info!(stream = ?stream, "default device changed; rebuilding capture");
                match rebuild(loopback, session, stop, &mut rebuild_failures) {
                    Some(next) => session = next,
                    None => return,
                }
                continue;
            }
        }
        if !fired {
            continue;
        }

        // Drain every pending packet before waiting again.
        loop {
            let packet = session.capture.get_next_packet_size();
            match packet {
                Ok(Some(0)) | Ok(None) => break,
                Ok(Some(_)) => {}
                Err(err) => {
                    tracing::warn!(stream = ?stream, error = %err, "capture read failed; rebuilding");
                    match rebuild(loopback, session, stop, &mut rebuild_failures) {
                        Some(next) => session = next,
                        None => return,
                    }
                    break;
                }
            }
            let (frames, info) = match session.capture.read_from_device(&mut buf) {
                Ok(read) => read,
                Err(err) => {
                    tracing::warn!(stream = ?stream, error = %err, "capture read failed; rebuilding");
                    match rebuild(loopback, session, stop, &mut rebuild_failures) {
                        Some(next) => session = next,
                        None => return,
                    }
                    break;
                }
            };
            if frames == 0 {
                break;
            }
            rebuild_failures = 0;
            if info.flags.timestamp_error {
                tracing::debug!(stream = ?stream, "capture packet flagged timestamp_error");
            }
            let samples = if info.flags.silent {
                // SILENT means "treat as silence"; the buffer contents are unspecified.
                vec![0.0f32; frames as usize]
            } else {
                buf[..frames as usize * 4]
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect()
            };
            let chunk = CaptureChunk {
                stream,
                chunk: AudioChunk {
                    // QPC position of the packet's first frame, 100 ns units -> ns. Both streams
                    // share the QPC clock, so this is the cross-stream alignment timestamp.
                    host_ts: info.timestamp.saturating_mul(100),
                    samples,
                },
            };
            if tx.blocking_send(chunk).is_err() {
                return; // the orchestrator dropped the receiver
            }
        }
    }
}

/// Whether the default endpoint for this stream kind is no longer the one the session was built
/// on. Process loopback is not tied to an endpoint, so it never reports a change.
fn default_device_changed(loopback: Option<LoopbackMode>, built_id: Option<&str>) -> bool {
    let direction = match loopback {
        Some(LoopbackMode::Process) => return false,
        Some(LoopbackMode::Device) => Direction::Render,
        None => Direction::Capture,
    };
    let Some(built_id) = built_id else {
        return false; // the built device's id was unreadable; nothing to compare against
    };
    let current = DeviceEnumerator::new()
        .and_then(|e| e.get_default_device(&direction))
        .and_then(|d| d.get_id());
    match current {
        Ok(id) => id != built_id,
        Err(_) => false, // transient enumeration failure; the read path will surface real trouble
    }
}

/// Tear down `session` and build a replacement, retrying with a pause. `None` (give up, ending
/// this stream's capture) after [`MAX_REBUILD_FAILURES`] consecutive failures or when stop is
/// requested mid-rebuild.
fn rebuild(
    loopback: Option<LoopbackMode>,
    session: Session,
    stop: &AtomicBool,
    failures: &mut u32,
) -> Option<Session> {
    let _ = session.client.stop_stream();
    drop(session);
    while !stop.load(Ordering::Relaxed) {
        match build_session(loopback) {
            Ok(next) => {
                *failures = 0;
                return Some(next);
            }
            Err(err) => {
                *failures += 1;
                if *failures >= MAX_REBUILD_FAILURES {
                    tracing::error!(error = %err, "capture rebuild failed {MAX_REBUILD_FAILURES} times; ending capture");
                    return None;
                }
                tracing::warn!(error = %err, attempt = *failures, "capture rebuild failed; retrying");
                std::thread::sleep(REBUILD_DELAY);
            }
        }
    }
    None
}

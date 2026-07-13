//! The live transcription pipeline: route each stream's PCM to its transcriber and persist +
//! broadcast the segments it emits. Port of `hearsay.transcript.pipeline` + the two sidecar
//! processors (`live.py` / `live_me.py`).
//!
//! Task layout:
//! - `demux` — reads the capture channel, anchors the shared epoch clock, and forwards each chunk
//!   to its stream's task as meeting-relative `(t0_s, samples)`.
//! - one task per stream — feeds its transcriber and handles the segments it emits: partials
//!   broadcast to the UI only; finals also persist to the database (Them binds a `Speaker N`
//!   cluster). Segment times are shifted by the stream's offset (its first fed `t0_s`).

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use uuid::Uuid;

use hearsay_db::queries;

use crate::error::OrchestratorError;
use crate::recorder::MeetingAudioRecorder;
use crate::traits::{AudioSource, BackendInstance, StreamRole, Transcriber};
use crate::types::{CaptureChunk, SegmentKind, SidecarSegment, Stream};

/// Capacity of the per-meeting live broadcast channel (transcript events to WebSocket subscribers).
const BROADCAST_CAPACITY: usize = 256;

/// Capacity of each stream's PCM hand-off channel (demux -> stream task). Bounded so a slow
/// transcriber backpressures capture instead of the queue growing without bound; ~13 s of 100 ms
/// chunks.
const PCM_CHANNEL_CAPACITY: usize = 128;

/// A running pipeline: the capture source (kept to stop it) and the spawned tasks.
pub(crate) struct Pipeline {
    pub(crate) broadcast_tx: broadcast::Sender<String>,
    pub(crate) source: Box<dyn AudioSource>,
    pub(crate) tasks: Vec<JoinHandle<()>>,
}

impl Pipeline {
    /// Stop capture, then wait for every task to wind down (each transcriber's tail is drained on
    /// close). After this returns, the broadcast channel closes when the pipeline is dropped.
    pub(crate) async fn close(mut self) {
        self.source.stop().await;
        for task in self.tasks.drain(..) {
            let _ = task.await;
        }
    }
}

/// Start the source + both transcribers and spawn the routing/handling tasks. `audio_path` is the
/// `audio.wav` to record (Me=L / Them=R) when recording is enabled, else `None`.
pub(crate) async fn spawn(
    instance: BackendInstance,
    pool: SqlitePool,
    meeting_id: Uuid,
    audio_path: Option<PathBuf>,
) -> Result<Pipeline, OrchestratorError> {
    let BackendInstance {
        mut source,
        mut me,
        mut them,
    } = instance;

    let capture_rx = source.start().await?;
    let me_emit = me.start().await?;
    let them_emit = them.start().await?;

    let (broadcast_tx, _) = broadcast::channel::<String>(BROADCAST_CAPACITY);
    let (me_tx, me_rx) = mpsc::channel::<(f64, Vec<f32>)>(PCM_CHANNEL_CAPACITY);
    let (them_tx, them_rx) = mpsc::channel::<(f64, Vec<f32>)>(PCM_CHANNEL_CAPACITY);

    let recorder = audio_path.map(MeetingAudioRecorder::new);
    let demux = tokio::spawn(demux(capture_rx, me_tx, them_tx, recorder));
    let me_task = tokio::spawn(stream_loop(
        StreamRole::Me,
        me,
        me_rx,
        me_emit,
        pool.clone(),
        meeting_id,
        broadcast_tx.clone(),
    ));
    let them_task = tokio::spawn(stream_loop(
        StreamRole::Them,
        them,
        them_rx,
        them_emit,
        pool,
        meeting_id,
        broadcast_tx.clone(),
    ));

    Ok(Pipeline {
        broadcast_tx,
        source,
        tasks: vec![demux, me_task, them_task],
    })
}

/// Read capture, anchor the shared epoch on the first chunk, record the stereo `audio.wav` (if
/// enabled), and forward each chunk to its stream's task as meeting-relative `(t0_s, samples)`. Both
/// streams anchor to the same epoch so their timelines align (alignment is by timestamp, never
/// sample index). The recorder is finalized once capture ends.
async fn demux(
    mut capture_rx: mpsc::Receiver<CaptureChunk>,
    me_tx: mpsc::Sender<(f64, Vec<f32>)>,
    them_tx: mpsc::Sender<(f64, Vec<f32>)>,
    mut recorder: Option<MeetingAudioRecorder>,
) {
    let mut epoch_ns: Option<u64> = None;
    while let Some(cap) = capture_rx.recv().await {
        let epoch = *epoch_ns.get_or_insert(cap.chunk.host_ts);
        let t0_s = cap.chunk.host_ts.saturating_sub(epoch) as f64 / 1e9;
        if let Some(rec) = recorder.as_mut() {
            rec.write(&cap.chunk.samples, t0_s, cap.stream);
        }
        let sender = match cap.stream {
            Stream::Me => &me_tx,
            Stream::Them => &them_tx,
        };
        let _ = sender.send((t0_s, cap.chunk.samples)).await;
    }
    // Capture ended: write the WAV. Best-effort — a failure never fails the meeting stop. The encode
    // walks every sample of the meeting, so run it off the async worker.
    if let Some(rec) = recorder.take() {
        match tokio::task::spawn_blocking(move || rec.close()).await {
            Ok(Err(err)) => tracing::error!(error = %err, "failed to write meeting audio.wav"),
            Err(err) => tracing::error!(error = %err, "meeting audio.wav writer panicked"),
            Ok(Ok(())) => {}
        }
    }
}

/// Per-stream task: feed the transcriber while capture flows, then flush + drain its tail. Segment
/// times are shifted by `offset` (the first `t0_s` fed to this stream), mapping sidecar-local time
/// back to meeting time.
async fn stream_loop(
    role: StreamRole,
    mut transcriber: Box<dyn Transcriber>,
    mut chunk_rx: mpsc::Receiver<(f64, Vec<f32>)>,
    mut emit_rx: mpsc::UnboundedReceiver<SidecarSegment>,
    pool: SqlitePool,
    meeting_id: Uuid,
    broadcast_tx: broadcast::Sender<String>,
) {
    let mut offset: Option<f64> = None;
    let mut clusters: HashMap<i64, Uuid> = HashMap::new();
    let mut feeding = true;

    loop {
        tokio::select! {
            biased;
            chunk = chunk_rx.recv(), if feeding => match chunk {
                Some((t0_s, samples)) => {
                    offset.get_or_insert(t0_s);
                    transcriber.feed(samples).await;
                }
                // Capture ended: stop feeding and flush the sidecar's finalized tail. The emit
                // channel closes once the sidecar exits, ending the drain below.
                None => {
                    feeding = false;
                    transcriber.close().await;
                }
            },
            seg = emit_rx.recv() => match seg {
                Some(seg) => {
                    handle(
                        role,
                        &seg,
                        offset.unwrap_or(0.0),
                        &pool,
                        meeting_id,
                        &broadcast_tx,
                        &mut clusters,
                    )
                    .await;
                }
                None => break, // the sidecar closed its output; the stream is done
            },
        }
    }
}

/// A transcript event pushed to WebSocket subscribers. Field order + names match the Python
/// `TranscriptEvent` (`kind`, `stream`, `speaker_label`, `text`, `start_s`, `end_s`).
#[derive(Serialize)]
struct TranscriptEvent<'a> {
    kind: SegmentKind,
    stream: &'a str,
    speaker_label: &'a str,
    text: &'a str,
    start_s: f64,
    end_s: f64,
}

fn publish(broadcast_tx: &broadcast::Sender<String>, event: &TranscriptEvent<'_>) {
    if let Ok(line) = serde_json::to_string(event) {
        // Err just means no live subscribers, which is fine.
        let _ = broadcast_tx.send(line);
    }
}

/// Persist + broadcast one emitted segment, applying the stream's meeting-time `offset`.
#[allow(clippy::too_many_arguments)]
async fn handle(
    role: StreamRole,
    seg: &SidecarSegment,
    offset: f64,
    pool: &SqlitePool,
    meeting_id: Uuid,
    broadcast_tx: &broadcast::Sender<String>,
    clusters: &mut HashMap<i64, Uuid>,
) {
    let start_s = seg.start_s + offset;
    let end_s = seg.end_s + offset;

    match role {
        // Me is always the local speaker: broadcast partials + finals; persist only finals.
        StreamRole::Me => {
            publish(
                broadcast_tx,
                &TranscriptEvent {
                    kind: seg.kind,
                    stream: "me",
                    speaker_label: "Me",
                    text: &seg.text,
                    start_s,
                    end_s,
                },
            );
            if seg.kind == SegmentKind::Final {
                if let Err(err) = queries::insert_segment(
                    pool,
                    meeting_id,
                    role.stream(),
                    "Me",
                    &seg.text,
                    start_s,
                    end_s,
                    None,
                )
                .await
                {
                    tracing::error!(error = %err, "failed to persist Me segment");
                }
            }
        }
        // Them: a partial is pre-diarization, so stream it speaker-less ("Them") and don't persist.
        // A final carries a 0-based speaker ordinal -> a `Speaker N` label + a per-meeting cluster.
        StreamRole::Them => {
            if seg.kind == SegmentKind::Partial {
                publish(
                    broadcast_tx,
                    &TranscriptEvent {
                        kind: SegmentKind::Partial,
                        stream: "them",
                        speaker_label: "Them",
                        text: &seg.text,
                        start_s,
                        end_s,
                    },
                );
                return;
            }
            let ordinal = seg.speaker.unwrap_or(0) + 1;
            let label = format!("Speaker {ordinal}");
            let cluster_id = cluster_for(pool, meeting_id, ordinal, clusters).await;
            if let Err(err) = queries::insert_segment(
                pool,
                meeting_id,
                role.stream(),
                &label,
                &seg.text,
                start_s,
                end_s,
                cluster_id,
            )
            .await
            {
                tracing::error!(error = %err, "failed to persist Them segment");
            }
            publish(
                broadcast_tx,
                &TranscriptEvent {
                    kind: SegmentKind::Final,
                    stream: "them",
                    speaker_label: &label,
                    text: &seg.text,
                    start_s,
                    end_s,
                },
            );
        }
    }
}

/// Get-or-create the per-meeting cluster for a Them speaker ordinal (unlocked, no centroid live —
/// the offline refine re-seeds centroids). Returns `None` if the cluster row could not be created,
/// leaving the segment unbound.
async fn cluster_for(
    pool: &SqlitePool,
    meeting_id: Uuid,
    ordinal: i64,
    clusters: &mut HashMap<i64, Uuid>,
) -> Option<Uuid> {
    if let Some(id) = clusters.get(&ordinal) {
        return Some(*id);
    }
    match queries::create_cluster(pool, meeting_id, ordinal, false, None).await {
        Ok(cluster) => {
            clusters.insert(ordinal, cluster.id);
            Some(cluster.id)
        }
        Err(err) => {
            tracing::error!(error = %err, ordinal, "failed to create speaker cluster");
            None
        }
    }
}

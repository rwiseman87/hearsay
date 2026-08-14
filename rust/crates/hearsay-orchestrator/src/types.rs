//! Value types shared across the orchestrator: audio chunks from capture and the transcript
//! segments a sidecar emits.

use serde::{Deserialize, Serialize};

pub use hearsay_db::models::Stream;

/// A normalized mono audio chunk on capture's single monotonic clock.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioChunk {
    /// Capture timestamp in nanoseconds — the shared `host_ts` clock across both streams.
    pub host_ts: u64,
    /// Mono PCM samples normalized to `[-1.0, 1.0]`.
    pub samples: Vec<f32>,
}

/// A stream-tagged audio chunk produced by an [`AudioSource`](crate::AudioSource). End-of-capture
/// is signaled by the source's channel closing (both streams end together).
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureChunk {
    pub stream: Stream,
    pub chunk: AudioChunk,
}

/// Whether a sidecar segment is an in-progress partial (streamed to the UI only) or a finalized
/// turn (also persisted + appended). Serializes lowercase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SegmentKind {
    /// In-progress speech; streamed to the UI, never persisted.
    Partial,
    /// A completed turn; persisted, appended, and broadcast.
    #[default]
    Final,
}

/// One segment a sidecar emits on stdout (NDJSON). Mirrors the Swift sidecars' `{kind, text,
/// start_s, end_s, speaker}` line: `kind` defaults to `final` when absent, and `speaker` is a
/// 0-based diarizer ordinal present only on Them finals. Times are relative to the sidecar's first
/// received sample; the pipeline shifts them to meeting time.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SidecarSegment {
    #[serde(default)]
    pub kind: SegmentKind,
    pub text: String,
    pub start_s: f64,
    pub end_s: f64,
    #[serde(default)]
    pub speaker: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_kind_defaults_to_final_and_speaker_optional() {
        let seg: SidecarSegment =
            serde_json::from_str(r#"{"text":"hi","start_s":0.0,"end_s":1.0}"#).unwrap();
        assert_eq!(seg.kind, SegmentKind::Final);
        assert_eq!(seg.speaker, None);
    }

    #[test]
    fn segment_parses_partial_and_speaker() {
        let seg: SidecarSegment = serde_json::from_str(
            r#"{"kind":"partial","text":"in progress","start_s":1.5,"end_s":2.0,"speaker":0}"#,
        )
        .unwrap();
        assert_eq!(seg.kind, SegmentKind::Partial);
        assert_eq!(seg.speaker, Some(0));
    }
}

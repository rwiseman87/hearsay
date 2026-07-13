//! Request/response DTOs. Port of `src/hearsay/schemas/`.
//!
//! Serialized to JSON for the loopback API and described via `utoipa::ToSchema` so the OpenAPI
//! spec can drive the TypeScript codegen (matching the Python OpenAPI->TS pipeline).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use hearsay_db::models::{Identity, Meeting, Segment};
use hearsay_db::queries::SpeakerRow;

/// Lifecycle state of a meeting (lowercase on the wire, matching the Python `StrEnum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MeetingStatus {
    Recording,
    Finalized,
}

impl From<hearsay_db::models::MeetingStatus> for MeetingStatus {
    fn from(status: hearsay_db::models::MeetingStatus) -> Self {
        match status {
            hearsay_db::models::MeetingStatus::Recording => MeetingStatus::Recording,
            hearsay_db::models::MeetingStatus::Finalized => MeetingStatus::Finalized,
        }
    }
}

/// Which capture channel a segment came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    Me,
    Them,
}

impl From<hearsay_db::models::Stream> for Stream {
    fn from(stream: hearsay_db::models::Stream) -> Self {
        match stream {
            hearsay_db::models::Stream::Me => Stream::Me,
            hearsay_db::models::Stream::Them => Stream::Them,
        }
    }
}

/// Paginated list envelope used by every list endpoint (`{ total, page, page_size, items }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Page<T> {
    pub total: i64,
    pub page: u32,
    pub page_size: u32,
    pub items: Vec<T>,
}

/// A meeting row for the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MeetingRead {
    pub id: Uuid,
    pub title: String,
    pub folder: String,
    pub status: MeetingStatus,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<Meeting> for MeetingRead {
    fn from(m: Meeting) -> Self {
        MeetingRead {
            id: m.id,
            title: m.title,
            folder: m.folder,
            status: m.status.into(),
            started_at: m.started_at,
            ended_at: m.ended_at,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

/// A transcript segment for the API.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct SegmentRead {
    pub id: Uuid,
    pub stream: Stream,
    pub speaker_label: String,
    pub cluster_id: Option<Uuid>,
    pub text: String,
    pub start_s: f64,
    pub end_s: f64,
}

impl From<Segment> for SegmentRead {
    fn from(s: Segment) -> Self {
        SegmentRead {
            id: s.id,
            stream: s.stream.into(),
            speaker_label: s.speaker_label,
            cluster_id: s.cluster_id,
            text: s.text,
            start_s: s.start_s,
            end_s: s.end_s,
        }
    }
}

/// A diarization cluster within a meeting, with its resolved display label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SpeakerRead {
    /// The cluster id.
    pub id: Uuid,
    pub ordinal: i64,
    /// The bound identity's name, else `"Speaker {ordinal}"`.
    pub label: String,
    pub identity_id: Option<Uuid>,
    pub locked: bool,
}

impl From<SpeakerRow> for SpeakerRead {
    fn from(row: SpeakerRow) -> Self {
        let label = row
            .display_name
            .unwrap_or_else(|| format!("Speaker {}", row.ordinal));
        SpeakerRead {
            id: row.id,
            ordinal: row.ordinal,
            label,
            identity_id: row.identity_id,
            locked: row.locked,
        }
    }
}

/// A known cross-meeting person (offered as a rename suggestion).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct IdentityRead {
    pub id: Uuid,
    pub display_name: String,
    pub email: Option<String>,
}

impl From<Identity> for IdentityRead {
    fn from(i: Identity) -> Self {
        IdentityRead {
            id: i.id,
            display_name: i.display_name,
            email: i.email,
        }
    }
}

/// Start a meeting. `title` defaults to a timestamp-derived name when omitted.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct MeetingCreate {
    #[serde(default)]
    pub title: Option<String>,
}

/// Rename a cluster to a person (binds + locks; relabels that speaker's segments).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SpeakerRename {
    pub display_name: String,
}

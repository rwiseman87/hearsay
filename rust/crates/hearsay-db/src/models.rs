//! ORM row types + enums for the SQLite schema. Port of `src/hearsay/models/`.
//!
//! Every table has a UUID primary key and `created_at` / `updated_at` timestamps. Enums are
//! stored as their lowercase string values (matching the Python `StrEnum` serialization).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

/// Lifecycle state of a meeting: `recording` while live, `refining` while the post-stop offline
/// refine + transcript write run in the background, then `finalized`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
pub enum MeetingStatus {
    Recording,
    Refining,
    Finalized,
}

/// Which capture channel a segment came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
pub enum Stream {
    Me,
    Them,
}

/// A meeting row.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct Meeting {
    pub id: Uuid,
    pub title: String,
    pub folder: String,
    pub status: MeetingStatus,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Absolute path of this meeting's recordings directory, pinned at creation from the then-effective
    /// output_dir so the meeting stays locatable after the Storage setting changes. Legacy rows created
    /// before this column store `""`; [`Meeting::dir_path`] falls back to `default_root.join(folder)`.
    #[sqlx(default)]
    pub dir: String,
}

impl Meeting {
    /// This meeting's recordings directory: the pinned absolute `dir`, or — for legacy rows that
    /// predate it — `default_root/<folder>` (the historical layout). `default_root` is the caller's
    /// effective output_dir.
    pub fn dir_path(&self, default_root: &Path) -> PathBuf {
        if self.dir.is_empty() {
            default_root.join(&self.folder)
        } else {
            PathBuf::from(&self.dir)
        }
    }
}

/// A transcript segment row.
#[derive(Debug, Clone, PartialEq, FromRow)]
pub struct Segment {
    pub id: Uuid,
    pub meeting_id: Uuid,
    pub cluster_id: Option<Uuid>,
    pub stream: Stream,
    pub speaker_label: String,
    pub text: String,
    pub start_s: f64,
    pub end_s: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A person identity row (a named, recognizable speaker).
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct Identity {
    pub id: Uuid,
    pub display_name: String,
    pub email: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A per-meeting speaker cluster row (a diarizer speaker, optionally bound to an identity).
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct Cluster {
    pub id: Uuid,
    pub meeting_id: Uuid,
    pub ordinal: i64,
    pub identity_id: Option<Uuid>,
    pub locked: bool,
    pub centroid: Option<Vec<u8>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A user-settings overlay row: one settings section stored as a JSON object string. The settings
/// service resolves the effective value as this stored override when present, else the config
/// default. Port of `src/hearsay/models/preference.py`.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct Preference {
    pub id: Uuid,
    pub section: String,
    pub value: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A meeting's generated notes row: the summary + action items produced by the optional local LLM
/// summarization step. One row per meeting (keyed by `meeting_id`); `action_items` is a JSON array
/// of strings and `model` records the GGUF that produced it. Cascades on meeting delete.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct MeetingNotes {
    pub meeting_id: Uuid,
    pub summary: String,
    pub action_items: String,
    pub model: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

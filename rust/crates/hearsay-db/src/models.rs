//! ORM row types + enums for the SQLite schema.
//!
//! Every table has a UUID primary key and `created_at` / `updated_at` timestamps. Enums are
//! stored as their lowercase string values.

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
#[derive(Debug, Clone, PartialEq, FromRow)]
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
    /// The user-facing organizational [`Folder`] this meeting is filed under, or `None` when unfiled.
    /// Distinct from `folder` above (the on-disk directory name); set the meeting's `folder_id`, not
    /// `folder`, to move it between folders.
    #[sqlx(default)]
    pub folder_id: Option<Uuid>,
    /// Transcribed share (0.0..=1.0) of the Them track's audible time at the last refine; `None`
    /// when never refined. A low value means whisper stalled and the transcript is truncated.
    #[sqlx(default)]
    pub refine_coverage: Option<f64>,
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

/// A user-facing organizational folder for meetings. Folders nest via `parent_id` (`None` = a root
/// folder); each meeting is filed under at most one folder (`Meeting::folder_id`). Deleting a folder
/// removes its sub-folder subtree and un-files (does not delete) the meetings within.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct Folder {
    pub id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
    /// Set once a user manually edits this segment — its text or its speaker assignment — so the UI
    /// can badge it and warn before a re-diarize would discard the change. Defaults to `0`;
    /// refine-inserted segments are unedited.
    #[sqlx(default)]
    pub edited: bool,
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
/// default.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct Preference {
    pub id: Uuid,
    pub section: String,
    pub value: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A meeting's user-authored notes row: the free-form text typed in the "My notes" panel during the
/// meeting, distinct from [`MeetingNotes`] (the LLM-generated notes). One row per meeting
/// (keyed by `meeting_id`), upserted on autosave. Cascades on meeting delete.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct UserNotes {
    pub meeting_id: Uuid,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A meeting's generated notes row: the local LLM summarization step's reply, stored verbatim as the
/// Markdown `content`. One row per meeting (keyed by `meeting_id`); `model` records the GGUF that
/// produced it. The prompt template dictates the note's format, so there is no structured
/// summary/action-item shape. Cascades on meeting delete.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct MeetingNotes {
    pub meeting_id: Uuid,
    pub content: String,
    pub model: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Set once a user manually edits these notes, so a regenerate can warn before overwriting them.
    /// Cleared (`0`) whenever the LLM (re)generates the notes.
    #[sqlx(default)]
    pub edited: bool,
}

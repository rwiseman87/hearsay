//! Request/response DTOs.
//!
//! Serialized to JSON for the loopback API and described via `utoipa::ToSchema` so the OpenAPI
//! spec can drive the TypeScript codegen.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use hearsay_db::models::{Folder, Identity, Meeting, MeetingNotes, Segment};
use hearsay_db::queries::{SearchHitRow, SpeakerRow};

/// Lifecycle state of a meeting (lowercase on the wire): `recording` while live, `refining` while the
/// post-stop refine + transcript write run in the background, then `finalized`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MeetingStatus {
    Recording,
    Refining,
    Finalized,
}

impl From<hearsay_db::models::MeetingStatus> for MeetingStatus {
    fn from(status: hearsay_db::models::MeetingStatus) -> Self {
        match status {
            hearsay_db::models::MeetingStatus::Recording => MeetingStatus::Recording,
            hearsay_db::models::MeetingStatus::Refining => MeetingStatus::Refining,
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
    /// The on-disk recordings-directory name (not an organizational folder). To move a meeting
    /// between user folders, use `folder_id`.
    pub folder: String,
    /// The organizational [`FolderRead`] this meeting is filed under, or `null` when unfiled.
    pub folder_id: Option<Uuid>,
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
            folder_id: m.folder_id,
            status: m.status.into(),
            started_at: m.started_at,
            ended_at: m.ended_at,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

/// An organizational folder for meetings (a node in the nested folder tree). `parent_id` is `null`
/// for a root folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct FolderRead {
    pub id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<Folder> for FolderRead {
    fn from(f: Folder) -> Self {
        FolderRead {
            id: f.id,
            name: f.name,
            parent_id: f.parent_id,
            created_at: f.created_at,
            updated_at: f.updated_at,
        }
    }
}

/// A meeting's generated notes for the API: the summary + action items, and which model produced
/// them. `action_items` is decoded from the stored JSON array (a corrupt row degrades to empty
/// rather than failing the read, matching the settings-section tolerance).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MeetingNotesRead {
    pub summary: String,
    pub action_items: Vec<String>,
    pub model: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Whether these notes have been manually edited (drives the "edited" badge + the regenerate
    /// overwrite warning).
    pub edited: bool,
    /// Whether the transcript changed after these notes were generated/edited — i.e. the notes may be
    /// out of date. Computed at read time (a mutating response sets it `false`).
    pub stale: bool,
}

impl From<MeetingNotes> for MeetingNotesRead {
    fn from(n: MeetingNotes) -> Self {
        MeetingNotesRead {
            summary: n.summary,
            action_items: serde_json::from_str(&n.action_items).unwrap_or_default(),
            model: n.model,
            created_at: n.created_at,
            updated_at: n.updated_at,
            edited: n.edited,
            stale: false,
        }
    }
}

/// A manual notes edit: replace the `summary` and `action_items`. Validated at the boundary
/// (length-bounded summary, capped item count/length).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
pub struct NotesEdit {
    pub summary: String,
    pub action_items: Vec<String>,
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
    /// Whether this segment's text has been manually edited (drives the "edited" badge + the
    /// discard-on-refine warning).
    pub edited: bool,
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
            edited: s.edited,
        }
    }
}

/// A manual transcript edit: replace a segment's `text`. Validated at the boundary (non-empty,
/// length-bounded).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
pub struct SegmentEdit {
    pub text: String,
}

/// One transcript-search hit for the API: the matched segment with enough meeting context to render
/// and navigate to it. `snippet` is the matched text with each match wrapped in the private-use
/// sentinels U+E000/U+E001, which the client swaps for highlight markup.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct SearchHit {
    pub meeting_id: Uuid,
    pub meeting_title: String,
    pub meeting_status: MeetingStatus,
    pub started_at: DateTime<Utc>,
    pub segment_id: Uuid,
    pub stream: Stream,
    pub speaker_label: String,
    pub start_s: f64,
    pub snippet: String,
}

impl From<SearchHitRow> for SearchHit {
    fn from(row: SearchHitRow) -> Self {
        SearchHit {
            meeting_id: row.meeting_id,
            meeting_title: row.meeting_title,
            meeting_status: row.meeting_status.into(),
            started_at: row.started_at,
            segment_id: row.segment_id,
            stream: row.stream.into(),
            speaker_label: row.speaker_label,
            start_s: row.start_s,
            snippet: row.snippet,
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

/// Rename a meeting. `title` replaces the meeting's display title (validated non-empty, <= 255 chars).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct MeetingUpdate {
    pub title: String,
}

/// File a meeting under a folder. `folder_id` is the target folder, or `null` to un-file (move it
/// back to the root/"Unfiled" list). Validated: a non-null `folder_id` must reference an existing
/// folder.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct MeetingFolderAssign {
    #[serde(default)]
    pub folder_id: Option<Uuid>,
}

/// Create a folder. `name` is validated non-empty, <= 255 chars; `parent_id` nests it under an
/// existing folder (omit or `null` for a root folder).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct FolderCreate {
    pub name: String,
    #[serde(default)]
    pub parent_id: Option<Uuid>,
}

/// Rename a folder. `name` replaces the folder's display name (validated non-empty, <= 255 chars).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct FolderUpdate {
    pub name: String,
}

/// Reparent a folder. `parent_id` is the new parent, or `null` to move it to the root. Validated: a
/// non-null `parent_id` must exist and must not be the folder itself or one of its descendants (which
/// would create a cycle).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct FolderReparent {
    #[serde(default)]
    pub parent_id: Option<Uuid>,
}

/// Rename a cluster to a person (binds + locks; relabels that speaker's segments).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SpeakerRename {
    pub display_name: String,
}

/// Recording & privacy — the single audio-retention switch (keep one WAV per meeting for playback
/// + the post-meeting refine). Editable section; a request body and part of [`SettingsRead`].
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RecordingSettings {
    pub record: bool,
}

/// Speaker diarization: re-diarize each meeting at finalize (`auto_refine`) + the cosine at/above
/// which a refined speaker is auto-matched to a person named in a prior meeting.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SpeakerSettings {
    pub auto_refine: bool,
    pub recognition_threshold: f64,
}

/// The default root new meetings are written under (existing meetings keep their stamped location).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StorageSettings {
    pub output_dir: String,
}

/// Read-only storage facts shown alongside the editable storage section.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StorageInfo {
    pub output_dir: String,
    pub database_path: String,
    pub tracked_bytes: i64,
    pub meeting_count: i64,
}

/// Models: the offline-refine whisper model path plus the optional local-LLM notes step (enable +
/// its GGUF model). Editable section; each effective value is the stored override, else the config
/// default. Live transcription is the FluidAudio/ANE sidecars and is not configured here.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelSettings {
    pub refine_model: String,
    /// Generate a summary + action items at meeting stop (the optional local-LLM notes step).
    /// `#[serde(default)]` so a `models` row written before notes existed still deserializes.
    #[serde(default)]
    pub notes_enabled: bool,
    /// GGUF model path for the notes step; empty until one is downloaded or chosen.
    #[serde(default)]
    pub notes_model: String,
    /// User-editable prompt template for the notes step (its `{transcript}` placeholder is filled
    /// with the finalized transcript). Empty means "use the built-in default"
    /// (`ModelsInfo::default_notes_prompt`). `#[serde(default)]` so a `models` row written before the
    /// prompt existed still deserializes.
    #[serde(default)]
    pub notes_prompt: String,
}

/// Read-only model facts shown alongside the editable models section: the bundled/config defaults
/// (reset targets) and whether each effective model file currently resolves on disk.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelsInfo {
    pub default_refine_model: String,
    pub refine_model_exists: bool,
    pub default_notes_model: String,
    pub notes_model_exists: bool,
    /// The built-in default notes prompt template (the reset target + the effective value when the
    /// editable `notes_prompt` is empty).
    pub default_notes_prompt: String,
}

/// One downloadable notes model in the in-app catalog (the internal repo/file/sha are not exposed).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub size_bytes: i64,
    pub license: String,
    pub context: String,
    pub note: String,
    pub recommended: bool,
    /// Whether this model's file already resolves in the models dir (downloaded).
    pub installed: bool,
}

/// The notes-model catalog + where downloads land, for the Settings > Models picker.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelCatalog {
    pub items: Vec<CatalogEntry>,
    pub models_dir: String,
}

/// Where a download is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    Idle,
    Downloading,
    Verifying,
    Ready,
    Error,
}

/// A snapshot of the (single, at-a-time) model download, polled by the UI like sidecar readiness.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DownloadState {
    pub status: DownloadStatus,
    /// The catalog id being downloaded (or last downloaded), if any.
    pub model_id: Option<String>,
    pub downloaded_bytes: i64,
    pub total_bytes: i64,
    /// A human-readable detail (an error reason, or the resolved path when ready).
    pub message: Option<String>,
}

/// Request body for starting a catalog download: the catalog `id` to fetch.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DownloadRequest {
    pub id: String,
}

/// Read-only build/runtime facts for the About panel (`protocol_version` is the core's IPC
/// frame-protocol constant; a mismatch with the helper's reported copy signals version drift).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AboutInfo {
    pub app_version: String,
    pub environment: String,
    pub protocol_version: u32,
    pub database_path: String,
}

/// Live TCC permission status probed from the capture helper (not a stored preference). Each field
/// is `granted` / `denied` / `undetermined`, or `unknown` when the helper is unavailable.
/// `helper_version` is the connected helper's build (`None` when unavailable).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PermissionsInfo {
    pub helper_available: bool,
    pub helper_version: Option<String>,
    pub microphone: String,
    pub audio_capture: String,
    pub screen_recording: String,
    pub accessibility: String,
    pub calendar: String,
}

/// The full editable settings, one field per panel/section. `storage_info` and `about` are
/// read-only context (not editable sections).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SettingsRead {
    pub recording: RecordingSettings,
    pub speakers: SpeakerSettings,
    pub storage: StorageSettings,
    pub storage_info: StorageInfo,
    pub models: ModelSettings,
    pub models_info: ModelsInfo,
    pub about: AboutInfo,
}

/// Live engine readiness for the UI to gate "Start" on. `sidecars_ready` is `true` once the
/// pre-warmed transcription sidecars have loaded their models (so a new meeting transcribes
/// immediately) and `false` while they are still loading after launch.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub struct StatusInfo {
    pub sidecars_ready: bool,
}

// Live-transcript WebSocket frames. The socket is not itself an OpenAPI operation, but the frames it
// broadcasts are modeled here (and registered in the OpenAPI components) so the TypeScript client
// codegen's their shapes instead of hand-maintaining them out of the drift gate. The orchestrator
// pipeline serializes the wire bytes today (`hearsay-orchestrator::pipeline`); these mirrors exist
// only for codegen, so their field names + serde renames MUST stay byte-identical to that producer.

/// The `kind` discriminant on a live [`TranscriptEvent`]: a pre-diarization `partial` (streamed, not
/// persisted) or a persisted `final`. Mirrors the orchestrator's `SegmentKind` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptKind {
    Partial,
    Final,
}

/// A live transcript line pushed to the meeting WebSocket: a `partial` (interim, speaker-less for
/// Them) or a `final` (persisted, with its resolved `speaker_label`).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct TranscriptEvent {
    #[schema(inline)]
    pub kind: TranscriptKind,
    pub stream: Stream,
    pub speaker_label: String,
    pub text: String,
    pub start_s: f64,
    pub end_s: f64,
}

/// The constant `kind` discriminant marking a [`StatusEvent`] (`"status"`), distinct from a
/// transcript line's `partial`/`final` so the client routes it off the transcript path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum StatusKind {
    Status,
}

/// Whether the live transcription sidecars are still loading their models (`warming`) or have
/// finished and are serving (`ready`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum WarmState {
    Warming,
    Ready,
}

/// A warm-up status frame (not a transcript line): a `warming` snapshot on connect for a cold start
/// and a `ready` transition once the sidecars finish, so the UI shows a "preparing" notice instead
/// of a silent gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct StatusEvent {
    #[schema(inline)]
    pub kind: StatusKind,
    #[schema(inline)]
    pub state: WarmState,
}

/// The constant `kind` discriminant marking a [`ResyncEvent`] (`"resync"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ResyncKind {
    Resync,
}

/// A backfill signal (not a transcript line): the broadcast buffer dropped events for a lagged
/// subscriber, so the persisted transcript is ahead of this live stream. On receipt the client
/// refetches persisted segments rather than diverging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct ResyncEvent {
    #[schema(inline)]
    pub kind: ResyncKind,
}

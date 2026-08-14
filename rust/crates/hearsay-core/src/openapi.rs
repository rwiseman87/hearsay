//! OpenAPI document (utoipa). Drives the TypeScript codegen for the shared web UI.
//! `GET /openapi.json` serves it; `--dump-openapi` prints it.

use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

use crate::schema::{
    AboutInfo, ArchiveState, CaptureHealthEvent, CaptureStateEvent, CatalogEntry, DownloadRequest,
    DownloadState, DownloadStatus, FolderCreate, FolderRead, FolderReparent, FolderUpdate,
    IdentityRead, IdentityRename, LevelEvent, MeetingCreate, MeetingFolderAssign, MeetingNotesRead,
    MeetingRead, MeetingStatus, MeetingUpdate, ModelCatalog, ModelSettings, ModelsInfo, NotesEdit,
    PermissionsInfo, PromptEvent, RecordingSettings, ResyncEvent, SearchHit, SegmentEdit,
    SegmentRead, SegmentSpeakerAssign, SettingsRead, SetupRequest, SetupState, SetupStatus,
    SetupStep, SetupStepStatus, SpeakerMerge, SpeakerRead, SpeakerRename, SpeakerSettings,
    StatusEvent, StatusInfo, StorageInfo, StorageSettings, Stream, TranscriptEvent, UserNotesRead,
    UserNotesWrite, VoiceprintRead, VoiceprintSampleRead,
};

/// Registers the session-token scheme so an API console can offer an "Authorize" box, and so the
/// document states the auth requirement instead of leaving readers to infer it from a 401.
struct SessionToken;

impl Modify for SessionToken {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "session_token",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .description(Some(
                            "Per-process session token. `make rust-serve` prints it in the \
                             `open:` URL; the packaged app passes it to the webview through a \
                             handshake file. Channels that cannot set a header — the WebSocket \
                             and the audio element — take `?token=` instead.",
                        ))
                        .build(),
                ),
            );
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "hearsay",
        description = "\
Local-first meeting-note transcriber. The core serves this API on `127.0.0.1` only, for the \
bundled web UI.

**Conventions**

- Base path is `/api`. Every request needs the session token; see the `session_token` scheme.
- List endpoints return `{ total, page, page_size, items }` and take `page` (at least 1, \
default 1) and `page_size` (1 to 200, default 50).
- Errors return `{ \"detail\": \"...\" }`. Database and internal failures render as a literal \
\"internal error\" — details go to the log only.
- Times are ISO 8601. Segment `start_s` and `end_s` are seconds relative to the meeting start, \
not wall clock.

**Not in this document**

Three served routes have no OpenAPI representation and are specified in `docs/api.md`: the live \
transcript WebSocket at `/ws/meetings/{id}`, meeting audio at `/api/meetings/{id}/audio` (a byte \
stream with `Range` support), and `/`, which serves the UI with the token injected.",
        license(
            name = "Apache-2.0 WITH Commons-Clause",
            url = "https://github.com/rwiseman87/hearsay/blob/main/LICENSE",
        ),
    ),
    modifiers(&SessionToken),
    security(("session_token" = [])),
    paths(
        crate::routes::meetings::list_meetings,
        crate::routes::meetings::start_meeting,
        crate::routes::meetings::get_meeting,
        crate::routes::meetings::update_meeting,
        crate::routes::meetings::list_segments,
        crate::routes::meetings::edit_segment,
        crate::routes::meetings::reassign_segment_speaker,
        crate::routes::meetings::stop_meeting,
        crate::routes::meetings::keep_recording,
        crate::routes::meetings::pause_meeting,
        crate::routes::meetings::resume_meeting,
        crate::routes::meetings::assign_meeting_folder,
        crate::routes::meetings::delete_meeting,
        crate::routes::meetings::reveal_meeting,
        crate::routes::meetings::read_status,
        crate::routes::folders::list_folders,
        crate::routes::folders::create_folder,
        crate::routes::folders::rename_folder,
        crate::routes::folders::reparent_folder,
        crate::routes::folders::delete_folder,
        crate::routes::speakers::list_speakers,
        crate::routes::speakers::rename_speaker,
        crate::routes::speakers::merge_speakers,
        crate::routes::speakers::rediarize,
        crate::routes::speakers::list_identities,
        crate::routes::voiceprints::list_voiceprints,
        crate::routes::voiceprints::rename_identity,
        crate::routes::voiceprints::delete_voiceprint,
        crate::routes::voiceprints::forget_voice,
        crate::routes::notes::generate_notes,
        crate::routes::notes::read_notes,
        crate::routes::notes::edit_notes,
        crate::routes::user_notes::read_user_notes,
        crate::routes::user_notes::save_user_notes,
        crate::routes::search::search,
        crate::routes::models::catalog,
        crate::routes::models::download_status,
        crate::routes::models::start_download,
        crate::routes::settings::read_settings,
        crate::routes::settings::read_permissions,
        crate::routes::settings::update_recording,
        crate::routes::settings::update_speakers,
        crate::routes::settings::update_storage,
        crate::routes::settings::read_archive,
        crate::routes::settings::start_archive,
        crate::routes::settings::update_models,
        crate::routes::settings::reset_models,
        crate::routes::settings::reveal_output_dir,
        crate::routes::settings::open_notices,
        crate::routes::setup::setup_status,
        crate::routes::setup::start_setup,
    ),
    components(schemas(
        MeetingRead,
        MeetingStatus,
        Stream,
        SegmentRead,
        SegmentEdit,
        SegmentSpeakerAssign,
        SpeakerRead,
        SpeakerMerge,
        SearchHit,
        IdentityRead,
        IdentityRename,
        VoiceprintRead,
        VoiceprintSampleRead,
        FolderRead,
        FolderCreate,
        FolderUpdate,
        FolderReparent,
        MeetingFolderAssign,
        MeetingCreate,
        MeetingUpdate,
        SpeakerRename,
        SettingsRead,
        RecordingSettings,
        SpeakerSettings,
        StorageSettings,
        StorageInfo,
        ArchiveState,
        ModelSettings,
        ModelsInfo,
        MeetingNotesRead,
        NotesEdit,
        UserNotesRead,
        UserNotesWrite,
        CatalogEntry,
        ModelCatalog,
        DownloadStatus,
        DownloadState,
        DownloadRequest,
        SetupStatus,
        SetupStepStatus,
        SetupStep,
        SetupState,
        SetupRequest,
        AboutInfo,
        PermissionsInfo,
        StatusInfo,
        TranscriptEvent,
        StatusEvent,
        ResyncEvent,
        PromptEvent,
        CaptureHealthEvent,
        LevelEvent,
        CaptureStateEvent,
    )),
    tags(
        (name = "meetings", description = "Meeting lifecycle, transcript segments, and playback"),
        (name = "folders", description = "Nested organizational folders for meetings"),
        (name = "speakers", description = "Per-meeting diarization clusters and the cross-meeting identities they bind to"),
        (name = "voiceprints", description = "Stored voice embeddings and the people they recognize"),
        (name = "notes", description = "Local-LLM meeting notes and user-authored \"My notes\""),
        (name = "search", description = "Full-text transcript search across meetings"),
        (name = "models", description = "Notes-model catalog + download manager"),
        (name = "settings", description = "Editable preferences + live permission status"),
        (name = "setup", description = "First-run model download (the installer ships no models)"),
    ),
)]
pub struct ApiDoc;

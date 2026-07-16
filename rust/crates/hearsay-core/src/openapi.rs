//! OpenAPI document (utoipa). Drives the TypeScript codegen for the shared web UI, matching the
//! Python OpenAPI->TS pipeline. `GET /openapi.json` serves it; `--dump-openapi` prints it.

use utoipa::OpenApi;

use crate::schema::{
    AboutInfo, IdentityRead, MeetingCreate, MeetingRead, MeetingStatus, MeetingUpdate,
    ModelSettings, ModelsInfo, PermissionsInfo, RecordingSettings, ResyncEvent, SegmentRead,
    SettingsRead, SpeakerRead, SpeakerRename, SpeakerSettings, StatusEvent, StatusInfo,
    StorageInfo, StorageSettings, Stream, TranscriptEvent,
};

#[derive(OpenApi)]
#[openapi(
    info(title = "hearsay", description = "Local-first meeting-note transcriber — loopback API."),
    paths(
        crate::routes::meetings::list_meetings,
        crate::routes::meetings::start_meeting,
        crate::routes::meetings::get_meeting,
        crate::routes::meetings::update_meeting,
        crate::routes::meetings::list_segments,
        crate::routes::meetings::stop_meeting,
        crate::routes::meetings::delete_meeting,
        crate::routes::meetings::read_status,
        crate::routes::speakers::list_speakers,
        crate::routes::speakers::rename_speaker,
        crate::routes::speakers::rediarize,
        crate::routes::speakers::list_identities,
        crate::routes::settings::read_settings,
        crate::routes::settings::read_permissions,
        crate::routes::settings::update_recording,
        crate::routes::settings::update_speakers,
        crate::routes::settings::update_storage,
        crate::routes::settings::update_models,
        crate::routes::settings::reset_models,
        crate::routes::settings::reveal_output_dir,
    ),
    components(schemas(
        MeetingRead,
        MeetingStatus,
        Stream,
        SegmentRead,
        SpeakerRead,
        IdentityRead,
        MeetingCreate,
        MeetingUpdate,
        SpeakerRename,
        SettingsRead,
        RecordingSettings,
        SpeakerSettings,
        StorageSettings,
        StorageInfo,
        ModelSettings,
        ModelsInfo,
        AboutInfo,
        PermissionsInfo,
        StatusInfo,
        TranscriptEvent,
        StatusEvent,
        ResyncEvent,
    )),
    tags(
        (name = "meetings", description = "Meeting lifecycle + transcript segments"),
        (name = "speakers", description = "Diarization clusters + cross-meeting identities"),
        (name = "settings", description = "Editable preferences + live permission status"),
    ),
)]
pub struct ApiDoc;

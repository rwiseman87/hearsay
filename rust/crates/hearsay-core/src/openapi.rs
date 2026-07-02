//! OpenAPI document (utoipa). Drives the TypeScript codegen for the shared web UI, matching the
//! Python OpenAPI->TS pipeline. `GET /openapi.json` serves it; `--dump-openapi` prints it.

use utoipa::OpenApi;

use crate::schema::{
    IdentityRead, MeetingCreate, MeetingRead, MeetingStatus, SegmentRead, SpeakerRead,
    SpeakerRename, Stream,
};

#[derive(OpenApi)]
#[openapi(
    info(title = "hearsay", description = "Local-first meeting-note transcriber — loopback API."),
    paths(
        crate::routes::meetings::list_meetings,
        crate::routes::meetings::start_meeting,
        crate::routes::meetings::get_meeting,
        crate::routes::meetings::list_segments,
        crate::routes::meetings::stop_meeting,
        crate::routes::meetings::delete_meeting,
        crate::routes::speakers::list_speakers,
        crate::routes::speakers::rename_speaker,
        crate::routes::speakers::rediarize,
        crate::routes::speakers::list_identities,
    ),
    components(schemas(
        MeetingRead,
        MeetingStatus,
        Stream,
        SegmentRead,
        SpeakerRead,
        IdentityRead,
        MeetingCreate,
        SpeakerRename,
    )),
    tags(
        (name = "meetings", description = "Meeting lifecycle + transcript segments"),
        (name = "speakers", description = "Diarization clusters + cross-meeting identities"),
    ),
)]
pub struct ApiDoc;

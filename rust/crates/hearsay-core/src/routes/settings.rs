//! Editable settings + live permission probe.
//!
//! Routers stay thin: resolve the effective value (stored `preferences` override, else the config
//! default), validate, persist, return. `GET /settings` returns every section; `PUT /{section}`
//! updates one. `GET /settings/permissions` is live OS state, not a stored preference.

use std::path::{Path, PathBuf};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::Router;

use hearsay_db::queries;
use hearsay_db::queries::{SECTION_MODELS, SECTION_RECORDING, SECTION_SPEAKERS, SECTION_STORAGE};

use crate::config::Settings;
use crate::error::{ApiError, ApiResult};
use crate::extract::Json;
use crate::schema::{
    AboutInfo, ArchiveState, ModelSettings, ModelsInfo, PermissionsInfo, RecordingSettings,
    SettingsRead, SpeakerSettings, StorageInfo, StorageSettings,
};
use crate::state::AppState;

/// Upper bound on the editable notes prompt template (characters). Bounds the tokenized prompt at
/// the boundary (the transcript itself is separately capped in the prompt builder) so an over-large
/// paste is a 422, never a runaway llama.cpp context.
const MAX_NOTES_PROMPT_LEN: usize = 8_000;

/// Routes served under the `/api` prefix (token-gated by the caller).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/settings", get(read_settings))
        .route("/settings/permissions", get(read_permissions))
        .route("/settings/recording", put(update_recording))
        .route("/settings/speakers", put(update_speakers))
        .route("/settings/storage", put(update_storage))
        .route(
            "/settings/storage/compress",
            get(read_archive).post(start_archive),
        )
        .route("/settings/models", put(update_models).delete(reset_models))
        .route("/settings/reveal", post(reveal_output_dir))
        .route("/settings/notices", post(open_notices))
}

/// The local DB file path for display; avoid leaking credentials for a remote DB URL: strip the
/// `sqlite://` scheme, else report an external database.
fn database_path(url: &str) -> String {
    if !url.starts_with("sqlite") {
        return "(external database)".to_string();
    }
    url.split_once("://")
        .map(|(_, rest)| rest.to_string())
        .unwrap_or_else(|| url.to_string())
}

fn about(settings: &Settings) -> AboutInfo {
    AboutInfo {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        database_path: database_path(&settings.database_url),
    }
}

async fn resolve_recording(state: &AppState) -> ApiResult<RecordingSettings> {
    // Resolve each field against its config default rather than a strict struct parse: a row that
    // predates the inactivity fields (only `record`) must still resolve, filling the missing fields
    // from the environment/config default. Mirrors the per-field `resolve_models`.
    let s = queries::Section::load(&state.pool, SECTION_RECORDING).await?;
    Ok(RecordingSettings {
        record: s.bool_field("record", state.settings.record),
        inactivity_prompt_enabled: s.bool_field(
            "inactivity_prompt_enabled",
            state.settings.inactivity_prompt,
        ),
        inactivity_auto_end_enabled: s.bool_field(
            "inactivity_auto_end_enabled",
            state.settings.inactivity_auto_end,
        ),
        inactivity_prompt_minutes: s
            .u64_field(
                "inactivity_prompt_minutes",
                state.settings.inactivity_prompt_minutes,
            )
            .min(u64::from(u32::MAX)) as u32,
        inactivity_end_minutes: s
            .u64_field(
                "inactivity_end_minutes",
                state.settings.inactivity_end_minutes,
            )
            .min(u64::from(u32::MAX)) as u32,
    })
}

/// Bound the inactivity thresholds at the boundary so a bad value never reaches the watchdog. The
/// prompt and the auto-end are independently toggleable: each enabled threshold must be 1..=1440
/// minutes, and when both are on the auto-end must be strictly after the prompt. A disabled
/// threshold's minutes are inert, so they are not checked.
fn validate_recording(body: &RecordingSettings) -> ApiResult<()> {
    if body.inactivity_prompt_enabled && !(1..=1440).contains(&body.inactivity_prompt_minutes) {
        return Err(ApiError::Unprocessable(
            "inactivity_prompt_minutes must be between 1 and 1440".into(),
        ));
    }
    if body.inactivity_auto_end_enabled && !(1..=1440).contains(&body.inactivity_end_minutes) {
        return Err(ApiError::Unprocessable(
            "inactivity_end_minutes must be between 1 and 1440".into(),
        ));
    }
    if body.inactivity_prompt_enabled
        && body.inactivity_auto_end_enabled
        && body.inactivity_end_minutes <= body.inactivity_prompt_minutes
    {
        return Err(ApiError::Unprocessable(
            "inactivity_end_minutes must be greater than inactivity_prompt_minutes".into(),
        ));
    }
    Ok(())
}

async fn resolve_speakers(state: &AppState) -> ApiResult<SpeakerSettings> {
    match queries::get_preference(&state.pool, SECTION_SPEAKERS).await? {
        Some(json) => serde_json::from_str(&json)
            .map_err(|e| ApiError::Internal(format!("corrupt speakers preference: {e}"))),
        None => Ok(SpeakerSettings {
            auto_refine: state.settings.auto_refine,
            recognition_threshold: state.settings.recognition_threshold,
        }),
    }
}

async fn resolve_storage(state: &AppState) -> ApiResult<StorageSettings> {
    // Resolve each field against its CONFIG default rather than deserializing the section as a
    // struct: every install that saved a recordings folder before archival existed holds a
    // `{"output_dir": ...}` row, and a struct parse resolves the absent keys to their *type*
    // defaults (false / 0) — shipping the feature silently disabled on exactly the installs that
    // have the most audio to reclaim. Mirrors the per-field `resolve_recording` / `resolve_models`.
    let s = queries::Section::load(&state.pool, SECTION_STORAGE).await?;
    Ok(StorageSettings {
        output_dir: s
            .path_field("output_dir", &state.settings.output_dir)
            .to_string_lossy()
            .to_string(),
        compress_audio: s.bool_field("compress_audio", state.settings.compress_audio),
        compress_after_days: s
            .u64_field("compress_after_days", state.settings.compress_after_days)
            .min(u64::from(u32::MAX)) as u32,
    })
}

async fn resolve_models(state: &AppState) -> ApiResult<ModelSettings> {
    // Resolve each field independently against its config default rather than deserializing the whole
    // section as a struct: the download manager merges in just `notes_model`, so the stored object is
    // often partial, which a strict struct parse would reject. Mirrors the per-field `effective_*`
    // readers in `hearsay-db`.
    let s = queries::Section::load(&state.pool, SECTION_MODELS).await?;
    Ok(ModelSettings {
        notes_enabled: s.bool_field("notes_enabled", state.settings.notes_enabled),
        notes_model: s
            .path_field("notes_model", &state.settings.notes_model)
            .to_string_lossy()
            .to_string(),
        notes_prompt: s.string_field("notes_prompt", &state.settings.notes_prompt),
    })
}

fn models_info(state: &AppState, effective: &ModelSettings) -> ModelsInfo {
    ModelsInfo {
        default_notes_model: state.settings.notes_model.to_string_lossy().to_string(),
        // An empty notes_model is "unset", not "missing file" — report it as not-resolving.
        notes_model_exists: !effective.notes_model.is_empty()
            && Path::new(&effective.notes_model).is_file(),
        default_notes_prompt: state.settings.notes_prompt.clone(),
    }
}

async fn storage_info(state: &AppState) -> ApiResult<StorageInfo> {
    let output_dir = resolve_storage(state).await?.output_dir;
    let meeting_count = queries::count_meetings(&state.pool).await?;
    let root = PathBuf::from(&output_dir);
    let (tracked_bytes, uncompressed_bytes) = tokio::task::spawn_blocking(move || dir_size(&root))
        .await
        .map_err(|e| ApiError::Internal(format!("storage scan panicked: {e}")))?;
    Ok(StorageInfo {
        output_dir,
        database_path: database_path(&state.settings.database_url),
        tracked_bytes,
        meeting_count,
        uncompressed_bytes,
    })
}

/// Recursive on-disk size of the recordings tree as `(total_bytes, uncompressed_audio_bytes)`. The
/// Rust core has no asset manifest yet, so this walks the output dir rather than summing a
/// `meeting_assets` table. The second figure buckets the still-uncompressed `audio.wav` files in the
/// same pass — it is what the archival sweep would shrink, and a separate walk would double the cost
/// of every `GET /settings`. Best-effort: unreadable entries are skipped; `DirEntry::metadata` does
/// not follow symlinks (no cycles). A missing root (fresh install) reports 0.
fn dir_size(root: &Path) -> (i64, i64) {
    let mut total: i64 = 0;
    let mut uncompressed: i64 = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.len() as i64);
                if entry.file_name() == hearsay_audio::AUDIO_WAV {
                    uncompressed = uncompressed.saturating_add(meta.len() as i64);
                }
            }
        }
    }
    (total, uncompressed)
}

/// The effective settings: editable sections plus read-only build and storage facts.
///
/// Returns every editable section (`recording`, `speakers`, `storage`, `models`) alongside the
/// read-only `storage_info`, `models_info`, and `about`. Each section's effective value is the
/// stored preference when one exists, else the environment default.
///
/// `models_info` reports the default refine and notes models, whether each file is present on disk,
/// and the default notes prompt, so the client can show what will run without the user having set
/// an override. `storage_info` adds `uncompressed_bytes` — how much is still held in un-archived
/// `audio.wav` files, which is what archiving would reclaim.
#[utoipa::path(
    get, path = "/api/settings", tag = "settings",
    responses((status = 200, body = SettingsRead, description = "The effective settings")),
)]
pub(crate) async fn read_settings(State(state): State<AppState>) -> ApiResult<Json<SettingsRead>> {
    let models = resolve_models(&state).await?;
    let models_info = models_info(&state, &models);
    Ok(Json(SettingsRead {
        recording: resolve_recording(&state).await?,
        speakers: resolve_speakers(&state).await?,
        storage: resolve_storage(&state).await?,
        storage_info: storage_info(&state).await?,
        models,
        models_info,
        about: about(&state.settings),
    }))
}

/// Live OS permission status for the capture helper.
///
/// Briefly spawns the capture helper and reads its permission snapshot and build version; nothing
/// is persisted. Degrades to `helper_available: false` with every field `unknown` when the helper
/// binary is absent, so a source build without the Swift side still renders the panel.
#[utoipa::path(
    get, path = "/api/settings/permissions", tag = "settings",
    responses((status = 200, body = PermissionsInfo, description = "The current permission snapshot")),
)]
pub(crate) async fn read_permissions(State(state): State<AppState>) -> Json<PermissionsInfo> {
    let snapshot = hearsay_backends::probe_permissions(state.settings.helper_path.clone()).await;
    let field = |value: Option<String>| value.unwrap_or_else(|| "unknown".to_string());
    Json(PermissionsInfo {
        helper_available: snapshot.available,
        helper_version: snapshot.helper_version,
        microphone: field(snapshot.microphone),
        audio_capture: field(snapshot.audio_capture),
    })
}

/// Replace the recording and privacy settings.
///
/// Full-replaces the section, so the body must carry every field — omitting one is a `422`, never a
/// silent reset. The inactivity prompt and the silence auto-end are gated independently: each
/// enabled threshold must be 1..=1440 minutes, and when both are on the auto-end must exceed the
/// prompt.
///
/// Takes effect from the next meeting; the effective settings are read at meeting start and stop,
/// so an edit never alters a meeting already in progress.
#[utoipa::path(
    put, path = "/api/settings/recording", tag = "settings",
    request_body = RecordingSettings,
    responses(
        (status = 200, body = RecordingSettings, description = "The stored section"),
        (status = 422, description = "A missing field, or a threshold outside 1..=1440 / out of order"),
    ),
)]
pub(crate) async fn update_recording(
    State(state): State<AppState>,
    Json(body): Json<RecordingSettings>,
) -> ApiResult<Json<RecordingSettings>> {
    validate_recording(&body)?;
    store_section(&state, SECTION_RECORDING, &body).await?;
    Ok(Json(body))
}

/// Replace the speaker-recognition settings.
///
/// Full-replaces the section. `recognition_threshold` is the cosine similarity a stored voiceprint
/// must clear to auto-name a returning speaker: higher is stricter.
#[utoipa::path(
    put, path = "/api/settings/speakers", tag = "settings",
    request_body = SpeakerSettings,
    responses(
        (status = 200, body = SpeakerSettings, description = "The stored section"),
        (status = 422, description = "A missing field, or a recognition threshold outside 0.0..=1.0"),
    ),
)]
pub(crate) async fn update_speakers(
    State(state): State<AppState>,
    Json(body): Json<SpeakerSettings>,
) -> ApiResult<Json<SpeakerSettings>> {
    if !(0.0..=1.0).contains(&body.recognition_threshold) {
        return Err(ApiError::Unprocessable(
            "recognition_threshold must be between 0.0 and 1.0".into(),
        ));
    }
    store_section(&state, SECTION_SPEAKERS, &body).await?;
    Ok(Json(body))
}

/// Replace the storage settings.
///
/// Full-replaces the section. `output_dir` must be absolute, existing, and writable. When
/// `compress_audio` is on, `compress_after_days` must be 1..=365 — a disabled threshold is inert,
/// and 0 is rejected because archiving the moment a meeting finalizes would race the post-stop
/// refine.
#[utoipa::path(
    put, path = "/api/settings/storage", tag = "settings",
    request_body = StorageSettings,
    responses(
        (status = 200, body = StorageSettings, description = "The stored section, with the output directory resolved"),
        (status = 422, description = "A missing field, an unusable output directory, or a compression threshold outside 1..=365"),
    ),
)]
pub(crate) async fn update_storage(
    State(state): State<AppState>,
    Json(body): Json<StorageSettings>,
) -> ApiResult<Json<StorageSettings>> {
    let input = body.output_dir.trim().to_string();
    if input.is_empty() {
        return Err(ApiError::Unprocessable(
            "output_dir must not be empty".into(),
        ));
    }
    validate_compression(&body)?;
    let resolved = tokio::task::spawn_blocking(move || validate_output_dir(&input))
        .await
        .map_err(|e| ApiError::Internal(format!("output_dir validation panicked: {e}")))??;
    let stored = StorageSettings {
        output_dir: resolved,
        compress_audio: body.compress_audio,
        compress_after_days: body.compress_after_days,
    };
    store_section(&state, SECTION_STORAGE, &stored).await?;
    Ok(Json(stored))
}

/// Replace the model settings.
///
/// Full-replaces the section: whether notes generation is enabled, which GGUF model runs it, and
/// the prompt template. The notes model path must exist and be a GGUF. The prompt's `{transcript}`
/// placeholder is filled at generation time.
#[utoipa::path(
    put, path = "/api/settings/models", tag = "settings",
    request_body = ModelSettings,
    responses(
        (status = 200, body = ModelSettings, description = "The stored section"),
        (status = 422, description = "A missing field, a model path that is absent or not a GGUF, or an over-long prompt"),
    ),
)]
pub(crate) async fn update_models(
    State(state): State<AppState>,
    Json(body): Json<ModelSettings>,
) -> ApiResult<Json<ModelSettings>> {
    // The notes model is optional: empty means "not chosen yet" (the notes step stays unavailable
    // until one is downloaded/selected). Validate the file only when a path is provided.
    let notes_input = body.notes_model.trim().to_string();
    let notes_model = if notes_input.is_empty() {
        String::new()
    } else {
        tokio::task::spawn_blocking(move || validate_notes_model(&notes_input))
            .await
            .map_err(|e| ApiError::Internal(format!("notes_model validation panicked: {e}")))??
    };

    // The prompt template is optional: empty means "use the built-in default". Trim and length-cap
    // it at the boundary so an over-large paste is a 422, never a runaway prompt at generate time.
    let notes_prompt = body.notes_prompt.trim().to_string();
    if notes_prompt.chars().count() > MAX_NOTES_PROMPT_LEN {
        return Err(ApiError::Unprocessable(format!(
            "notes_prompt exceeds {MAX_NOTES_PROMPT_LEN} characters"
        )));
    }

    let stored = ModelSettings {
        notes_enabled: body.notes_enabled,
        notes_model,
        notes_prompt,
    };
    store_section(&state, SECTION_MODELS, &stored).await?;
    Ok(Json(stored))
}

/// Reset the model settings to the environment defaults.
///
/// Clears the stored `models` overrides so the section falls back to what the environment
/// configures, and returns the resulting effective section.
#[utoipa::path(
    delete, path = "/api/settings/models", tag = "settings",
    responses((status = 200, body = ModelSettings, description = "The effective section after the reset")),
)]
pub(crate) async fn reset_models(State(state): State<AppState>) -> ApiResult<Json<ModelSettings>> {
    queries::clear_preference(&state.pool, SECTION_MODELS).await?;
    Ok(Json(resolve_models(&state).await?))
}

/// Open the effective recordings directory in the OS file manager. Runs in the core (a native
/// process in the user's login session), reached over the same-origin HTTP API the rest of Settings
/// uses — the desktop shell's Tauri `invoke()` is not reliably reachable from the webview's remote
/// loopback origin, so the "Reveal data folder" button routes here instead. On failure the reason is
/// surfaced to the client (not collapsed to a generic 500) so a broken reveal is diagnosable.
#[utoipa::path(
    post, path = "/api/settings/reveal", tag = "settings",
    responses(
        (status = 204, description = "Handed off to the file manager"),
        (status = 503, description = "The directory could not be opened"),
    ),
)]
pub(crate) async fn reveal_output_dir(State(state): State<AppState>) -> ApiResult<StatusCode> {
    let dir = PathBuf::from(resolve_storage(&state).await?.output_dir);
    let _ = tokio::fs::create_dir_all(&dir).await; // best-effort; open still surfaces a real failure
    tokio::task::spawn_blocking(move || reveal_in_file_manager(&dir))
        .await
        .map_err(|e| ApiError::Internal(format!("reveal task panicked: {e}")))??;
    Ok(StatusCode::NO_CONTENT)
}

/// Open the bundled third-party notices in the OS default handler. Attribution for the CC BY 4.0
/// model weights has to reach the user from the distributed app, so the notices ship as a bundle
/// resource and Settings > About opens this copy. Routed through the core for the same reason as
/// [`reveal_output_dir`].
#[utoipa::path(
    post, path = "/api/settings/notices", tag = "settings",
    responses(
        (status = 204, description = "Handed off to the default handler"),
        (status = 503, description = "The notices file is missing or could not be opened"),
    ),
)]
pub(crate) async fn open_notices(State(state): State<AppState>) -> ApiResult<StatusCode> {
    let path = state.settings.notices_path.clone();
    if !path.is_file() {
        return Err(ApiError::Unavailable(format!(
            "third-party notices not found at {}",
            path.display()
        )));
    }
    tokio::task::spawn_blocking(move || reveal_in_file_manager(&path))
        .await
        .map_err(|e| ApiError::Internal(format!("notices task panicked: {e}")))??;
    Ok(StatusCode::NO_CONTENT)
}

/// Open `dir` in Finder via an absolute `/usr/bin/open` (no PATH dependency from the bundled app's
/// minimal process environment). `dir` is app-controlled (the effective recordings dir or the
/// bundled notices file), never user-supplied, so there is no argument-injection surface. Errors
/// carry the reason for the UI.
#[cfg(target_os = "macos")]
pub(crate) fn reveal_in_file_manager(dir: &Path) -> ApiResult<()> {
    tracing::info!(path = %dir.display(), "reveal: opening in Finder");
    let status = std::process::Command::new("/usr/bin/open")
        .arg(dir)
        .status()
        .map_err(|e| ApiError::Unavailable(format!("could not launch /usr/bin/open: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(ApiError::Unavailable(format!(
            "/usr/bin/open exited with {status} for {}",
            dir.display()
        )))
    }
}

/// Serialize a settings section to JSON and upsert its `preferences` row.
async fn store_section<T: serde::Serialize>(
    state: &AppState,
    section: &str,
    value: &T,
) -> ApiResult<()> {
    let json = serde_json::to_string(value)
        .map_err(|e| ApiError::Internal(format!("serialize {section} preference: {e}")))?;
    queries::set_preference(&state.pool, section, &json).await?;
    Ok(())
}

/// Resolve `input` to an absolute, existing, writable directory or a 422:
/// expand `~`, require absolute, resolve, is-dir, write-probe.
fn archive_state(sweeper: &hearsay_backends::archive::Sweeper) -> ArchiveState {
    progress_to_state(sweeper.progress())
}

fn progress_to_state(p: hearsay_backends::archive::SweepProgress) -> ArchiveState {
    ArchiveState {
        running: p.running,
        total: p.total as u32,
        done: p.done as u32,
        compressed: p.compressed as u32,
        failed: p.failed as u32,
        reclaimed_bytes: p.reclaimed_bytes as i64,
    }
}

/// The archival pass's current state, for polling.
///
/// The same snapshot the POST returns: whether a pass is running, how many meetings it has to do,
/// how far it has got, and how many bytes it has reclaimed.
#[utoipa::path(
    get, path = "/api/settings/storage/compress", tag = "settings",
    responses((status = 200, body = ArchiveState, description = "The current archival state")),
)]
pub(crate) async fn read_archive(State(state): State<AppState>) -> Json<ArchiveState> {
    Json(archive_state(&state.archive))
}

/// Run the archival pass now instead of waiting for the periodic sweep.
///
/// Returns immediately with the starting snapshot and does the work in the background -- a backlog
/// can take a minute of CPU, far longer than a request should hold. The UI polls the GET above.
/// Honors the effective age threshold, so pressing the button never archives a meeting the user's
/// own setting says is still too recent; it does not require the automatic sweep to be enabled,
/// since pressing it is an explicit instruction.
#[utoipa::path(
    post, path = "/api/settings/storage/compress", tag = "settings",
    responses(
        (status = 202, body = ArchiveState, description = "Accepted; the work list is already counted, so `total` is real and `running` is true"),
        (status = 409, description = "A meeting is recording, or a pass is already running"),
    ),
)]
pub(crate) async fn start_archive(
    State(state): State<AppState>,
) -> ApiResult<(StatusCode, Json<ArchiveState>)> {
    if state.engine.active_meeting().is_some() {
        return Err(ApiError::Conflict(
            "a meeting is recording; archiving would compete with it".into(),
        ));
    }
    if state.archive.is_running() {
        return Err(ApiError::Conflict("archiving is already running".into()));
    }
    let (_enabled, days) = queries::effective_compression(
        &state.pool,
        state.settings.compress_audio,
        state.settings.compress_after_days,
    )
    .await?;

    let started = hearsay_backends::archive::start_background_pass(
        state.pool.clone(),
        state.settings.output_dir.clone(),
        days,
        state.archive.clone(),
        state.engine.clone(),
    )
    .await
    .ok_or_else(|| ApiError::Conflict("archiving is already running".into()))?;
    Ok((StatusCode::ACCEPTED, Json(progress_to_state(started))))
}

/// Bound the archival threshold at the boundary so a bad value never reaches the sweep. Like the
/// inactivity thresholds, the value is only checked when the feature is on — a disabled threshold is
/// inert. Zero is rejected even so: archiving the instant a meeting finalizes would race the
/// post-stop refine, which is still reading the WAV.
fn validate_compression(body: &StorageSettings) -> ApiResult<()> {
    if body.compress_audio && !(1..=365).contains(&body.compress_after_days) {
        return Err(ApiError::Unprocessable(
            "compress_after_days must be between 1 and 365".into(),
        ));
    }
    Ok(())
}

fn validate_output_dir(input: &str) -> Result<String, ApiError> {
    let expanded = expand_home(input);
    let path = Path::new(&expanded);
    if !path.is_absolute() {
        return Err(ApiError::Unprocessable(
            "output_dir must be an absolute path".into(),
        ));
    }
    let resolved = std::fs::canonicalize(path)
        .map_err(|_| ApiError::Unprocessable(format!("{expanded} is not an existing directory")))?;
    if !resolved.is_dir() {
        return Err(ApiError::Unprocessable(format!(
            "{} is not a directory",
            resolved.display()
        )));
    }
    let probe = resolved.join(".hearsay-write-test");
    std::fs::write(&probe, b"")
        .and_then(|()| std::fs::remove_file(&probe))
        .map_err(|_| ApiError::Unprocessable(format!("{} is not writable", resolved.display())))?;
    Ok(resolved.to_string_lossy().to_string())
}

/// Resolve a user-supplied model path and prove it is the expected format: expand `~`, require an
/// absolute path, canonicalize it, and check the leading four magic bytes. `field` names the setting
/// in the 422 and `expected` describes the format.
fn validate_model_file(
    input: &str,
    field: &str,
    magic: [u8; 4],
    expected: &str,
) -> Result<String, ApiError> {
    let expanded = expand_home(input);
    let path = Path::new(&expanded);
    if !path.is_absolute() {
        return Err(ApiError::Unprocessable(format!(
            "{field} must be an absolute path"
        )));
    }
    let resolved = std::fs::canonicalize(path)
        .map_err(|_| ApiError::Unprocessable(format!("{expanded} does not exist")))?;
    if !resolved.is_file() {
        return Err(ApiError::Unprocessable(format!(
            "{} is not a file",
            resolved.display()
        )));
    }
    let mut found = [0u8; 4];
    std::fs::File::open(&resolved)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut found))
        .map_err(|_| ApiError::Unprocessable(format!("{} is not readable", resolved.display())))?;
    if found != magic {
        return Err(ApiError::Unprocessable(format!(
            "{} is not {expected}",
            resolved.display()
        )));
    }
    Ok(resolved.to_string_lossy().to_string())
}

/// Resolve `input` to an absolute, existing, readable GGUF file or a 422 — the notes step loads this
/// model with llama.cpp at each run, so reject a bad path at the boundary. Same shape as
/// `validate_model_file` but checks the **GGUF** magic (the ASCII bytes `GGUF` = `0x47 0x47 0x55
/// 0x46`, the first 4 bytes of every `.gguf` model) so pointing the notes step at a non-GGUF file
/// or an unrelated file is caught here, not as a cryptic llama.cpp load failure at generate time.
fn validate_notes_model(input: &str) -> Result<String, ApiError> {
    validate_model_file(
        input,
        "notes_model",
        crate::models::GGUF_MAGIC,
        "a GGUF model (expected a .gguf file)",
    )
}

/// Expand a leading `~/` to `$HOME`; otherwise unchanged.
fn expand_home(input: &str) -> String {
    match input.strip_prefix("~/") {
        Some(rest) => match std::env::var("HOME") {
            Ok(home) => format!("{home}/{rest}"),
            Err(_) => input.to_string(),
        },
        None => input.to_string(),
    }
}

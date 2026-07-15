//! Editable settings + live permission probe. Port of `src/hearsay/api/settings.py` +
//! `services/settings.py` + `services/permissions.py`.
//!
//! Routers stay thin: resolve the effective value (stored `preferences` override, else the config
//! default), validate, persist, return. `GET /settings` returns every section; `PUT /{section}`
//! updates one. `GET /settings/permissions` is live OS state, not a stored preference.

use std::path::{Path, PathBuf};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};

use hearsay_db::queries;
use hearsay_db::queries::{SECTION_MODELS, SECTION_RECORDING, SECTION_SPEAKERS, SECTION_STORAGE};

use crate::config::Settings;
use crate::error::{ApiError, ApiResult};
use crate::schema::{
    AboutInfo, ModelSettings, ModelsInfo, PermissionsInfo, RecordingSettings, SettingsRead,
    SpeakerSettings, StorageInfo, StorageSettings,
};
use crate::state::AppState;

/// Routes served under the `/api` prefix (token-gated by the caller).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/settings", get(read_settings))
        .route("/settings/permissions", get(read_permissions))
        .route("/settings/recording", put(update_recording))
        .route("/settings/speakers", put(update_speakers))
        .route("/settings/storage", put(update_storage))
        .route("/settings/models", put(update_models).delete(reset_models))
        .route("/settings/reveal", post(reveal_output_dir))
}

/// The local DB file path for display; avoid leaking credentials for a remote DB URL. Mirrors the
/// Python `_database_path` (strip the `sqlite://` scheme, else report an external database).
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
        environment: settings.environment.clone(),
        protocol_version: u32::from(hearsay_ipc::VERSION),
        database_path: database_path(&settings.database_url),
    }
}

async fn resolve_recording(state: &AppState) -> ApiResult<RecordingSettings> {
    match queries::get_preference(&state.pool, SECTION_RECORDING).await? {
        Some(json) => serde_json::from_str(&json)
            .map_err(|e| ApiError::Internal(format!("corrupt recording preference: {e}"))),
        None => Ok(RecordingSettings {
            record: state.settings.record,
        }),
    }
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
    match queries::get_preference(&state.pool, SECTION_STORAGE).await? {
        Some(json) => serde_json::from_str(&json)
            .map_err(|e| ApiError::Internal(format!("corrupt storage preference: {e}"))),
        None => Ok(StorageSettings {
            output_dir: state.settings.output_dir.to_string_lossy().to_string(),
        }),
    }
}

async fn resolve_models(state: &AppState) -> ApiResult<ModelSettings> {
    match queries::get_preference(&state.pool, SECTION_MODELS).await? {
        Some(json) => serde_json::from_str(&json)
            .map_err(|e| ApiError::Internal(format!("corrupt models preference: {e}"))),
        None => Ok(ModelSettings {
            refine_model: state.settings.refine_model.to_string_lossy().to_string(),
        }),
    }
}

fn models_info(state: &AppState, effective: &ModelSettings) -> ModelsInfo {
    ModelsInfo {
        default_refine_model: state.settings.refine_model.to_string_lossy().to_string(),
        refine_model_exists: Path::new(&effective.refine_model).is_file(),
    }
}

async fn storage_info(state: &AppState) -> ApiResult<StorageInfo> {
    let output_dir = resolve_storage(state).await?.output_dir;
    let meeting_count = queries::count_meetings(&state.pool).await?;
    let root = PathBuf::from(&output_dir);
    let tracked_bytes = tokio::task::spawn_blocking(move || dir_size(&root))
        .await
        .map_err(|e| ApiError::Internal(format!("storage scan panicked: {e}")))?;
    Ok(StorageInfo {
        output_dir,
        database_path: database_path(&state.settings.database_url),
        tracked_bytes,
        meeting_count,
    })
}

/// Recursive on-disk size (bytes) of the recordings tree. The Rust core has no asset manifest yet,
/// so this walks the output dir rather than summing a `meeting_assets` table. Best-effort:
/// unreadable entries are skipped; `DirEntry::metadata` does not follow symlinks (no cycles). A
/// missing root (fresh install) reports 0.
fn dir_size(root: &Path) -> i64 {
    let mut total: i64 = 0;
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
            }
        }
    }
    total
}

#[utoipa::path(get, path = "/api/settings", tag = "settings", responses((status = 200, body = SettingsRead)))]
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

#[utoipa::path(get, path = "/api/settings/permissions", tag = "settings", responses((status = 200, body = PermissionsInfo)))]
pub(crate) async fn read_permissions(State(state): State<AppState>) -> Json<PermissionsInfo> {
    let snapshot = hearsay_capture::probe_permissions(state.settings.helper_path.clone()).await;
    let field = |value: Option<String>| value.unwrap_or_else(|| "unknown".to_string());
    Json(PermissionsInfo {
        helper_available: snapshot.available,
        helper_version: snapshot.helper_version,
        microphone: field(snapshot.microphone),
        audio_capture: field(snapshot.audio_capture),
        screen_recording: field(snapshot.screen_recording),
        accessibility: field(snapshot.accessibility),
        calendar: field(snapshot.calendar),
    })
}

#[utoipa::path(
    put, path = "/api/settings/recording", tag = "settings",
    request_body = RecordingSettings, responses((status = 200, body = RecordingSettings)),
)]
pub(crate) async fn update_recording(
    State(state): State<AppState>,
    Json(body): Json<RecordingSettings>,
) -> ApiResult<Json<RecordingSettings>> {
    store_section(&state, SECTION_RECORDING, &body).await?;
    Ok(Json(body))
}

#[utoipa::path(
    put, path = "/api/settings/speakers", tag = "settings",
    request_body = SpeakerSettings, responses((status = 200, body = SpeakerSettings), (status = 422)),
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

#[utoipa::path(
    put, path = "/api/settings/storage", tag = "settings",
    request_body = StorageSettings, responses((status = 200, body = StorageSettings), (status = 422)),
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
    let resolved = tokio::task::spawn_blocking(move || validate_output_dir(&input))
        .await
        .map_err(|e| ApiError::Internal(format!("output_dir validation panicked: {e}")))??;
    let stored = StorageSettings {
        output_dir: resolved,
    };
    store_section(&state, SECTION_STORAGE, &stored).await?;
    Ok(Json(stored))
}

#[utoipa::path(
    put, path = "/api/settings/models", tag = "settings",
    request_body = ModelSettings, responses((status = 200, body = ModelSettings), (status = 422)),
)]
pub(crate) async fn update_models(
    State(state): State<AppState>,
    Json(body): Json<ModelSettings>,
) -> ApiResult<Json<ModelSettings>> {
    let input = body.refine_model.trim().to_string();
    if input.is_empty() {
        return Err(ApiError::Unprocessable(
            "refine_model must not be empty".into(),
        ));
    }
    let resolved = tokio::task::spawn_blocking(move || validate_refine_model(&input))
        .await
        .map_err(|e| ApiError::Internal(format!("refine_model validation panicked: {e}")))??;
    let stored = ModelSettings {
        refine_model: resolved,
    };
    store_section(&state, SECTION_MODELS, &stored).await?;
    Ok(Json(stored))
}

#[utoipa::path(
    delete, path = "/api/settings/models", tag = "settings",
    responses((status = 200, body = ModelSettings)),
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
    responses((status = 204), (status = 503)),
)]
pub(crate) async fn reveal_output_dir(State(state): State<AppState>) -> ApiResult<StatusCode> {
    let dir = PathBuf::from(resolve_storage(&state).await?.output_dir);
    let _ = tokio::fs::create_dir_all(&dir).await; // best-effort; open still surfaces a real failure
    tokio::task::spawn_blocking(move || reveal_in_file_manager(&dir))
        .await
        .map_err(|e| ApiError::Internal(format!("reveal task panicked: {e}")))??;
    Ok(StatusCode::NO_CONTENT)
}

/// Open `dir` in Finder via an absolute `/usr/bin/open` (no PATH dependency from the bundled app's
/// minimal process environment). `dir` is app-controlled (the effective recordings dir), never
/// user-supplied, so there is no argument-injection surface. Errors carry the reason for the UI.
#[cfg(target_os = "macos")]
fn reveal_in_file_manager(dir: &Path) -> ApiResult<()> {
    tracing::info!(dir = %dir.display(), "reveal: opening recordings dir in Finder");
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

/// Non-macOS placeholder: the Windows port (planned) will use `explorer`; other targets have no
/// file manager to drive.
#[cfg(not(target_os = "macos"))]
fn reveal_in_file_manager(_dir: &Path) -> ApiResult<()> {
    Err(ApiError::Unavailable(
        "revealing the recordings folder is not supported on this platform".into(),
    ))
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

/// Resolve `input` to an absolute, existing, writable directory or a 422. Mirrors the Python
/// `_validate_output_dir` (expand `~`, require absolute, resolve, is-dir, write-probe).
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

/// Resolve `input` to an absolute, existing, readable GGML whisper model file or a 422. The refine
/// loads this model at each run, so reject a bad path at the boundary (empty, non-absolute, missing,
/// a directory, or not a whisper model) instead of surfacing a cryptic whisper load failure at
/// refine time. The GGML magic check (little-endian `0x67676d6c`, the first 4 bytes of every
/// `ggml-*.bin` whisper model) guards against pointing the refine at an unrelated file.
fn validate_refine_model(input: &str) -> Result<String, ApiError> {
    let expanded = expand_home(input);
    let path = Path::new(&expanded);
    if !path.is_absolute() {
        return Err(ApiError::Unprocessable(
            "refine_model must be an absolute path".into(),
        ));
    }
    let resolved = std::fs::canonicalize(path)
        .map_err(|_| ApiError::Unprocessable(format!("{expanded} does not exist")))?;
    if !resolved.is_file() {
        return Err(ApiError::Unprocessable(format!(
            "{} is not a file",
            resolved.display()
        )));
    }
    let mut magic = [0u8; 4];
    std::fs::File::open(&resolved)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut magic))
        .map_err(|_| ApiError::Unprocessable(format!("{} is not readable", resolved.display())))?;
    // Whisper `GGML_FILE_MAGIC` (0x67676d6c) stored little-endian on disk.
    if magic != [0x6c, 0x6d, 0x67, 0x67] {
        return Err(ApiError::Unprocessable(format!(
            "{} is not a GGML whisper model (expected a ggml-*.bin file)",
            resolved.display()
        )));
    }
    Ok(resolved.to_string_lossy().to_string())
}

/// Expand a leading `~/` to `$HOME` (matching Python's `expanduser`); otherwise unchanged.
fn expand_home(input: &str) -> String {
    match input.strip_prefix("~/") {
        Some(rest) => match std::env::var("HOME") {
            Ok(home) => format!("{home}/{rest}"),
            Err(_) => input.to_string(),
        },
        None => input.to_string(),
    }
}

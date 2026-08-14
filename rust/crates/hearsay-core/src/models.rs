//! The notes-model download manager: an in-app catalog of small, ungated, permissively-licensed
//! GGUF instruct models and a single-at-a-time **anonymous** download (resumable, SHA256-verified)
//! into the models dir, driving the Settings > Models picker. Generalizes the FluidAudio
//! "download on first launch" pattern to a user-chosen model — no HuggingFace login/token, because
//! every catalog repo is public and ungated.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::schema::{CatalogEntry, DownloadState, DownloadStatus, ModelCatalog};

/// Leading bytes of a GGUF file (the ASCII `GGUF`) — the notes models.
pub(crate) const GGUF_MAGIC: [u8; 4] = *b"GGUF";
/// Whisper's `GGML_FILE_MAGIC` (0x67676d6c) as stored little-endian — the refine models.
pub(crate) const GGML_MAGIC: [u8; 4] = [0x6c, 0x6d, 0x67, 0x67];

/// A downloadable model file: its (internal) HuggingFace source + the integrity metadata enforced
/// after download. Shared by the notes catalog and the first-run setup, which fetches the refine
/// model the same resumable, hash-checked way.
pub(crate) struct Source {
    repo: &'static str,
    file: &'static str,
    /// HF-reported content SHA256 (its LFS `x-linked-etag`), enforced after download.
    sha256: &'static str,
    size_bytes: i64,
}

impl Source {
    fn url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/main/{}",
            self.repo, self.file
        )
    }

    pub(crate) fn size_bytes(&self) -> i64 {
        self.size_bytes
    }
}

/// One catalog model: where to fetch it plus what the picker shows.
struct Model {
    id: &'static str,
    name: &'static str,
    source: Source,
    license: &'static str,
    context: &'static str,
    note: &'static str,
    recommended: bool,
}

/// The curated catalog: small, ungated, permissively-licensed GGUF instruct models (Q4_K_M), all
/// resolving anonymously from HuggingFace. Sizes + SHA256 are the HF-reported values, verified at
/// authoring time; a swapped-out file fails the integrity check rather than loading silently.
const CATALOG: &[Model] = &[
    Model {
        id: "qwen3-4b-instruct-2507",
        name: "Qwen3-4B-Instruct-2507",
        source: Source {
            repo: "unsloth/Qwen3-4B-Instruct-2507-GGUF",
            file: "Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
            sha256: "3605803b982cb64aead44f6c1b2ae36e3acdb41d8e46c8a94c6533bc4c67e597",
            size_bytes: 2_497_281_120,
        },
        license: "Apache-2.0",
        context: "256K",
        note: "Best quality; 256K context fits a whole meeting in one pass",
        recommended: true,
    },
    Model {
        id: "qwen3-1.7b",
        name: "Qwen3-1.7B",
        source: Source {
            repo: "unsloth/Qwen3-1.7B-GGUF",
            file: "Qwen3-1.7B-Q4_K_M.gguf",
            sha256: "b139949c5bd74937ad8ed8c8cf3d9ffb1e99c866c823204dc42c0d91fa181897",
            size_bytes: 1_107_409_472,
        },
        license: "Apache-2.0",
        context: "32K",
        note: "Lightweight; comfortable on 8 GB machines",
        recommended: false,
    },
    Model {
        id: "smollm3-3b",
        name: "SmolLM3-3B",
        source: Source {
            repo: "unsloth/SmolLM3-3B-GGUF",
            file: "SmolLM3-3B-Q4_K_M.gguf",
            sha256: "4de907d2d388a5508fb7cb443a06effe14cce3518b0a78d3bdd9e74d9edce989",
            size_bytes: 1_915_306_528,
        },
        license: "Apache-2.0",
        context: "128K",
        note: "Fully-open middle ground",
        recommended: false,
    },
];

/// The whisper refine models the first-run setup knows how to fetch, keyed by file name: setup
/// downloads the one the configured refine path names, so an install pointed at some other model
/// skips the step rather than fetching a model it will never load. Sizes + SHA256 are HuggingFace's
/// reported values (its LFS oid), verified against the file at authoring time.
const REFINE_SOURCES: &[Source] = &[
    // The macOS default.
    Source {
        repo: "ggerganov/whisper.cpp",
        file: "ggml-large-v3-turbo.bin",
        sha256: "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
        size_bytes: 1_624_555_275,
    },
    // The Windows default (bundled by the installer today; here so an install that loses it can
    // fetch it back).
    Source {
        repo: "ggerganov/whisper.cpp",
        file: "ggml-small.en.bin",
        sha256: "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
        size_bytes: 487_614_201,
    },
];

fn find(id: &str) -> Option<&'static Model> {
    CATALOG.iter().find(|m| m.id == id)
}

/// The catalog source for a configured refine-model path, matched on its file name.
pub(crate) fn refine_source(path: &Path) -> Option<&'static Source> {
    let name = path.file_name()?.to_str()?;
    REFINE_SOURCES.iter().find(|s| s.file == name)
}

/// The catalog source for a notes-model id (what the first-run setup downloads when the user picks
/// one alongside the required models).
pub(crate) fn notes_source(id: &str) -> Option<&'static Source> {
    find(id).map(|m| &m.source)
}

/// Cheap on-disk integrity gate: the file exists and begins with `magic`. Not a full hash (that runs
/// only on the network path), but it stops a truncated or foreign file that merely matches the
/// expected byte size from being adopted as a model and handed to llama.cpp / whisper.
fn has_magic(path: &Path, magic: [u8; 4]) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut found = [0u8; 4];
    f.read_exact(&mut found).is_ok() && found == magic
}

fn is_gguf(path: &Path) -> bool {
    has_magic(path, GGUF_MAGIC)
}

/// Whether `path` is a loadable whisper refine model (present, with the GGML magic) — the first-run
/// setup's readiness check for the refine step.
pub(crate) fn is_ggml(path: &Path) -> bool {
    has_magic(path, GGML_MAGIC)
}

/// Manages the catalog + the single active download and its progress. Held in `AppState` behind an
/// `Arc`; the progress state is a `Mutex` the background download task updates as bytes arrive.
pub struct DownloadManager {
    models_dir: PathBuf,
    state: Arc<Mutex<DownloadState>>,
}

/// Why a download could not be started.
pub enum StartError {
    UnknownModel,
    Busy,
}

impl DownloadManager {
    pub fn new(models_dir: PathBuf) -> Self {
        DownloadManager {
            models_dir,
            state: Arc::new(Mutex::new(idle_state())),
        }
    }

    /// The current download snapshot.
    pub fn status(&self) -> DownloadState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The catalog annotated with which models already resolve on disk, plus the models dir.
    pub fn catalog(&self) -> ModelCatalog {
        let items = CATALOG
            .iter()
            .map(|m| CatalogEntry {
                id: m.id.to_string(),
                name: m.name.to_string(),
                size_bytes: m.source.size_bytes,
                license: m.license.to_string(),
                context: m.context.to_string(),
                note: m.note.to_string(),
                recommended: m.recommended,
                installed: is_gguf(&self.models_dir.join(m.source.file)),
            })
            .collect();
        ModelCatalog {
            items,
            models_dir: self.models_dir.to_string_lossy().to_string(),
        }
    }

    /// Start downloading `id` in the background (single-at-a-time). Idempotent: an already-present,
    /// correct-size file skips the network and just re-points the notes model at it. Returns the
    /// current snapshot; errors if the id is unknown or a download is already running.
    pub fn start(&self, id: &str, pool: SqlitePool) -> Result<DownloadState, StartError> {
        let model = find(id).ok_or(StartError::UnknownModel)?;
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(
                st.status,
                DownloadStatus::Downloading | DownloadStatus::Verifying
            ) {
                return Err(StartError::Busy);
            }
            *st = DownloadState {
                status: DownloadStatus::Downloading,
                model_id: Some(model.id.to_string()),
                downloaded_bytes: 0,
                total_bytes: model.source.size_bytes,
                message: None,
            };
        }

        let dest = self.models_dir.join(model.source.file);
        let state = self.state.clone();

        // Already downloaded (size matches + GGUF magic) -> no network, just (re)point the
        // preference. The magic check keeps a same-size non-model file from being adopted.
        if dest.metadata().map(|m| m.len() as i64).unwrap_or(-1) == model.source.size_bytes
            && is_gguf(&dest)
        {
            let path = dest.to_string_lossy().to_string();
            tokio::spawn(async move {
                let _ = hearsay_db::queries::set_notes_model(&pool, &path).await;
                let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                st.status = DownloadStatus::Ready;
                st.downloaded_bytes = st.total_bytes;
                st.message = Some(path);
            });
            return Ok(self.status());
        }

        let dir = self.models_dir.clone();
        let source = &model.source;

        tokio::spawn(async move {
            let dl_state = state.clone();
            let dl = tokio::task::spawn_blocking(move || {
                download_source(source, &dir, &move |status, downloaded, total| {
                    let mut st = dl_state.lock().unwrap_or_else(|e| e.into_inner());
                    st.status = status;
                    st.downloaded_bytes = downloaded as i64;
                    st.total_bytes = total as i64;
                })
            })
            .await;
            match dl.unwrap_or_else(|e| Err(format!("download task panicked: {e}"))) {
                Ok(path) => {
                    let _ = hearsay_db::queries::set_notes_model(&pool, &path).await;
                    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                    st.status = DownloadStatus::Ready;
                    st.downloaded_bytes = st.total_bytes;
                    st.message = Some(path);
                }
                Err(reason) => {
                    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                    st.status = DownloadStatus::Error;
                    st.message = Some(reason);
                }
            }
        });
        Ok(self.status())
    }
}

fn idle_state() -> DownloadState {
    DownloadState {
        status: DownloadStatus::Idle,
        model_id: None,
        downloaded_bytes: 0,
        total_bytes: 0,
        message: None,
    }
}

/// Blocking download of `source` into `dir` via a `.part` file (resuming from any bytes already
/// there), streaming SHA256 as it goes, then size- + hash-verifying and atomically renaming into
/// place. `progress` is called with `(status, downloaded, total)` as bytes arrive — the notes
/// catalog maps that onto its download snapshot, first-run setup onto its step. Returns the final
/// path string on success. Called via `spawn_blocking`.
pub(crate) fn download_source(
    source: &Source,
    dir: &Path,
    progress: &(dyn Fn(DownloadStatus, u64, u64) + Sync),
) -> Result<String, String> {
    let url = source.url();
    let dest = dir.join(source.file);
    let mut part = dest.clone().into_os_string();
    part.push(".part");
    let part = PathBuf::from(part);
    let (part, dest) = (part.as_path(), dest.as_path());
    let expected_sha = source.sha256;
    let expected_size = source.size_bytes as u64;

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create models dir: {e}"))?;
    }

    let mut hasher = Sha256::new();
    let existing = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    // Report the bytes already on disk before the request + re-hash, so a resumed multi-GB download
    // does not show an empty bar for the seconds those take.
    progress(DownloadStatus::Downloading, existing, expected_size);

    // ureq's `timeout_recv_body` is a whole-body deadline, not an idle timeout: a flat 120 s failed
    // every multi-GB catalog download on normal broadband. Budget it from the model size at a ~1
    // Mbit/s floor (10-min minimum) so legitimate slow links complete; a truly-stuck transfer still
    // trips it, and the next start resumes from the `.part`.
    let body_deadline = Duration::from_secs((expected_size / 125_000).max(600));
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(body_deadline))
        .build()
        .new_agent();
    let mut req = agent.get(&url);
    if existing > 0 {
        req = req.header("Range", format!("bytes={existing}-").as_str());
    }
    let mut resp = req.call().map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status().as_u16();
    if status != 200 && status != 206 {
        return Err(format!("model host returned HTTP {status}"));
    }
    // Resume only when the server honored the Range (206); a 200 means it is sending the whole file.
    let resuming = existing > 0 && status == 206;

    // Re-hash the already-downloaded bytes so the streaming digest covers the whole file.
    if resuming {
        let mut f = std::fs::File::open(part).map_err(|e| format!("open partial: {e}"))?;
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = f.read(&mut buf).map_err(|e| format!("read partial: {e}"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resuming)
        .truncate(!resuming)
        .open(part)
        .map_err(|e| format!("open partial for write: {e}"))?;

    let mut downloaded = if resuming { existing } else { 0 };
    progress(DownloadStatus::Downloading, downloaded, expected_size);

    let mut reader = resp.body_mut().as_reader();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("read body: {e}"))?;
        if n == 0 {
            break;
        }
        downloaded += n as u64;
        // Hard stop: never write past the known model size, so a misbehaving host cannot fill the
        // disk (the post-loop size check alone would run only after the whole body was written).
        if downloaded > expected_size {
            let _ = std::fs::remove_file(part);
            return Err(format!(
                "model host sent more than the expected {expected_size} bytes; discarded"
            ));
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("write partial: {e}"))?;
        hasher.update(&buf[..n]);
        progress(DownloadStatus::Downloading, downloaded, expected_size);
    }
    file.flush().map_err(|e| format!("flush partial: {e}"))?;
    drop(file);

    if downloaded != expected_size {
        return Err(format!(
            "size mismatch: got {downloaded} bytes, expected {expected_size}"
        ));
    }

    progress(DownloadStatus::Verifying, downloaded, expected_size);
    let got = hex_lower(&hasher.finalize());
    if got != expected_sha {
        let _ = std::fs::remove_file(part);
        return Err("integrity check failed (SHA256 mismatch); the download was discarded".into());
    }

    std::fs::rename(part, dest).map_err(|e| format!("finalize model file: {e}"))?;
    Ok(dest.to_string_lossy().to_string())
}

/// Lowercase hex of a byte slice (for comparing the streamed digest to the catalog SHA256).
fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_unique_and_have_valid_sha256() {
        let mut seen = std::collections::HashSet::new();
        for m in CATALOG {
            assert!(seen.insert(m.id), "duplicate catalog id {}", m.id);
            assert_eq!(
                m.source.sha256.len(),
                64,
                "{} sha256 must be 64 hex chars",
                m.id
            );
            assert!(m.source.sha256.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(m.source.size_bytes > 0);
            assert!(m.source.file.ends_with(".gguf"));
        }
        assert_eq!(CATALOG.iter().filter(|m| m.recommended).count(), 1);
    }

    #[test]
    fn refine_sources_are_valid_and_match_on_file_name() {
        for s in REFINE_SOURCES {
            assert_eq!(s.sha256.len(), 64, "{} sha256 must be 64 hex chars", s.file);
            assert!(s.sha256.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(s.size_bytes > 0);
            assert!(s.file.ends_with(".bin"));
        }
        // The configured path is matched by file name, wherever it lives.
        let found = refine_source(Path::new("/opt/models/ggml-large-v3-turbo.bin"))
            .expect("the macOS default is in the catalog");
        assert_eq!(found.file, "ggml-large-v3-turbo.bin");
        // An unknown model is skipped rather than replaced with one of ours.
        assert!(refine_source(Path::new("/opt/models/ggml-tiny.bin")).is_none());
        assert!(refine_source(Path::new("")).is_none());
    }

    #[test]
    fn hex_lower_pads_and_lowercases() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }

    #[test]
    fn catalog_annotates_installed_and_lists_models_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = DownloadManager::new(tmp.path().to_path_buf());
        let cat = mgr.catalog();
        assert_eq!(cat.items.len(), CATALOG.len());
        assert!(cat.items.iter().all(|e| !e.installed));
        // A same-name file that is not a GGUF is not adopted (the magic integrity gate).
        std::fs::write(tmp.path().join(CATALOG[0].source.file), b"not a gguf").unwrap();
        assert!(!mgr.catalog().items[0].installed);
        // A file with the GGUF magic reports installed.
        std::fs::write(tmp.path().join(CATALOG[0].source.file), b"GGUF\0\0\0\0").unwrap();
        assert!(mgr.catalog().items[0].installed);
    }
}

//! Typed application settings, resolved from the environment with loopback-safe defaults.
//!
//! Kept small and stdlib-only (no config crate): every field has a default and an environment
//! override.

use std::env;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use hearsay_backends::LoopbackMode;

/// Resolved settings for one process.
#[derive(Debug, Clone)]
pub struct Settings {
    /// SQLx database URL (e.g. `sqlite://.../hearsay.db`). Portable to PostgreSQL later.
    pub database_url: String,
    /// Root of the per-meeting output folders (audio, transcript, notes).
    pub output_dir: PathBuf,
    /// Built web UI directory (`web/dist`); served when it contains `index.html`.
    pub web_dir: PathBuf,
    /// Bind host (loopback only in production).
    pub server_host: String,
    /// Bind port; `0` asks the OS for a free port.
    pub server_port: u16,
    /// Deployment environment (`development` | `staging` | `production`).
    pub environment: String,
    /// Path to the Swift `hearsay-helper` capture binary (the `hearsay-live` / `hearsay-me` /
    /// `hearsay-diarize` sidecars are resolved as siblings). Used by the macOS live-capture backend.
    pub helper_path: PathBuf,
    /// GGML whisper model for the offline refine (re-transcribing diarized turns at re-diarization).
    pub refine_model: PathBuf,
    /// Deadline for the `hearsay-diarize` refine subprocess (`HEARSAY_REFINE_TIMEOUT_SECS`, default
    /// 1800s). Generous — a legitimate refine is minutes-long — but bounded so a hung sidecar can
    /// never wedge meeting stop.
    pub refine_timeout: Duration,
    /// Auto-run the offline refine when a meeting stops (`HEARSAY_AUTO_REFINE`, default off — the
    /// refine contends with the next meeting's sidecars on the ANE, so it is opt-in and users drive
    /// it from the "Refine speakers" button when they have time). The manual `/rediarize` route works
    /// regardless.
    pub auto_refine: bool,
    /// Default audio-retention switch: keep one WAV per meeting (`HEARSAY_RECORD`, default on). The
    /// editable `recording` settings section overrides this per install.
    pub record: bool,
    /// Default cosine threshold at/above which a refined speaker is auto-matched to a person named
    /// in a prior meeting (`HEARSAY_RECOGNITION_THRESHOLD`, default 0.6; the `speakers` section
    /// overrides it).
    pub recognition_threshold: f64,
    /// Default switch for the inactivity "still recording?" prompt (`HEARSAY_INACTIVITY_PROMPT`,
    /// default on). The `recording` section overrides it per install.
    pub inactivity_prompt: bool,
    /// Default switch for auto-ending a meeting after prolonged silence
    /// (`HEARSAY_INACTIVITY_AUTO_END`, default on). Independent of the prompt; the `recording` section
    /// overrides it per install.
    pub inactivity_auto_end: bool,
    /// Default minutes of continuous silence (no Me/Them speech) before the in-app "still recording?"
    /// prompt (`HEARSAY_INACTIVITY_PROMPT_MINUTES`, default 5; the `recording` section overrides it).
    pub inactivity_prompt_minutes: u64,
    /// Default minutes of continuous silence before the meeting auto-ends with a logged transcript
    /// marker (`HEARSAY_INACTIVITY_END_MINUTES`, default 10; must exceed the prompt threshold when both
    /// are enabled; the `recording` section overrides it).
    pub inactivity_end_minutes: u64,
    /// Default for the optional local-LLM notes step: generate meeting notes at stop
    /// (`HEARSAY_NOTES`, default off — opt-in, and needs a downloaded model). The `models` settings
    /// section overrides it per install.
    pub notes_enabled: bool,
    /// Default GGUF model for the notes step (`HEARSAY_NOTES_MODEL`, default empty — unset until the
    /// user downloads or points at one). The `models` section overrides it; read fresh at each
    /// stop/generate so a Models-panel change or a completed download applies with no restart.
    pub notes_model: PathBuf,
    /// Default prompt template for the notes step (`HEARSAY_NOTES_PROMPT`, default the built-in
    /// [`hearsay_backends::DEFAULT_NOTES_PROMPT`]). The template's `{transcript}` placeholder is
    /// filled with the finalized transcript. The `models` section overrides it per install; read
    /// fresh at each generate so a Models-panel edit applies with no restart.
    pub notes_prompt: String,
    /// Path to the `hearsay-notes` sidecar that runs the local-LLM notes step out-of-process
    /// (`HEARSAY_NOTES_PATH`, default a `hearsay-notes` sibling of this executable). Out-of-process so
    /// llama.cpp never links into the core alongside whisper (a `ggml` collision that slows the
    /// refine ~5x).
    pub notes_binary: PathBuf,
    /// Root the download manager writes models into and references them from
    /// (`HEARSAY_MODELS_DIR`, default `outputs/models`; the desktop shell points it at a persistent
    /// app-data dir so downloaded models survive reinstall).
    pub models_dir: PathBuf,
    /// Path to the desktop shell's handshake file (`HEARSAY_HANDSHAKE_PATH`): the private 0600 file
    /// the shell reads once for `{port, token}`. `None` in headless dev, where no handshake is written.
    pub handshake_path: Option<PathBuf>,
    /// Bundled FluidAudio live-models directory (`HEARSAY_FLUID_MODELS_DIR`, set by the desktop shell):
    /// seeded into FluidAudio's cache on first launch. `None` in headless dev, where FluidAudio downloads.
    pub fluid_models_dir: Option<PathBuf>,
    /// The process's home directory (`HOME`): the base of FluidAudio's default model cache when
    /// seeding the bundled models. `None` when `HOME` is unset.
    pub home_dir: Option<PathBuf>,
    /// Directory holding the sherpa live/diarize models for the Windows backend
    /// (`HEARSAY_SHERPA_MODELS_DIR`, default `outputs/models/sherpa`; the desktop shell points it at
    /// the bundled copy). Unused on macOS.
    pub sherpa_models_dir: PathBuf,
    /// Which WASAPI loopback path captures the Them stream on Windows (`HEARSAY_WIN_LOOPBACK`,
    /// `device` | `process`, default `device` — see `docs/windows-port.md`). Unused on macOS.
    pub win_loopback_mode: LoopbackMode,
}

fn env_or(key: &str, default: impl Into<String>) -> String {
    env::var(key).unwrap_or_else(|_| default.into())
}

/// An optional path env var (`None` when unset). For the paths the desktop shell injects (the
/// handshake file, the bundled FluidAudio models) and the process's `HOME`.
fn env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key).map(PathBuf::from)
}

/// The `hearsay-notes` sidecar path: `HEARSAY_NOTES_PATH` if set, else a `hearsay-notes` sibling of
/// this executable (where the bundler stages it, and where `cargo`'s target dir puts it in dev). A
/// bare `hearsay-notes` is the last resort when the exe path is unreadable (relies on `PATH`).
fn default_notes_binary() -> PathBuf {
    if let Some(path) = env::var_os("HEARSAY_NOTES_PATH") {
        return PathBuf::from(path);
    }
    env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("hearsay-notes")))
        .unwrap_or_else(|| PathBuf::from("hearsay-notes"))
}

/// Parse a boolean env var (`1`/`true`/`yes`/`on` -> true, `0`/`false`/`no`/`off` -> false,
/// case-insensitive); `default` when unset. A set-but-unrecognized value (e.g. the typo `ture`) is
/// recorded in `problems` so [`Settings::from_env`] surfaces it instead of silently mapping to false.
fn env_bool(key: &str, default: bool, problems: &mut Vec<String>) -> bool {
    let Ok(raw) = env::var(key) else {
        return default;
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => {
            problems.push(format!(
                "{key}={raw:?} is not a boolean (expected one of 1/true/yes/on or 0/false/no/off)"
            ));
            default
        }
    }
}

/// Parse an unsigned-integer env var; `default` when unset. A set-but-unparseable value is recorded
/// in `problems` instead of falling back silently.
fn env_u64(key: &str, default: u64, problems: &mut Vec<String>) -> u64 {
    let Ok(raw) = env::var(key) else {
        return default;
    };
    match raw.trim().parse::<u64>() {
        Ok(value) => value,
        Err(_) => {
            problems.push(format!("{key}={raw:?} is not a non-negative integer"));
            default
        }
    }
}

/// Parse the Windows loopback-mode env var (`HEARSAY_WIN_LOOPBACK`); `default` when unset. A
/// set-but-unrecognized value is recorded in `problems` instead of falling back silently.
fn env_loopback_mode(default: LoopbackMode, problems: &mut Vec<String>) -> LoopbackMode {
    const KEY: &str = "HEARSAY_WIN_LOOPBACK";
    let Ok(raw) = env::var(KEY) else {
        return default;
    };
    match raw.parse::<LoopbackMode>() {
        Ok(mode) => mode,
        Err(err) => {
            problems.push(format!("{KEY}: {err}"));
            default
        }
    }
}

/// Parse the recognition-threshold env var and range-check it to the same `0.0..=1.0` the settings
/// API enforces (`routes::settings::update_speakers`); `default` when unset. A set-but-unparseable or
/// out-of-range value is recorded in `problems` instead of falling back silently.
fn env_recognition_threshold(default: f64, problems: &mut Vec<String>) -> f64 {
    const KEY: &str = "HEARSAY_RECOGNITION_THRESHOLD";
    let Ok(raw) = env::var(KEY) else {
        return default;
    };
    match raw.trim().parse::<f64>() {
        Ok(value) if (0.0..=1.0).contains(&value) => value,
        Ok(value) => {
            problems.push(format!(
                "{KEY}={value} is out of range (recognition_threshold must be between 0.0 and 1.0)"
            ));
            default
        }
        Err(_) => {
            problems.push(format!(
                "{KEY}={raw:?} is not a number (recognition_threshold must be between 0.0 and 1.0)"
            ));
            default
        }
    }
}

/// Whether `host` is a safe loopback bind target: a loopback IP literal (`127.0.0.1`, `::1`) or the
/// name `localhost`. A hostname other than `localhost` (which could resolve anywhere) is not loopback.
fn is_loopback_bind(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Refuse a non-loopback bind host outside development (see [`Settings::ensure_bind_allowed`]).
fn bind_allowed(host: &str, environment: &str) -> Result<(), String> {
    if environment == "development" || is_loopback_bind(host) {
        Ok(())
    } else {
        Err(format!(
            "refusing to bind non-loopback host {host:?} with ENVIRONMENT={environment:?} \
             (loopback only outside development; set ENVIRONMENT=development to override)"
        ))
    }
}

impl Settings {
    /// Resolve settings from environment variables, falling back to loopback-safe defaults.
    ///
    /// A malformed override (a boolean typo, an unparseable number, an out-of-range threshold) is a
    /// hard error outside development so a misconfigured deploy fails at startup rather than silently
    /// running with the default; in development the same problems are logged as warnings and the
    /// default is used, keeping local runs convenient.
    pub fn from_env() -> Result<Self, String> {
        let environment = env_or("ENVIRONMENT", "development");
        let mut problems: Vec<String> = Vec::new();

        let output_dir = PathBuf::from(env_or("HEARSAY_OUTPUT_DIR", "./outputs/recordings"));
        let database_url = env::var("DATABASE_URL")
            .unwrap_or_else(|_| "sqlite://./outputs/db/hearsay.db".to_string());
        let server_port = env::var("HEARSAY_SERVER_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        let auto_refine = env_bool("HEARSAY_AUTO_REFINE", false, &mut problems);
        let record = env_bool("HEARSAY_RECORD", true, &mut problems);
        let recognition_threshold = env_recognition_threshold(0.6, &mut problems);
        let inactivity_prompt = env_bool("HEARSAY_INACTIVITY_PROMPT", true, &mut problems);
        let inactivity_auto_end = env_bool("HEARSAY_INACTIVITY_AUTO_END", true, &mut problems);
        let inactivity_prompt_minutes =
            env_u64("HEARSAY_INACTIVITY_PROMPT_MINUTES", 5, &mut problems);
        let inactivity_end_minutes = env_u64("HEARSAY_INACTIVITY_END_MINUTES", 10, &mut problems);
        let notes_enabled = env_bool("HEARSAY_NOTES", false, &mut problems);
        let refine_timeout =
            Duration::from_secs(env_u64("HEARSAY_REFINE_TIMEOUT_SECS", 1800, &mut problems));
        let win_loopback_mode = env_loopback_mode(LoopbackMode::Device, &mut problems);

        if !problems.is_empty() {
            if environment == "development" {
                for problem in &problems {
                    tracing::warn!("{problem}; using default");
                }
            } else {
                return Err(format!(
                    "refusing to start with {} invalid configuration override(s) \
                     (ENVIRONMENT={environment:?}): {}",
                    problems.len(),
                    problems.join("; ")
                ));
            }
        }

        Ok(Settings {
            database_url,
            output_dir,
            web_dir: PathBuf::from(env_or("HEARSAY_WEB_DIR", "./web/dist")),
            server_host: env_or("HEARSAY_SERVER_HOST", "127.0.0.1"),
            server_port,
            environment,
            helper_path: PathBuf::from(env_or(
                "HEARSAY_HELPER_PATH",
                "helper/.build/arm64-apple-macosx/debug/hearsay-helper",
            )),
            refine_model: PathBuf::from(env_or(
                "HEARSAY_REFINE_MODEL",
                "outputs/models/ggml-large-v3-turbo.bin",
            )),
            refine_timeout,
            auto_refine,
            record,
            recognition_threshold,
            inactivity_prompt,
            inactivity_auto_end,
            inactivity_prompt_minutes,
            inactivity_end_minutes,
            notes_enabled,
            notes_model: PathBuf::from(env_or("HEARSAY_NOTES_MODEL", "")),
            notes_prompt: env_or(
                "HEARSAY_NOTES_PROMPT",
                hearsay_backends::DEFAULT_NOTES_PROMPT,
            ),
            notes_binary: default_notes_binary(),
            models_dir: PathBuf::from(env_or("HEARSAY_MODELS_DIR", "outputs/models")),
            handshake_path: env_path("HEARSAY_HANDSHAKE_PATH"),
            fluid_models_dir: env_path("HEARSAY_FLUID_MODELS_DIR"),
            home_dir: env_path("HOME"),
            sherpa_models_dir: PathBuf::from(env_or(
                "HEARSAY_SHERPA_MODELS_DIR",
                "outputs/models/sherpa",
            )),
            win_loopback_mode,
        })
    }

    /// Refuse a non-loopback bind host outside development. Loopback is not a security boundary, but
    /// a public bind (`0.0.0.0`, a LAN IP) exposes the token-gated API to the whole network — combined
    /// with an env var flip that is remote compromise. Overridable only with `ENVIRONMENT=development`.
    pub fn ensure_bind_allowed(&self) -> Result<(), String> {
        bind_allowed(&self.server_host, &self.environment)
    }
}

#[cfg(test)]
mod tests {
    use super::{bind_allowed, is_loopback_bind};

    #[test]
    fn loopback_bind_accepts_loopback_ips_and_localhost_only() {
        assert!(is_loopback_bind("127.0.0.1"));
        assert!(is_loopback_bind("::1"));
        assert!(is_loopback_bind("localhost"));
        assert!(is_loopback_bind("LocalHost"));
        assert!(!is_loopback_bind("0.0.0.0"));
        assert!(!is_loopback_bind("10.0.0.1"));
        assert!(!is_loopback_bind("::"));
        assert!(!is_loopback_bind("example.com"));
        assert!(!is_loopback_bind(""));
    }

    #[test]
    fn bind_allowed_refuses_public_bind_outside_development() {
        // Loopback is always fine.
        assert!(bind_allowed("127.0.0.1", "production").is_ok());
        assert!(bind_allowed("localhost", "staging").is_ok());
        // Development may bind anywhere (the explicit override).
        assert!(bind_allowed("0.0.0.0", "development").is_ok());
        // A public bind outside development is refused.
        assert!(bind_allowed("0.0.0.0", "production").is_err());
        assert!(bind_allowed("10.0.0.1", "staging").is_err());
    }
}

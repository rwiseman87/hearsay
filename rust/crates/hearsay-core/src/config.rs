//! Typed application settings, resolved from the environment with loopback-safe defaults.
//!
//! Kept small and stdlib-only (no config crate): every field has a default and an environment
//! override.

use std::env;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Resolved settings for one process.
#[derive(Debug, Clone)]
pub struct Settings {
    /// SQLx database URL.
    pub database_url: String,
    /// Root of the per-meeting output folders (audio, transcript, notes).
    pub output_dir: PathBuf,
    /// Built web UI directory; served when it contains `index.html`.
    pub web_dir: PathBuf,
    /// Bind host (loopback only outside development).
    pub server_host: String,
    /// Bind port; `0` asks the OS for a free port.
    pub server_port: u16,
    /// Deployment environment (`development` | `staging` | `production`).
    pub environment: String,
    /// Path to the Swift `hearsay-helper` capture binary; the sidecars resolve as siblings.
    pub helper_path: PathBuf,
    /// Dev-only (`HEARSAY_SCRIPTED`): swap the platform backend for the model-free scripted engine.
    /// It spawns no sidecars, so first-run setup is skipped with it.
    pub scripted: bool,
    /// Deadline for the `hearsay-diarize` refine subprocess, so a hung sidecar cannot wedge stop.
    pub refine_timeout: Duration,
    /// Run the offline refine at stop. Off by default: it contends with the next meeting's
    /// sidecars on the ANE. The manual `/rediarize` route works regardless.
    pub auto_refine: bool,
    /// Keep one WAV per meeting.
    pub record: bool,
    /// Cosine threshold at/above which a refined speaker is matched to a person named previously.
    pub recognition_threshold: f64,
    /// Show the inactivity "still recording?" prompt.
    pub inactivity_prompt: bool,
    /// Auto-end a meeting after prolonged silence. Independent of the prompt.
    pub inactivity_auto_end: bool,
    /// Minutes of continuous silence before the "still recording?" prompt.
    pub inactivity_prompt_minutes: u64,
    /// Minutes of continuous silence before auto-end. Must exceed the prompt threshold when both
    /// are enabled.
    pub inactivity_end_minutes: u64,
    /// Archive a finalized meeting's audio as lossless FLAC.
    pub compress_audio: bool,
    /// Age in days before a finalized meeting's audio is archived.
    pub compress_after_days: u64,
    /// Generate meeting notes at stop. Off by default; needs a downloaded model.
    pub notes_enabled: bool,
    /// GGUF model for the notes step. Read fresh at each generate, so a Models-panel change or a
    /// completed download applies with no restart.
    pub notes_model: PathBuf,
    /// Prompt template for the notes step; its `{transcript}` placeholder is filled with the
    /// finalized transcript. Read fresh at each generate, like `notes_model`.
    pub notes_prompt: String,
    /// Path to the `hearsay-notes` sidecar. Out-of-process so a llama.cpp crash or stall cannot take
    /// down the core — see `docs/architecture.md`.
    pub notes_binary: PathBuf,
    /// Root the download manager writes models into and references them from.
    pub models_dir: PathBuf,
    /// The desktop shell's private 0600 `{port, token}` handshake file. `None` in headless dev.
    pub handshake_path: Option<PathBuf>,
    /// The bundled third-party notices, which Settings > About opens. Defaults to the repo copy for
    /// dev; the desktop shell points it at the bundle resource.
    pub notices_path: PathBuf,
    /// The process's home directory: the base of FluidAudio's model cache.
    pub home_dir: Option<PathBuf>,
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
        let scripted = environment == "development" && env::var_os("HEARSAY_SCRIPTED").is_some();
        let models_dir = PathBuf::from(env_or("HEARSAY_MODELS_DIR", "outputs/models"));
        let auto_refine = env_bool("HEARSAY_AUTO_REFINE", false, &mut problems);
        let record = env_bool("HEARSAY_RECORD", true, &mut problems);
        let recognition_threshold = env_recognition_threshold(0.6, &mut problems);
        let inactivity_prompt = env_bool("HEARSAY_INACTIVITY_PROMPT", true, &mut problems);
        let inactivity_auto_end = env_bool("HEARSAY_INACTIVITY_AUTO_END", true, &mut problems);
        let inactivity_prompt_minutes =
            env_u64("HEARSAY_INACTIVITY_PROMPT_MINUTES", 5, &mut problems);
        let inactivity_end_minutes = env_u64("HEARSAY_INACTIVITY_END_MINUTES", 10, &mut problems);
        let compress_audio = env_bool("HEARSAY_COMPRESS_AUDIO", true, &mut problems);
        let compress_after_days = env_u64("HEARSAY_COMPRESS_AFTER_DAYS", 7, &mut problems);
        let notes_enabled = env_bool("HEARSAY_NOTES", false, &mut problems);
        let refine_timeout =
            Duration::from_secs(env_u64("HEARSAY_REFINE_TIMEOUT_SECS", 1800, &mut problems));

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
            scripted,
            refine_timeout,
            auto_refine,
            record,
            recognition_threshold,
            inactivity_prompt,
            inactivity_auto_end,
            inactivity_prompt_minutes,
            inactivity_end_minutes,
            compress_audio,
            compress_after_days,
            notes_enabled,
            notes_model: PathBuf::from(env_or("HEARSAY_NOTES_MODEL", "")),
            notes_prompt: env_or(
                "HEARSAY_NOTES_PROMPT",
                hearsay_backends::DEFAULT_NOTES_PROMPT,
            ),
            notes_binary: default_notes_binary(),
            models_dir,
            handshake_path: env_path("HEARSAY_HANDSHAKE_PATH"),
            notices_path: PathBuf::from(env_or(
                "HEARSAY_THIRD_PARTY_NOTICES",
                "./THIRD-PARTY-NOTICES.md",
            )),
            home_dir: env_path("HOME"),
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

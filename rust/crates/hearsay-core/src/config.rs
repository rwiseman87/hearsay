//! Typed application settings, resolved from the environment with loopback-safe defaults.
//!
//! Rust counterpart of `src/hearsay/config/settings.py`. Kept small and stdlib-only (no config
//! crate): every field has a default and an environment override.

use std::env;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

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
}

fn env_or(key: &str, default: impl Into<String>) -> String {
    env::var(key).unwrap_or_else(|_| default.into())
}

/// Parse a boolean env var (`1`/`true`/`yes`/`on` -> true, case-insensitive); `default` when unset.
fn env_bool(key: &str, default: bool) -> bool {
    match env::var(key) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => default,
    }
}

/// Parse a float env var; `default` when unset or unparseable.
fn env_f64(key: &str, default: f64) -> f64 {
    env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
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
    pub fn from_env() -> Self {
        let output_dir = PathBuf::from(env_or("HEARSAY_OUTPUT_DIR", "./outputs/recordings"));
        let database_url = env::var("DATABASE_URL")
            .unwrap_or_else(|_| "sqlite://./outputs/db/hearsay.db".to_string());
        let server_port = env::var("HEARSAY_SERVER_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        Settings {
            database_url,
            output_dir,
            web_dir: PathBuf::from(env_or("HEARSAY_WEB_DIR", "./web/dist")),
            server_host: env_or("HEARSAY_SERVER_HOST", "127.0.0.1"),
            server_port,
            environment: env_or("ENVIRONMENT", "development"),
            helper_path: PathBuf::from(env_or(
                "HEARSAY_HELPER_PATH",
                "helper/.build/arm64-apple-macosx/debug/hearsay-helper",
            )),
            refine_model: PathBuf::from(env_or(
                "HEARSAY_REFINE_MODEL",
                "outputs/models/ggml-large-v3-turbo.bin",
            )),
            refine_timeout: Duration::from_secs(
                env::var("HEARSAY_REFINE_TIMEOUT_SECS")
                    .ok()
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(1800),
            ),
            auto_refine: env_bool("HEARSAY_AUTO_REFINE", false),
            record: env_bool("HEARSAY_RECORD", true),
            recognition_threshold: env_f64("HEARSAY_RECOGNITION_THRESHOLD", 0.6),
        }
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

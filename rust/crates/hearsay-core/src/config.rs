//! Typed application settings, resolved from the environment with loopback-safe defaults.
//!
//! Rust counterpart of `src/hearsay/config/settings.py`. Kept small and stdlib-only (no config
//! crate): every field has a default and an environment override.

use std::env;
use std::path::PathBuf;

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
    /// Path to the Swift `hearsay-helper` capture binary (the `hearsay-live` / `hearsay-me`
    /// sidecars are resolved as siblings). Used by the macOS live-capture backend.
    pub helper_path: PathBuf,
}

fn env_or(key: &str, default: impl Into<String>) -> String {
    env::var(key).unwrap_or_else(|_| default.into())
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
        }
    }
}

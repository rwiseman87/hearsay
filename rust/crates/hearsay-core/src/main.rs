//! Hearsay application binary.
//!
//! axum HTTP + WebSocket API bound to 127.0.0.1 with a per-session bearer token (Origin/Host
//! allowlist); wires `hearsay-db` + `hearsay-orchestrator` + `hearsay-attribution`; serves the React
//! UI bundle (or runs under the Tauri shell). OpenAPI via utoipa, structured JSON logs via tracing.
//!
//! Rust port of `src/hearsay/api/` and the application entrypoint.
//!
//! Scaffold: see `docs/architecture-cross-platform.md`.

fn main() {
    // Wiring lands once the crate layout is validated.
}

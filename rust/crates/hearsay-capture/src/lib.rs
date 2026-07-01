//! Cross-platform audio capture behind a single trait.
//!
//! - `cfg(windows)`: WASAPI loopback (Them) + microphone (Me), via cpal.
//! - `cfg(macos)`: Core Audio process tap (global-except-self) for Them + mic for Me; the existing
//!   Swift `hearsay-helper` is the fallback if cpal's macOS loopback does not preserve the Me/Them
//!   separation and the Teams per-process-silent workaround.
//!
//! Me is the mic and is never diarized; Them is system audio. Both resampled to 16 kHz mono and
//! stamped with one monotonic `host_ts`; cross-stream alignment is by timestamp, never sample index.
//! Emits PCM over `hearsay-ipc`.
//!
//! Scaffold: see `docs/architecture-cross-platform.md`.

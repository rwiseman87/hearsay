//! Local-only inference sidecar.
//!
//! Pipeline: Silero VAD (ONNX) -> pure-Rust Segmenter (partial/final hysteresis) -> whisper.cpp ASR
//! (Metal on macOS, Vulkan on the Intel Arc iGPU, CUDA, or CPU) for live; whisper-large-v3-turbo /
//! distil-large-v3 for the offline refine; offline diarization (sherpa-onnx / pyannote ONNX) at stop.
//! Model size is selected by detected hardware tier. Speaks the `hearsay-ipc` contract.
//!
//! Replaces the FluidAudio Swift sidecars on the unified cross-platform path (FluidAudio/ANE may be
//! kept as a macOS high-accuracy tier behind the same interface — see the open decision in the docs).
//!
//! Scaffold: see `docs/architecture-cross-platform.md`.

fn main() {
    // Wiring lands once the crate layout is validated.
}

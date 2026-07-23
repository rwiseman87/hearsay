# Third-Party Notices

Hearsay's own source is licensed under the PolyForm Noncommercial License 1.0.0 (see `LICENSE`). The
packaged application bundles and/or downloads third-party machine-learning models and libraries that
carry their **own** licenses; none of these weights are stored in this repository. Those licenses
govern the models and library code, not Hearsay's source, and their obligations apply when Hearsay is
distributed as an installer.

Every bundled or downloaded component is under a permissive or attribution license (MIT, Apache-2.0,
or CC-BY-4.0) — all permit redistribution and commercial use. Licenses below were verified on
2026-07-23 against each project's Hugging Face model card, NGC catalog entry, or repository LICENSE.

## macOS live models — bundled, via FluidAudio

| Component | Role | License | Source (conversion / upstream) |
|---|---|---|---|
| Parakeet TDT 0.6B v3 (Core ML) | batch ASR | **CC-BY-4.0** | `FluidInference/parakeet-tdt-0.6b-v3-coreml` / `nvidia/parakeet-tdt-0.6b-v3` |
| Parakeet unified EN 0.6B (Core ML) | streaming ASR | **CC-BY-4.0** | `FluidInference/parakeet-unified-en-0.6b-coreml` / NVIDIA Parakeet |
| LS-EEND (Core ML) | live diarizer | MIT | `FluidInference/ls-eend-coreml` |
| Speaker diarization: pyannote segmentation 3.0 + WeSpeaker v2 (Core ML) | refine diarizer + embeddings | **CC-BY-4.0** (as distributed) | `FluidInference/speaker-diarization-coreml` (pyannote upstream MIT; WeSpeaker toolkit Apache-2.0) |
| Silero VAD (Core ML) | voice activity detection | MIT | `FluidInference/silero-vad-coreml` / `snakers4/silero-vad` |

## Windows live models — bundled, via sherpa-onnx (k2-fsa model zoo)

| Component | Role | License | Source (conversion / upstream) |
|---|---|---|---|
| Streaming Zipformer EN 2023-06-21 | streaming ASR | Apache-2.0 | `csukuangfj/sherpa-onnx-streaming-zipformer-en-2023-06-21` |
| pyannote segmentation 3.0 | diarization segmentation | MIT | `csukuangfj/sherpa-onnx-pyannote-segmentation-3-0` (license from upstream `pyannote/segmentation-3.0`; conversion repo states none) |
| Online punctuation EN 2024-08-06 | punctuation | Apache-2.0 | k2-fsa sherpa punctuation zoo (license from upstream Edge-Punct-Casing; conversion repo states none) |
| TitaNet-small (ONNX) | speaker embeddings | Apache-2.0 (NVIDIA NeMo Toolkit license) | `nemo_en_titanet_small.onnx` |

## Offline refine — bundled

| Component | License | Source |
|---|---|---|
| Whisper ggml models (large-v3-turbo, small.en) | MIT | `ggerganov/whisper.cpp` / upstream OpenAI Whisper |

## Optional notes LLMs — user-downloaded on selection, not bundled

| Component | License | Source (quantization / upstream) |
|---|---|---|
| Qwen3-1.7B, Qwen3-4B-Instruct-2507 (GGUF) | Apache-2.0 | `unsloth/*-GGUF` / `Qwen/*` |
| SmolLM3-3B (GGUF) | Apache-2.0 | `unsloth/SmolLM3-3B-GGUF` / `HuggingFaceTB/SmolLM3-3B` |

## Libraries — bundled or linked

| Library | License |
|---|---|
| FluidAudio | Apache-2.0 |
| sherpa-onnx | Apache-2.0 |
| onnxruntime | MIT |
| whisper.cpp / ggml | MIT |
| llama.cpp | MIT |
| Rust crate dependencies | permissive only (MIT / Apache-2.0 / BSD / ISC / etc.), enforced in CI by `cargo deny` (`rust/deny.toml`) |

## Attribution requirements

The CC-BY-4.0 components require crediting the original author and indicating that the weights were
format-converted. Surface these in an in-app "Licenses" / "About" screen or a notices file bundled in
the installer:

- "Parakeet TDT 0.6B v3" and "Parakeet unified EN 0.6B" (c) NVIDIA — CC-BY-4.0; converted to Core ML.
- "pyannote speaker diarization / segmentation 3.0" (c) Herve Bredin et al., with "WeSpeaker" speaker
  embeddings — distributed under CC-BY-4.0 by FluidInference; converted to Core ML.

All MIT and Apache-2.0 components require preserving their license text and copyright notices in the
distributed application.

Packaging note (not a licensing restriction): the upstream `pyannote/segmentation-3.0` repository is
access-gated on Hugging Face — downloading it directly requires an authenticated token that has
accepted its conditions. Hearsay ships the already-converted weights, so end users are never gated;
this affects only rebuilding the model set from upstream.

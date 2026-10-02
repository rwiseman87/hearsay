# Third-Party Notices

Hearsay incorporates the third-party components listed here. Each remains under its own license,
reproduced or referenced below. Hearsay's own source is licensed separately (see `LICENSE`).

---

## Creative Commons Attribution 4.0 International (CC BY 4.0)

Licensed under CC BY 4.0: https://creativecommons.org/licenses/by/4.0/

Each of these components was format-converted for use in Hearsay. No other changes were made.

- **Parakeet Ultra** — © moondream, based on NVIDIA Parakeet TDT 0.6B v3 (© NVIDIA Corporation).
  Converted to Core ML by FluidInference (`FluidInference/parakeet-ultra-coreml`).
- **Parakeet unified EN 0.6B** — © NVIDIA Corporation. Converted to Core ML by FluidInference
  (`FluidInference/parakeet-unified-en-0.6b-coreml`).
- **pyannote segmentation 3.0 with WeSpeaker v2 speaker embeddings** — © Hervé Bredin and
  contributors (pyannote); WeSpeaker authors (embeddings). Distributed under CC BY 4.0 by
  FluidInference and converted to Core ML (`FluidInference/speaker-diarization-coreml`).

THE WORKS ARE PROVIDED "AS-IS" AND WITHOUT WARRANTIES OF ANY KIND, TO THE EXTENT PERMITTED BY THE
CC BY 4.0 PUBLIC LICENSE.

---

## MIT License

The following components are licensed under the MIT License:

- **LS-EEND** (Core ML conversion `FluidInference/ls-eend-coreml`)
- **Silero VAD** — © Silero Team (`snakers4/silero-vad`; Core ML conversion
  `FluidInference/silero-vad-coreml`)
- **llama.cpp** — © Georgi Gerganov and contributors
- **Tauri** — © 2017 - Present Tauri Apps Contributors

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
associated documentation files (the "Software"), to deal in the Software without restriction,
including without limitation the rights to use, copy, modify, merge, publish, distribute,
sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or
substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT
NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT
OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

---

## Apache License 2.0

The following components are licensed under the Apache License, Version 2.0. You may obtain a copy
of the License at http://www.apache.org/licenses/LICENSE-2.0

- **FluidAudio**
- **Qwen3-1.7B, Qwen3-4B-Instruct-2507** — © Alibaba Cloud (GGUF quantizations by Unsloth).
  Downloaded on user request; not bundled.
- **SmolLM3-3B** — © Hugging Face (GGUF quantization by Unsloth). Downloaded on user request; not
  bundled.
- **Tauri** (dual-licensed MIT / Apache-2.0)

Unless required by applicable law or agreed to in writing, software distributed under the License
is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
implied. See the License for the specific language governing permissions and limitations under the
License.

Model weights converted to Core ML for use in Hearsay are modified from their original
distributed form; no other changes were made.

---

## Rust and JavaScript dependencies

Hearsay links a tree of Rust crates and bundles a compiled JavaScript frontend. Every dependency is
under a permissive license (MIT, Apache-2.0, BSD, ISC, Unicode-3.0, Zlib, or MPL-2.0), enforced at
build time by `cargo deny` against the policy in `rust/deny.toml`. The MIT and Apache-2.0 terms
above apply to those dependencies carrying those licenses; per-crate copyright notices are held in
each crate's own repository.

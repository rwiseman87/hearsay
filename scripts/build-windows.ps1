# Build the Hearsay Windows installer (NSIS) on a Windows x86_64 machine.
# The Windows counterpart of the Makefile's stage-release + dmg path (Windows has no make).
# Prerequisites: docs/development.md "Windows" section. Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1 [-NoVulkan] [-Aec] [-SkipModels]
#
#   -NoVulkan    drop the vulkan feature (CPU-only whisper/llama; no Vulkan SDK needed to build)
#   -NoAec       drop the aec echo-cancellation feature
#   -SkipModels  skip model download/staging (reuse what is already staged)
#
# Vulkan is on by default: it covers the ggml half of the stack (the whisper refine and the notes
# LLM) on AMD, NVIDIA, and Intel alike, which is what one installer shipping to unknown hardware
# needs — CUDA would bind the build to one vendor. It does NOT accelerate the live sherpa ASR:
# onnxruntime has no Vulkan execution provider, so that path stays on the CPU regardless. Building
# it needs the Vulkan SDK and Windows long paths (checked below, with the fix in the message).

param(
    [switch]$NoVulkan,
    [switch]$NoAec,
    [switch]$SkipModels
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

# Require-Tool / Initialize-LibClang / Initialize-VulkanBuild, shared with scripts\test-windows.ps1.
. "$PSScriptRoot\windows-build-env.ps1"

Require-Tool cargo "Install Rust (MSVC toolchain) from https://rustup.rs"
Require-Tool npm "Install Node 22 from https://nodejs.org"
Require-Tool cmake "Install CMake (whisper-rs / llama-cpp-2 build) from https://cmake.org"
Require-Tool tar "Windows 10 1803+ ships bsdtar; update Windows"
if (-not (cargo tauri --version 2>$null)) {
    throw "tauri-cli not found. Run: cargo install tauri-cli --locked"
}

Initialize-LibClang
# Never inherit the build host's instruction set -- an AVX-512 host would ship a core that dies with
# STATUS_ILLEGAL_INSTRUCTION on any Intel machine. Unconditional: -NoVulkan is affected identically.
Initialize-GgmlIsaFloor

# The vulkan feature compiles ggml's Vulkan backend, which needs the SDK's headers + glslc at build
# time plus Windows long-path support and the Ninja generator. Preflight it here (with the fixes in
# the message) rather than failing deep inside ggml's cmake. `$coreTarget` is the short target dir
# the shader sub-build needs; it is applied to the core build only (below), never exported, since
# the Tauri build must keep writing to web\src-tauri\target where the bundle is collected from.
if (-not $NoVulkan) {
    $coreTarget = Initialize-VulkanBuild
}
# Where cargo writes the core binaries (the Vulkan path redirects it; see above).
$target = if ($coreTarget) { "$coreTarget\release" } else { "rust\target\release" }

# --- Models -------------------------------------------------------------------------------------
# Same artifacts and layout as `make fetch-sherpa-models` / `stage-sherpa-models` / `stage-model`.
$sherpaSrc = "outputs\models\sherpa"
$sherpaDst = "web\src-tauri\models\sherpa"
$sherpaRelease = "https://github.com/k2-fsa/sherpa-onnx/releases/download"
# The 70M LibriSpeech+GigaSpeech zipformer (see hearsay-backends/src/windows.rs STREAMING_DIR).
$streaming = "sherpa-onnx-streaming-zipformer-en-2023-06-21"
$archives = @(
    @{ Name = $streaming; Tag = "asr-models" },
    @{ Name = "sherpa-onnx-pyannote-segmentation-3-0"; Tag = "speaker-segmentation-models" },
    # Restores case + punctuation on the streaming zipformer's bare uppercase output, which macOS
    # gets natively from Parakeet.
    @{ Name = "sherpa-onnx-online-punct-en-2024-08-06"; Tag = "punctuation-models" }
)
$embedding = "nemo_en_titanet_small.onnx"
# "recongition" is the real upstream release-tag spelling.
$embeddingUrl = "$sherpaRelease/speaker-recongition-models/$embedding"
$refineModel = "ggml-small.en.bin"
$refineUrl = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/$refineModel"

if (-not $SkipModels) {
    New-Item -ItemType Directory -Force -Path $sherpaSrc | Out-Null
    foreach ($a in $archives) {
        if (Test-Path "$sherpaSrc\$($a.Name)") {
            Write-Host "sherpa model $($a.Name) already fetched"
        } else {
            Write-Host "fetching sherpa model $($a.Name)..."
            $tarball = "$env:TEMP\$($a.Name).tar.bz2"
            Invoke-WebRequest -Uri "$sherpaRelease/$($a.Tag)/$($a.Name).tar.bz2" -OutFile $tarball
            tar xjf $tarball -C $sherpaSrc
            if ($LASTEXITCODE -ne 0) { throw "tar extraction failed for $($a.Name)" }
            Remove-Item $tarball
        }
    }
    if (Test-Path "$sherpaSrc\$embedding") {
        Write-Host "sherpa model $embedding already fetched"
    } else {
        Write-Host "fetching sherpa model $embedding..."
        Invoke-WebRequest -Uri $embeddingUrl -OutFile "$sherpaSrc\$embedding"
    }
    if (-not (Test-Path "outputs\models\$refineModel")) {
        Write-Host "fetching whisper refine model $refineModel..."
        Invoke-WebRequest -Uri $refineUrl -OutFile "outputs\models\$refineModel"
    }

    New-Item -ItemType Directory -Force -Path $sherpaDst | Out-Null
    New-Item -ItemType Directory -Force -Path "web\src-tauri\models" | Out-Null
    foreach ($m in ($archives.Name + $embedding)) {
        if (Test-Path "$sherpaDst\$m") { continue }
        if ($m -eq $streaming) {
            # The tarball carries both fp32 (337 MB) and int8 (179 MB) weights; the backend loads
            # int8, so staging only those keeps the installer from carrying a unused copy.
            Write-Host "staging sherpa model $m (int8 only)..."
            New-Item -ItemType Directory -Force -Path "$sherpaDst\$m" | Out-Null
            Copy-Item "$sherpaSrc\$m\*.int8.onnx", "$sherpaSrc\$m\tokens.txt" "$sherpaDst\$m\"
        } else {
            Write-Host "staging sherpa model $m..."
            Copy-Item -Recurse "$sherpaSrc\$m" "$sherpaDst\$m"
        }
    }
    Copy-Item -Force "outputs\models\$refineModel" "web\src-tauri\models\$refineModel"
}
if (Test-Path "web\src-tauri\models\fluidaudio") {
    Write-Warning "web\src-tauri\models\fluidaudio exists (macOS staging); it would be bundled into the Windows installer. Remove it first."
}

# --- Web UI -------------------------------------------------------------------------------------
Push-Location web
npm ci
if ($LASTEXITCODE -ne 0) { throw "npm ci failed" }
npm run build
if ($LASTEXITCODE -ne 0) { throw "web build failed" }
Pop-Location

# --- Core binary --------------------------------------------------------------------------------
# The core is built WITHOUT notes: the local-LLM notes step ships as its own `hearsay-notes` sidecar
# (below) so llama.cpp never co-links with the core's whisper -- both vendor `ggml`, and co-linking
# degrades the whisper refine ~5x (a symbol collision).
$features = "sherpa"
if (-not $NoVulkan) { $features += ",vulkan" }
# macOS ships aec, so Windows does too -- without it the mic picks up the meeting audio whenever the
# user is on speakers. Its bindgen needs libclang, a hard prerequisite above (also needed by the
# hearsay-notes / llama-cpp-2 build).
if (-not $NoAec) { $features += ",aec" }
# The notes sidecar takes the same GPU accel as the core (Vulkan when enabled), on llama-cpp-2.
$notesFeatures = if (-not $NoVulkan) { "vulkan" } else { "" }
Write-Host "building hearsay-core (features: $features) + hearsay-notes (features: $notesFeatures)..."
# Everything here builds against the default dynamic CRT: sherpa-onnx is linked as a DLL (see
# hearsay-inference/Cargo.toml) precisely so no crt-static juggling is needed.
$prevTargetDir = $env:CARGO_TARGET_DIR
if ($coreTarget) { $env:CARGO_TARGET_DIR = $coreTarget }
try {
    # Inside the try so it sees the same CARGO_TARGET_DIR the build will write to.
    Reset-StaleGgmlBuild $(if ($coreTarget) { $coreTarget } else { "rust\target" })
    cargo build --release --manifest-path rust\Cargo.toml -p hearsay-core --features $features
    if ($LASTEXITCODE -ne 0) { throw "cargo build (core) failed" }
    if ($notesFeatures) {
        cargo build --release --manifest-path rust\Cargo.toml -p hearsay-notes --features $notesFeatures
    } else {
        cargo build --release --manifest-path rust\Cargo.toml -p hearsay-notes
    }
    if ($LASTEXITCODE -ne 0) { throw "cargo build (hearsay-notes) failed" }
} finally {
    $env:CARGO_TARGET_DIR = $prevTargetDir
}

New-Item -ItemType Directory -Force -Path "web\src-tauri\binaries" | Out-Null
Copy-Item -Force "$target\hearsay-core.exe" `
    "web\src-tauri\binaries\hearsay-core-x86_64-pc-windows-msvc.exe"
Copy-Item -Force "$target\hearsay-notes.exe" `
    "web\src-tauri\binaries\hearsay-notes-x86_64-pc-windows-msvc.exe"

# sherpa-onnx is linked as DLLs, so they have to sit next to the sidecar at runtime (Windows
# resolves a DLL from the loading binary's directory). sherpa-onnx-sys drops them beside the build
# output; tauri.windows.conf.json maps them to the install root, where the sidecar also lands.
foreach ($dll in @("onnxruntime.dll", "onnxruntime_providers_shared.dll",
        "sherpa-onnx-c-api.dll", "sherpa-onnx-cxx-api.dll")) {
    if (-not (Test-Path "$target\$dll")) {
        throw "$dll missing from $target - expected sherpa-onnx-sys to stage it."
    }
    Copy-Item -Force "$target\$dll" "web\src-tauri\binaries\$dll"
}

# --- Installer ----------------------------------------------------------------------------------
Push-Location web\src-tauri
cargo tauri build --bundles nsis
if ($LASTEXITCODE -ne 0) { throw "tauri build failed" }
Pop-Location

Write-Host ""
Write-Host "built (unsigned): web\src-tauri\target\release\bundle\nsis\"
Get-ChildItem "web\src-tauri\target\release\bundle\nsis\*.exe" | ForEach-Object { Write-Host "  $($_.FullName)" }

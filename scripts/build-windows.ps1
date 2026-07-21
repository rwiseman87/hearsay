# Build the Hearsay Windows installer (NSIS) on a Windows x86_64 machine.
# The Windows counterpart of the Makefile's stage-release + dmg path (Windows has no make).
# Prerequisites: docs/development.md "Windows" section. Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1 [-NoVulkan] [-Aec] [-SkipModels]
#
#   -NoVulkan    drop the vulkan feature (CPU-only whisper/llama; no Vulkan SDK needed to build)
#   -Aec         add the aec echo-cancellation feature
#   -SkipModels  skip model download/staging (reuse what is already staged)
#
# Vulkan is on by default: it covers the ggml half of the stack (the whisper refine and the notes
# LLM) on AMD, NVIDIA, and Intel alike, which is what one installer shipping to unknown hardware
# needs — CUDA would bind the build to one vendor. It does NOT accelerate the live sherpa ASR:
# onnxruntime has no Vulkan execution provider, so that path stays on the CPU regardless. Building
# it needs the Vulkan SDK and Windows long paths (checked below, with the fix in the message).

param(
    [switch]$NoVulkan,
    [switch]$Aec,
    [switch]$SkipModels
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

function Require-Tool($name, $hint) {
    if (-not (Get-Command $name -ErrorAction SilentlyContinue)) {
        throw "$name not found. $hint"
    }
}
Require-Tool cargo "Install Rust (MSVC toolchain) from https://rustup.rs"
Require-Tool npm "Install Node 22 from https://nodejs.org"
Require-Tool cmake "Install CMake (whisper-rs / llama-cpp-2 build) from https://cmake.org"
Require-Tool tar "Windows 10 1803+ ships bsdtar; update Windows"
if (-not (cargo tauri --version 2>$null)) {
    throw "tauri-cli not found. Run: cargo install tauri-cli --locked"
}

# llama-cpp-2 (the always-on notes feature) and the aec feature generate bindings with bindgen,
# which loads libclang.dll at build time.
if (-not $env:LIBCLANG_PATH) {
    $candidates = @("C:\Program Files\LLVM\bin") + (
        Get-ChildItem "C:\Program Files*\Microsoft Visual Studio\*\*\VC\Tools\Llvm\x64\bin" `
            -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName)
    $libclang = $candidates | Where-Object { Test-Path (Join-Path $_ "libclang.dll") } | Select-Object -First 1
    if (-not $libclang) {
        throw "libclang.dll not found (needed by bindgen). Install LLVM: winget install -e --id LLVM.LLVM"
    }
    $env:LIBCLANG_PATH = $libclang
    Write-Host "LIBCLANG_PATH=$libclang"
}

# The vulkan feature compiles ggml's Vulkan backend, which needs the SDK's headers + glslc at build
# time, and Windows long-path support: ggml builds its `vulkan-shaders-gen` helper as a nested cmake
# sub-project, and MSBuild's .tlog paths under it blow past MAX_PATH (error MSB3491) no matter how
# short CARGO_TARGET_DIR is. Fail here with both fixes rather than deep inside ggml's cmake.
if (-not $NoVulkan) {
    if (-not $env:VULKAN_SDK) {
        throw "VULKAN_SDK not set. Install the Vulkan SDK from https://vulkan.lunarg.com/sdk/home#windows, or pass -NoVulkan to build CPU-only."
    }
    $longPaths = (Get-ItemProperty "HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem" `
            -Name LongPathsEnabled -ErrorAction SilentlyContinue).LongPathsEnabled
    if ($longPaths -ne 1) {
        throw @"
Windows long paths are disabled, so the ggml Vulkan shader sub-build will fail with MSB3491.
Enable them from an elevated PowerShell, then reboot:
  Set-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem' -Name LongPathsEnabled -Value 1 -Type DWord
Or pass -NoVulkan to build CPU-only.
"@
    }
    # ggml builds its Vulkan shader generator as a nested cmake sub-project. Under the Visual Studio
    # generator that sub-build fails three different ways (MSB3491 .tlog over MAX_PATH, FTK1011 from
    # FileTracker — which is native and ignores the long-path setting — and an MSB8066/VCEnd custom
    # build step). Ninja produces none of them.
    Require-Tool ninja "Install Ninja: winget install -e --id Ninja-build.Ninja"
    $env:CMAKE_GENERATOR = "Ninja"

    # Even under Ninja, cl.exe is not long-path aware, and the sub-build's compiler-probe objects sit
    # ~230 characters below the target dir. Build into a short path so they stay under MAX_PATH.
    # Applied to the core build only (below), never exported: the Tauri build must keep writing to
    # web\src-tauri\target, which is where the bundle is collected from.
    $coreTarget = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "$env:USERPROFILE\.hs" }
    if ($coreTarget.Length -gt 24) {
        throw "CARGO_TARGET_DIR '$coreTarget' is too long for the ggml Vulkan sub-build; use a path of 24 characters or fewer (or pass -NoVulkan)."
    }
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
    @{ Name = "sherpa-onnx-pyannote-segmentation-3-0"; Tag = "speaker-segmentation-models" }
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
$features = "sherpa,notes"
if (-not $NoVulkan) { $features += ",vulkan" }
if ($Aec) { $features += ",aec" }
Write-Host "building hearsay-core (features: $features)..."
# Everything here builds against the default dynamic CRT: sherpa-onnx is linked as a DLL (see
# hearsay-inference/Cargo.toml) precisely so no crt-static juggling is needed.
$prevTargetDir = $env:CARGO_TARGET_DIR
if ($coreTarget) { $env:CARGO_TARGET_DIR = $coreTarget }
try {
    cargo build --release --manifest-path rust\Cargo.toml -p hearsay-core --features $features
} finally {
    $env:CARGO_TARGET_DIR = $prevTargetDir
}
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

New-Item -ItemType Directory -Force -Path "web\src-tauri\binaries" | Out-Null
Copy-Item -Force "$target\hearsay-core.exe" `
    "web\src-tauri\binaries\hearsay-core-x86_64-pc-windows-msvc.exe"

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

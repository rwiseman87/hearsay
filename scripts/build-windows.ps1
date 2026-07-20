# Build the Hearsay Windows installer (NSIS) on a Windows x86_64 machine.
# The Windows counterpart of the Makefile's stage-release + dmg path (Windows has no make).
# Prerequisites: docs/development.md "Windows" section. Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1 [-Vulkan] [-Aec] [-SkipModels]
#
#   -Vulkan      add the vulkan whisper/llama feature (needs the Vulkan SDK)
#   -Aec         add the aec echo-cancellation feature (needs LLVM/libclang for bindgen)
#   -SkipModels  skip model download/staging (reuse what is already staged)

param(
    [switch]$Vulkan,
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

# --- Models -------------------------------------------------------------------------------------
# Same artifacts and layout as `make fetch-sherpa-models` / `stage-sherpa-models` / `stage-model`.
$sherpaSrc = "outputs\models\sherpa"
$sherpaDst = "web\src-tauri\models\sherpa"
$sherpaRelease = "https://github.com/k2-fsa/sherpa-onnx/releases/download"
$archives = @(
    @{ Name = "sherpa-onnx-streaming-zipformer-en-20M-2023-02-17"; Tag = "asr-models" },
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
        if (-not (Test-Path "$sherpaDst\$m")) {
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
if ($Vulkan) { $features += ",vulkan" }
if ($Aec) { $features += ",aec" }
Write-Host "building hearsay-core (features: $features)..."
cargo build --release --manifest-path rust\Cargo.toml -p hearsay-core --features $features
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

New-Item -ItemType Directory -Force -Path "web\src-tauri\binaries" | Out-Null
Copy-Item -Force "rust\target\release\hearsay-core.exe" `
    "web\src-tauri\binaries\hearsay-core-x86_64-pc-windows-msvc.exe"

# --- Installer ----------------------------------------------------------------------------------
Push-Location web\src-tauri
cargo tauri build --bundles nsis
if ($LASTEXITCODE -ne 0) { throw "tauri build failed" }
Pop-Location

Write-Host ""
Write-Host "built (unsigned): web\src-tauri\target\release\bundle\nsis\"
Get-ChildItem "web\src-tauri\target\release\bundle\nsis\*.exe" | ForEach-Object { Write-Host "  $($_.FullName)" }

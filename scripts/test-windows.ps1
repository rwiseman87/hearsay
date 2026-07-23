# On-demand test suite for Hearsay on Windows (the counterpart of the Makefile; Windows has no make).
# Mirrors the make targets in docs/testing.md, running the same cargo / npm / vitest commands with the
# Windows feature set (sherpa for the live/diarize backend; vulkan for the GPU probes). Nothing here is
# automatic -- run it when you want.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\test-windows.ps1 [-Target ci] [-NoVulkan]
#
#   -Target ci        (default) the deterministic gate: rust + web + Tauri lint/tests
#   -Target web       web unit/component tests only (vitest)
#   -Target tauri     the Tauri shell tests only (cargo test on web\src-tauri)
#   -Target probes    the model/hardware probes -- needs the sherpa models + a Vulkan GPU
#   -Target coverage  coverage report into outputs\coverage\ (cargo-llvm-cov + vitest v8)
#   -Target e2e       the browser end-to-end (Playwright) vs the scripted core + vite
#   -Target all       ci + probes + e2e
#   -NoVulkan         run the rust build + probes CPU-only (drop the vulkan feature)
#
# The e2e target needs the Playwright browser once: cd web; npm install; npx playwright install chromium
#
# Codegen drift, version, audit, and licenses are platform-independent and run on the primary macOS
# gate (`make ci`); this mirror focuses on the Windows-specific test execution and shared-suite parity.
# hearsay-inference\build.rs copies the sherpa / onnxruntime DLLs next to the test binaries, so the
# rust tests need only the sherpa feature to compile the Windows backend and link at runtime.

param(
    [ValidateSet("ci", "web", "tauri", "probes", "coverage", "e2e", "all")]
    [string]$Target = "ci",
    [switch]$NoVulkan
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

# The Windows backend (sherpa live/diarize) is behind the `sherpa` feature; the whisper refine + notes
# LLM take `vulkan` on Windows (see scripts\build-windows.ps1). Feature vars are script-scoped so the
# functions below can splat them.
$rustFeatures = if ($NoVulkan) { "sherpa" } else { "sherpa,vulkan" }
$probeFeatureArgs = if ($NoVulkan) { @() } else { @("--features", "vulkan") }

function Require-Tool($name, $hint) {
    if (-not (Get-Command $name -ErrorAction SilentlyContinue)) {
        throw "$name not found. $hint"
    }
}

# $ErrorActionPreference = Stop does not trip on a native command's non-zero exit, so check explicitly.
function Assert-Ok($label) {
    if ($LASTEXITCODE -ne 0) { throw "$label failed (exit code $LASTEXITCODE)" }
}

function Run-Rust {
    Write-Host "`n==> rust: clippy + fmt --check + cargo test (--features $rustFeatures)" -ForegroundColor Cyan
    cargo clippy --manifest-path rust\Cargo.toml --all-targets --features $rustFeatures -- -D warnings
    Assert-Ok "cargo clippy"
    cargo fmt --manifest-path rust\Cargo.toml --all --check
    Assert-Ok "cargo fmt --check"
    cargo test --manifest-path rust\Cargo.toml --workspace --features $rustFeatures
    Assert-Ok "cargo test"
}

function Run-Tauri {
    Write-Host "`n==> tauri: cargo test (web\src-tauri)" -ForegroundColor Cyan
    cargo test --manifest-path web\src-tauri\Cargo.toml
    Assert-Ok "cargo test (tauri shell)"
}

function Run-Web {
    Write-Host "`n==> web: npm ci + typecheck + lint + test + build" -ForegroundColor Cyan
    Push-Location web
    try {
        npm ci;            Assert-Ok "npm ci"
        npm run typecheck; Assert-Ok "npm run typecheck"
        npm run lint;      Assert-Ok "npm run lint"
        npm run test;      Assert-Ok "npm run test"
        npm run build;     Assert-Ok "npm run build"
    } finally {
        Pop-Location
    }
}

function Run-Probes {
    Write-Host "`n==> probes: model/hardware tests (#[ignore]d)" -ForegroundColor Cyan
    cargo test --manifest-path rust\Cargo.toml -p hearsay-inference @probeFeatureArgs -- --ignored
    Assert-Ok "probes (hearsay-inference)"
    cargo test --manifest-path rust\Cargo.toml -p hearsay-notes @probeFeatureArgs -- --ignored
    Assert-Ok "probes (hearsay-notes)"
    cargo test --manifest-path rust\Cargo.toml -p hearsay-backends --features sherpa -- --ignored
    Assert-Ok "probes (hearsay-backends)"
    cargo test --manifest-path rust\Cargo.toml -p hearsay-capture -- --ignored
    Assert-Ok "probes (hearsay-capture)"
}

function Run-Coverage {
    Require-Tool cargo-llvm-cov "Install it: cargo install cargo-llvm-cov"
    Write-Host "`n==> coverage: cargo-llvm-cov + vitest v8 -> outputs\coverage\" -ForegroundColor Cyan
    New-Item -ItemType Directory -Force -Path outputs\coverage\rust | Out-Null
    cargo llvm-cov clean --workspace --manifest-path rust\Cargo.toml
    Assert-Ok "cargo llvm-cov clean"
    cargo llvm-cov --no-report --workspace --features $rustFeatures --manifest-path rust\Cargo.toml
    Assert-Ok "cargo llvm-cov run"
    cargo llvm-cov report --lcov --output-path outputs\coverage\rust\lcov.info --manifest-path rust\Cargo.toml
    Assert-Ok "cargo llvm-cov report (lcov)"
    cargo llvm-cov report --html --output-dir outputs\coverage\rust --manifest-path rust\Cargo.toml
    Assert-Ok "cargo llvm-cov report (html)"
    Push-Location web
    try { npm run coverage; Assert-Ok "vitest coverage" } finally { Pop-Location }
}

function Run-E2E {
    Write-Host "`n==> e2e: Playwright vs the scripted core + vite" -ForegroundColor Cyan
    if (-not (Test-Path web\node_modules\@playwright\test)) {
        throw "playwright not installed: run 'cd web; npm install' then 'npx playwright install chromium'"
    }
    New-Item -ItemType Directory -Force -Path outputs\e2e | Out-Null
    # The Windows core must compile the sherpa backend (a compile_error otherwise); the scripted engine
    # itself is platform-neutral (no refine, so no vulkan needed). playwright.config.ts reads
    # HEARSAY_CORE_FEATURES to build the `cargo run` command for its core webServer.
    cargo build --manifest-path rust\Cargo.toml -p hearsay-core --features sherpa
    Assert-Ok "cargo build (core, e2e)"
    $env:HEARSAY_CORE_FEATURES = "sherpa"
    Push-Location web
    try {
        npx playwright test; Assert-Ok "playwright test"
    } finally {
        Pop-Location
        Remove-Item Env:\HEARSAY_CORE_FEATURES -ErrorAction SilentlyContinue
    }
}

Require-Tool cargo "Install Rust (MSVC toolchain) from https://rustup.rs"
Require-Tool npm "Install Node from https://nodejs.org"

switch ($Target) {
    "ci"       { Run-Rust; Run-Tauri; Run-Web }
    "web"      { Run-Web }
    "tauri"    { Run-Tauri }
    "probes"   { Run-Probes }
    "coverage" { Run-Coverage }
    "e2e"      { Run-E2E }
    "all"      { Run-Rust; Run-Tauri; Run-Web; Run-Probes; Run-E2E }
}

Write-Host "`nOK: test-windows.ps1 -Target $Target" -ForegroundColor Green

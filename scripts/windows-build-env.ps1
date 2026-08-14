# Shared Windows build preflight, dot-sourced by build-windows.ps1 and test-windows.ps1.
#
# The `vulkan` feature builds ggml's Vulkan backend, and doing that on Windows needs more than the
# SDK: ggml compiles its `vulkan-shaders-gen` helper as a nested cmake sub-project whose paths blow
# past MAX_PATH under the default toolchain. The packaging script learned this the hard way; the
# test script needs the identical setup or `-Target ci` dies inside ggml's cmake with an error that
# looks nothing like the real cause. Keeping both preflights here is what keeps them in step.

function Require-Tool($name, $hint) {
    if (-not (Get-Command $name -ErrorAction SilentlyContinue)) {
        throw "$name not found. $hint"
    }
}

# The PowerShell half of `make version-check`. rust/Cargo.toml is canonical; bump with
# `make set-version` on a Mac, or edit all four and re-run.
function Assert-VersionConsistent {
    # Cargo: `version = "x"`. JSON: `"version": "x"` -- match past the colon so the key isn't caught.
    $read = {
        param($path, $pattern)
        $line = Select-String -Path $path -Pattern $pattern | Select-Object -First 1
        if (-not $line) { throw "no version found in $path" }
        $m = [regex]::Match($line.Line, '^\s*(?:"version"\s*:|version\s*=)\s*"([^"]+)"')
        if (-not $m.Success) { throw "could not parse the version out of $path" }
        $m.Groups[1].Value
    }
    $rust = & $read "rust\Cargo.toml" '^version'
    $files = @{
        "web\src-tauri\Cargo.toml"      = & $read "web\src-tauri\Cargo.toml" '^version'
        "web\src-tauri\tauri.conf.json" = & $read "web\src-tauri\tauri.conf.json" '"version"'
        "web\package.json"              = & $read "web\package.json" '"version"'
        "helper\Info.plist"             = ([regex]::Match(
            (Get-Content "helper\Info.plist" -Raw),
            '<key>CFBundleShortVersionString</key>\s*<string>([^<]+)</string>').Groups[1].Value)
    }
    $bad = $files.GetEnumerator() | Where-Object { $_.Value -ne $rust }
    if ($bad) {
        $bad | ForEach-Object { Write-Host "ERROR: version drift -- $($_.Key)=$($_.Value), expected $rust" }
        throw "version drift; rust\Cargo.toml is canonical"
    }
    Write-Host "version $rust consistent across 5 files"
}

# llama-cpp-2 (the hearsay-notes sidecar) and the aec feature generate bindings with bindgen, which
# loads libclang.dll at build time. No-op when LIBCLANG_PATH is already set.
function Initialize-LibClang {
    if ($env:LIBCLANG_PATH) { return }
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

# Pin the CPU instruction-set floor for the ggml builds. Call before any cargo build/test.
#
# ggml bakes the instruction set in at COMPILE time -- there is no runtime dispatch unless
# GGML_BACKEND_DL + GGML_CPU_ALL_VARIANTS are on, and whisper-rs-sys leaves both off. whisper.cpp's
# cmake defaults GGML_NATIVE=ON, which probes the BUILD host's CPUID (ggml-cpu/cmake/FindSIMD.cmake)
# and picks /arch: from whatever that machine happens to have. Built on a Zen 5 host (AVX-512), the
# result was a hearsay-core.exe that died with STATUS_ILLEGAL_INSTRUCTION (0xC000001D) the instant
# the whisper refine touched a CPU op on any Intel machine -- consumer Intel has had AVX-512 fused
# off since 12th gen. This is NOT Vulkan-specific: the -NoVulkan build picked
# AdvancedVectorExtensions512 exactly the same way, so the pin is unconditional.
#
# AVX2 (Haswell 2013 / Zen 1 2017) is the floor because llama-cpp-sys-2 already derives precisely
# that for the hearsay-notes sidecar shipped in this same installer -- pinning it here makes the two
# ggml builds in one installer agree instead of diverging on host luck.
#
# whisper-rs-sys forwards any GGML_* env var to cmake as a -D define (its build.rs). Setting the
# instruction flags explicitly (not just GGML_NATIVE=OFF) is required: cmake option() keeps whatever
# a previous configure cached, so a stale GGML_AVX2:BOOL=OFF would otherwise survive and silently
# produce an SSE2-only build. On MSVC, GGML_AVX2 alone selects /arch:AVX2 and implies FMA + F16C.
function Initialize-GgmlIsaFloor {
    # `-C target-cpu=native` is the one way the host's ISA still leaks in despite everything below:
    # rustc would emit host-only instructions into hearsay-core's own Rust code (which no GGML_* flag
    # governs), and llama-cpp-sys-2 reads the same RUSTFLAGS to flip its ggml back to GGML_NATIVE=ON
    # -- taking the hearsay-notes sidecar down with it. Refuse rather than ship a host-locked build.
    foreach ($var in @("RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS")) {
        if ((Get-Item "env:$var" -ErrorAction SilentlyContinue).Value -match "target-cpu=native") {
            throw "$var sets target-cpu=native, which bakes this machine's instruction set into the installer. Remove it before building for distribution."
        }
    }
    $env:GGML_NATIVE = "OFF"
    $env:GGML_AVX = "ON"
    $env:GGML_AVX2 = "ON"
    $env:GGML_AVX512 = "OFF"
    $env:GGML_AVX512_VBMI = "OFF"
    $env:GGML_AVX512_VNNI = "OFF"
    $env:GGML_AVX512_BF16 = "OFF"
    Write-Host "ggml ISA floor: AVX2 (GGML_NATIVE=OFF)"
}

# Drop an already-built whisper-rs-sys that was configured with GGML_NATIVE=ON, so the ISA floor
# above actually reaches it. whisper-rs-sys lists no `rerun-if-env-changed` for GGML_*, so cargo
# considers a host-native build fresh and would happily relink the AVX-512 objects into a "fixed"
# installer. Detect that configure and force the one-time rebuild. Cheap no-op once migrated.
function Reset-StaleGgmlBuild($targetDir) {
    $caches = Get-ChildItem -Path "$targetDir\*\build\whisper-rs-sys-*\out\build\CMakeCache.txt" `
        -ErrorAction SilentlyContinue
    $stale = $caches | Where-Object {
        Select-String -Path $_.FullName -Pattern '^GGML_NATIVE:BOOL=ON$' -Quiet
    }
    if (-not $stale) { return }
    Write-Host "whisper-rs-sys was built host-native (GGML_NATIVE=ON); rebuilding at the pinned ISA floor..."
    cargo clean --manifest-path rust\Cargo.toml -p whisper-rs-sys
    if ($LASTEXITCODE -ne 0) { throw "cargo clean (whisper-rs-sys) failed" }
    # `cargo clean -p` only reaches the units matching the current feature resolution, so build dirs
    # from other feature combinations (vulkan vs -NoVulkan) survive with their host-native cmake
    # cache. Delete those too: otherwise this guard re-fires on every later run, turning a one-time
    # migration into a permanent whisper rebuild -- and a build that did reuse one would ship
    # AVX-512 again. Cargo re-runs the build script when its output dir is gone.
    foreach ($cache in $stale) {
        # .FullName, not the DirectoryInfo: a bare DirectoryInfo stringifies to its leaf name ("out"),
        # which Test-Path then resolves against the cwd and quietly reports missing.
        $pkgDir = $cache.Directory.Parent.Parent.FullName  # ...\whisper-rs-sys-<hash>
        if (Test-Path -LiteralPath $pkgDir) { Remove-Item -Recurse -Force -LiteralPath $pkgDir }
    }
}

# Preflight the ggml Vulkan sub-build and return the short cargo target dir it has to build into.
# Sets CMAKE_GENERATOR=Ninja as a side effect. Call only when the vulkan feature is actually on.
function Initialize-VulkanBuild {
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
    # FileTracker -- which is native and ignores the long-path setting -- and an MSB8066/VCEnd custom
    # build step). Ninja produces none of them.
    Require-Tool ninja "Install Ninja: winget install -e --id Ninja-build.Ninja"
    $env:CMAKE_GENERATOR = "Ninja"

    # Even under Ninja, cl.exe is not long-path aware, and the sub-build's compiler-probe objects sit
    # ~230 characters below the target dir. Build into a short path so they stay under MAX_PATH. The
    # caller applies this to the Rust workspace only: the Tauri build must keep writing to
    # web\src-tauri\target, which is where the bundle is collected from.
    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "$env:USERPROFILE\.hs" }
    if ($target.Length -gt 24) {
        throw "CARGO_TARGET_DIR '$target' is too long for the ggml Vulkan sub-build; use a path of 24 characters or fewer (or pass -NoVulkan)."
    }
    return $target
}

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

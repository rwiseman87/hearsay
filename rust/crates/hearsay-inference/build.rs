//! Windows + `sherpa` only: copy the sherpa/onnxruntime DLLs next to cargo's *test* binaries.
//!
//! `sherpa-onnx-sys` stages them in `target/<profile>`, which covers the built binaries, but cargo
//! puts test executables in `target/<profile>/deps`. Windows resolves a DLL from the loading
//! binary's own directory first and then **System32** — and Windows ships its own older
//! `onnxruntime.dll` in System32 — so a test binary with no adjacent copy silently loads that one
//! and dies with STATUS_ACCESS_VIOLATION ("The requested API version [24] is not available").
//! PATH cannot fix it: System32 is searched first.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(all(windows, feature = "sherpa"))]
    {
        let Ok(out_dir) = std::env::var("OUT_DIR") else {
            return;
        };
        // OUT_DIR is <profile>/build/<pkg>-<hash>/out; the profile dir is three levels up.
        let out = std::path::PathBuf::from(out_dir);
        let Some(profile) = out.ancestors().nth(3) else {
            return;
        };
        let deps = profile.join("deps");
        let Ok(entries) = std::fs::read_dir(profile) else {
            return;
        };
        if std::fs::create_dir_all(&deps).is_err() {
            return;
        }
        for entry in entries.flatten() {
            let path = entry.path();
            let is_runtime_dll = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.ends_with(".dll")
                    && (n.starts_with("onnxruntime") || n.starts_with("sherpa-onnx"))
            });
            if is_runtime_dll {
                if let Some(name) = path.file_name() {
                    // Best-effort: a locked destination (a test binary mid-run) is not fatal.
                    let _ = std::fs::copy(&path, deps.join(name));
                }
            }
        }
    }
}

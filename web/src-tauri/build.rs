fn main() {
    // The main window is navigated to the Rust core's loopback URL (a *remote* origin), so Tauri
    // enforces the ACL on our own commands: each must have an `allow-<command>` permission generated
    // here and granted to that origin in `capabilities/default.json`. Registering the commands makes
    // the app ACL manifest; without it every `invoke` is rejected with "not allowed by ACL".
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "erase_all_data",
            "quit_app",
            "notify_still_recording",
        ]),
    ))
    .expect("failed to run tauri-build");
}

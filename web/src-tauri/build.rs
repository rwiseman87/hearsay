fn main() {
    // The main window is navigated to the Rust core's loopback URL (a *remote* origin), so Tauri
    // enforces the ACL on our own commands: each must have an `allow-<command>` permission generated
    // here and granted to that origin in `capabilities/default.json`. Registering the commands makes
    // the app ACL manifest; without it every `invoke` is rejected with "not allowed by ACL".
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "reveal_data_dir",
            "erase_all_data",
            "open_url",
            "quit_app",
            "pick_refine_model",
        ]),
    ))
    .expect("failed to run tauri-build");
}

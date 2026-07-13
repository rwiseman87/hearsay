//! Hearsay desktop shell. Bundles the `hearsay-core` server + the Swift capture/AI sidecars, spawns
//! the core with bundle-resolved paths and a user-writable data dir, and points the window at the
//! loopback URL the core prints. The core is killed on quit so its child sidecars don't leak.

use std::sync::Mutex;

use tauri::{Manager, RunEvent};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

/// The running `hearsay-core` child, kept so it (and its Swift sidecars) can be killed on quit.
struct CoreChild(Mutex<Option<CommandChild>>);

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(CoreChild(Mutex::new(None)))
        .setup(|app| {
            let handle = app.handle().clone();

            // Bundle layout: externalBins are siblings of this binary in Contents/MacOS; web/dist
            // is a bundled resource. The DB + recordings must be user-writable (the .app is not).
            let exe_dir = std::env::current_exe()?
                .parent()
                .expect("executable has a parent directory")
                .to_path_buf();
            let helper = exe_dir.join("hearsay-helper");
            let web_dir = app.path().resource_dir()?.join("web-dist");

            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(data_dir.join("db"))?;
            std::fs::create_dir_all(data_dir.join("recordings"))?;
            let db_url = format!("sqlite://{}", data_dir.join("db/hearsay.db").display());

            let (mut rx, child) = app
                .shell()
                .sidecar("hearsay-core")?
                .env("HEARSAY_HELPER_PATH", helper.to_string_lossy().to_string())
                .env("HEARSAY_WEB_DIR", web_dir.to_string_lossy().to_string())
                .env(
                    "HEARSAY_OUTPUT_DIR",
                    data_dir.join("recordings").to_string_lossy().to_string(),
                )
                .env("DATABASE_URL", db_url)
                .env("ENVIRONMENT", "production")
                .spawn()?;
            app.state::<CoreChild>().0.lock().unwrap().replace(child);

            // Navigate the window to the core's loopback URL once it prints `open: http://…?token=…`.
            tauri::async_runtime::spawn(async move {
                while let Some(event) = rx.recv().await {
                    let bytes = match event {
                        CommandEvent::Stdout(b) | CommandEvent::Stderr(b) => b,
                        _ => continue,
                    };
                    let line = String::from_utf8_lossy(&bytes);
                    if !line.contains("?token=") {
                        continue;
                    }
                    let Some(idx) = line.find("http://127.0.0.1") else {
                        continue;
                    };
                    let url_str = line[idx..].split_whitespace().next().unwrap_or_default();
                    if let (Some(win), Ok(url)) = (
                        handle.get_webview_window("main"),
                        url_str.parse::<tauri::Url>(),
                    ) {
                        let _ = win.navigate(url);
                    }
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error building the Hearsay app")
        .run(|app_handle, event| {
            if let RunEvent::ExitRequested { .. } = event {
                if let Some(child) = app_handle.state::<CoreChild>().0.lock().unwrap().take() {
                    let _ = child.kill();
                }
            }
        });
}

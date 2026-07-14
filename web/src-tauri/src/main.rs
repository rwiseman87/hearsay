//! Hearsay desktop shell. Bundles the `hearsay-core` server + the Swift capture/AI sidecars, spawns
//! the core with bundle-resolved paths and a user-writable data dir, and points the window at the
//! loopback URL the core prints. The core is killed on quit so its child sidecars don't leak.

use std::sync::Mutex;

use tauri::{Manager, RunEvent};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

/// The running `hearsay-core` child, kept so it (and its Swift sidecars) can be killed on quit.
struct CoreChild(Mutex<Option<CommandChild>>);

/// Open the app's data directory (db + recordings) in Finder. The "keep my recordings" path:
/// shows the user where their data lives so they can back it up before dragging the app to the Trash.
#[tauri::command]
fn reveal_data_dir(app: tauri::AppHandle) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::process::Command::new("open")
        .arg(&dir)
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Erase everything Hearsay stored on this Mac and reset its macOS permission grants. Removes the
/// data dir (db + recordings/transcripts), the downloadable model caches, and the disposable WebView
/// state; then `tccutil reset`s so a reinstall re-prompts for mic / system-audio / screen access.
/// Best-effort: a missing path never aborts the wipe. The caller quits via `quit_app` afterward.
#[tauri::command]
fn erase_all_data(app: tauri::AppHandle, core: tauri::State<'_, CoreChild>) -> Result<(), String> {
    // Stop the core (and its Swift sidecars) first so the SQLite file handle is released.
    if let Some(child) = core.0.lock().unwrap().take() {
        let _ = child.kill();
    }

    let home = app.path().home_dir().map_err(|e| e.to_string())?;
    // Hearsay's data + every re-creatable cache (models re-download; WebView state is disposable).
    let targets = [
        app.path().app_data_dir().ok(),
        Some(home.join("Library/Application Support/FluidAudio")),
        Some(home.join(".cache/fluidaudio")),
        Some(home.join("Library/Caches/com.hearsay.app")),
        Some(home.join("Library/WebKit/com.hearsay.app")),
        Some(home.join("Library/HTTPStorages/com.hearsay.app")),
        Some(home.join("Library/Saved Application State/com.hearsay.app.savedState")),
        Some(home.join("Library/Preferences/com.hearsay.app.plist")),
    ];
    for path in targets.into_iter().flatten() {
        let result = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(err) = result {
            if err.kind() != std::io::ErrorKind::NotFound {
                eprintln!("erase: could not remove {}: {err}", path.display());
            }
        }
    }

    // Reset TCC grants for both bundles (the app and its embedded capture helper). `reset All`
    // avoids guessing per-service names across macOS versions.
    for bundle_id in ["com.hearsay.app", "com.hearsay.helper"] {
        let _ = std::process::Command::new("tccutil")
            .args(["reset", "All", bundle_id])
            .status();
    }
    Ok(())
}

/// Open a macOS System Settings deep link (`x-apple.systempreferences:…`) via `open(1)`. The
/// WKWebView drops navigations to non-http URL schemes, so the Permissions panel's "Open in System
/// Settings" links route through this instead of an `<a href>`. Restricted to that one scheme so it
/// can't be used as a general URL opener.
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    if !url.starts_with("x-apple.systempreferences:") {
        return Err("unsupported URL scheme".into());
    }
    std::process::Command::new("open")
        .arg(&url)
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Quit the app. Called after `erase_all_data` so the user can then drag Hearsay to the Trash.
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(CoreChild(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            reveal_data_dir,
            erase_all_data,
            open_url,
            quit_app
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            // Bundle layout: externalBins are siblings of this binary in Contents/MacOS; web/dist
            // is a bundled resource. The DB + recordings must be user-writable (the .app is not).
            let exe_dir = std::env::current_exe()?
                .parent()
                .expect("executable has a parent directory")
                .to_path_buf();
            let helper = exe_dir.join("hearsay-helper");
            let resource_dir = app.path().resource_dir()?;
            let web_dir = resource_dir.join("web-dist");
            // Bundled GGML whisper model for the offline refine; the core defaults to a repo-relative
            // path that doesn't exist in an installed .app, so point it at the resource copy.
            let refine_model = resource_dir.join("models/ggml-large-v3-turbo.bin");

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
                    "HEARSAY_REFINE_MODEL",
                    refine_model.to_string_lossy().to_string(),
                )
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

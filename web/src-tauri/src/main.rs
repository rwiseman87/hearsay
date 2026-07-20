//! Hearsay desktop shell. Bundles the `hearsay-core` server + the Swift capture/AI sidecars, spawns
//! the core with bundle-resolved paths and a user-writable data dir, and points the window at the
//! loopback URL from its readiness handshake. On quit the core is asked to shut down gracefully
//! (SIGTERM, then a SIGKILL backstop) so the active meeting is finalized and its Swift sidecars don't
//! leak. If the core dies during boot, the splash is replaced with an error instead of spinning.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use tauri::{AppHandle, Manager, RunEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

/// The running `hearsay-core` child, kept so it (and its Swift sidecars) can be killed on quit.
struct CoreChild(Mutex<Option<CommandChild>>);

/// Ask the core to shut down cleanly, then force-kill as a backstop. The core watches for SIGTERM
/// (and for its stdin closing) and, on either, stops the active meeting so its row is finalized and
/// the audio flushed before it exits — a bare SIGKILL would strand a `recording` row and orphan the
/// Swift sidecars. We give it a short window to exit on its own, then SIGKILL.
fn stop_core_gracefully(child: CommandChild) {
    #[cfg(unix)]
    {
        // SIGTERM triggers the core's graceful shutdown. `kill(pid, 0)` then only probes existence;
        // bail as soon as the process is gone (normally < 2s) so quitting stays snappy, capping the
        // wait at ~5s for a slow finalize.
        let pid = child.pid() as libc::pid_t;
        unsafe { libc::kill(pid, libc::SIGTERM) };
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            if unsafe { libc::kill(pid, 0) } != 0 {
                return; // process gone -> clean exit, no backstop needed
            }
        }
    }
    // Timed out (or non-unix): force-kill. `kill_on_drop` on the core's own child handles reaps the
    // sidecars.
    let _ = child.kill();
}

/// Render a boot-failure message into the window when the core never becomes ready (it exited during
/// startup, or never wrote its handshake), so the user gets an explanation instead of an eternal
/// splash spinner. Best-effort: a missing window or a failed eval is ignored.
fn show_boot_error(app: &AppHandle, detail: &str) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let html = format!(
        "<div style=\"font-family:-apple-system,system-ui,sans-serif;max-width:34rem;margin:12vh auto;\
         padding:0 2rem;color:#1a1a1a\">\
         <h1 style=\"font-size:1.3rem;margin:0 0 .5rem\">Hearsay could not start</h1>\
         <p style=\"color:#555;line-height:1.5;margin:0 0 1rem\">The background service exited before it \
         was ready. Try reopening the app; if this keeps happening, reinstalling should fix it.</p>\
         <pre style=\"white-space:pre-wrap;background:#f4f4f5;padding:.75rem 1rem;border-radius:8px;\
         color:#444;font-size:.8rem;margin:0\">{}</pre></div>",
        html_escape(detail)
    );
    let js = match serde_json::to_string(&html) {
        Ok(literal) => format!("document.body.innerHTML = {literal};"),
        Err(_) => return,
    };
    let _ = win.eval(&js);
}

/// Escape the untrusted `detail` string before interpolating it into the boot-error markup.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The core's readiness handshake: the resolved loopback port + the per-session token, written by
/// the core to a private 0600 file once it is listening (replaces scraping the token off stdout).
#[derive(Deserialize)]
struct Handshake {
    port: u16,
    token: String,
}

/// Erase everything Hearsay stored on this Mac and reset its macOS permission grants. Removes the
/// data dir (db + recordings/transcripts), the downloadable model caches, and the disposable WebView
/// state; then `tccutil reset`s so a reinstall re-prompts for mic / system-audio / screen access.
/// Best-effort: a missing path never aborts the wipe. The caller quits via `quit_app` afterward.
#[tauri::command]
fn erase_all_data(app: tauri::AppHandle, core: tauri::State<'_, CoreChild>) -> Result<(), String> {
    // Require explicit native confirmation before wiping. Driven from the backend, so a compromised
    // webview cannot suppress or auto-confirm the dialog — the destructive action needs a real click.
    let confirmed = app
        .dialog()
        .message(
            "This permanently deletes all Hearsay recordings, transcripts, and settings on this Mac, \
             and resets its permissions. This cannot be undone.",
        )
        .title("Erase all Hearsay data?")
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Erase".into(),
            "Cancel".into(),
        ))
        .blocking_show();
    if !confirmed {
        return Ok(());
    }

    // Stop the core (and its Swift sidecars) first so the SQLite file handle is released. Graceful
    // so an in-progress meeting is finalized before the DB file is deleted out from under it.
    if let Some(child) = core.0.lock().unwrap().take() {
        stop_core_gracefully(child);
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

/// Quit the app. Called after `erase_all_data` so the user can then drag Hearsay to the Trash.
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

/// Post a native OS notification — the "still recording?" nudge for a user who has switched away from
/// the window (the in-app banner alone would go unseen). Called from the webview only when the window
/// is unfocused. Best-effort: it requests notification permission if not yet granted (macOS prompts
/// on first request), then shows the notification; any failure is returned as a string the caller
/// swallows, so a denied/undelivered notification never disrupts the meeting.
#[tauri::command]
fn notify_still_recording(app: tauri::AppHandle, title: String, body: String) -> Result<(), String> {
    use tauri_plugin_notification::{NotificationExt, PermissionState};
    let notifier = app.notification();
    if notifier.permission_state().map_err(|e| e.to_string())? != PermissionState::Granted {
        // Best-effort: request once. If the user is away (the case this feature targets), they cannot
        // grant it now, so this first notification may not show — a later one will once granted.
        let _ = notifier.request_permission();
    }
    notifier
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|e| e.to_string())
}

/// Open a native file picker for the offline-refine whisper model and return the chosen absolute
/// path (or `None` if the user cancels). Filtered to GGML `.bin` models. Async so it runs off the
/// main thread — the blocking picker dispatches the panel to the main run loop and waits, which
/// would deadlock on the main thread (this is the plugin's documented pattern). The shell only
/// surfaces the chooser; the picked path is handed to the core's `PUT /api/settings/models`, which
/// validates it (GGML magic) and persists it.
#[tauri::command]
async fn pick_refine_model(app: tauri::AppHandle) -> Option<String> {
    app.dialog()
        .file()
        .add_filter("GGML whisper model", &["bin"])
        .blocking_pick_file()
        .and_then(|path| path.into_path().ok())
        .map(|path| path.to_string_lossy().to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .manage(CoreChild(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            erase_all_data,
            quit_app,
            pick_refine_model,
            notify_still_recording
        ])
        .setup(|app| {
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
            // Bundled FluidAudio live models (Parakeet ASR, LS-EEND diarizer, VAD, pyannote refine).
            // The core seeds these into FluidAudio's cache on first launch so the sidecars load them
            // locally instead of downloading from HuggingFace (a self-contained, offline install).
            let fluid_models = resource_dir.join("models/fluidaudio/Models");

            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(data_dir.join("db"))?;
            std::fs::create_dir_all(data_dir.join("recordings"))?;
            // Notes models the user downloads land here (the .app bundle is read-only, and the core's
            // repo-relative `outputs/models` default resolves under the read-only launch dir). Kept in
            // app-data so downloads survive app updates.
            let models_dir = data_dir.join("models");
            std::fs::create_dir_all(&models_dir)?;
            let db_url = format!("sqlite://{}", data_dir.join("db/hearsay.db").display());

            // Private readiness handshake: the core writes {port, token} here once it is listening,
            // and the shell reads it to navigate the webview. This replaces scraping the token off a
            // stdout log line (secret in logs + a brittle re-navigate-on-any-matching-line handshake).
            let cache_dir = app.path().app_cache_dir()?;
            std::fs::create_dir_all(&cache_dir)?;
            let handshake_path = cache_dir.join("core-handshake.json");
            let _ = std::fs::remove_file(&handshake_path); // clear any stale handshake first

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
                    "HEARSAY_FLUID_MODELS_DIR",
                    fluid_models.to_string_lossy().to_string(),
                )
                .env(
                    "HEARSAY_OUTPUT_DIR",
                    data_dir.join("recordings").to_string_lossy().to_string(),
                )
                .env(
                    "HEARSAY_MODELS_DIR",
                    models_dir.to_string_lossy().to_string(),
                )
                .env("DATABASE_URL", db_url)
                .env("ENVIRONMENT", "production")
                .env(
                    "HEARSAY_HANDSHAKE_PATH",
                    handshake_path.to_string_lossy().to_string(),
                )
                .spawn()?;
            app.state::<CoreChild>().0.lock().unwrap().replace(child);

            // Whether the boot outcome has been decided — either we navigated to the ready core, or a
            // boot error was shown. Shared so the drain task, the handshake poll, and the boot timeout
            // each act at most once and never overwrite one another.
            let boot_settled = Arc::new(AtomicBool::new(false));

            // Drain the core's stdout/stderr so its pipe never blocks; JSON logs on stderr are
            // surfaced for debugging. If the core exits before we navigate, it died during boot —
            // show why instead of leaving the splash spinning forever.
            let drain_handle = app.handle().clone();
            let drain_settled = boot_settled.clone();
            tauri::async_runtime::spawn(async move {
                while let Some(event) = rx.recv().await {
                    match event {
                        CommandEvent::Stdout(bytes) | CommandEvent::Stderr(bytes) => {
                            eprint!("{}", String::from_utf8_lossy(&bytes));
                        }
                        CommandEvent::Terminated(payload) => {
                            if !drain_settled.swap(true, Ordering::SeqCst) {
                                let detail = format!(
                                    "hearsay-core exited during startup (code {:?}, signal {:?}).",
                                    payload.code, payload.signal
                                );
                                show_boot_error(&drain_handle, &detail);
                            }
                            break;
                        }
                        _ => {}
                    }
                }
            });

            // Navigate the window once the core writes its handshake file. Polling a private file is
            // the structured replacement for sniffing `?token=` out of a log line.
            let nav_handle = app.handle().clone();
            let nav_settled = boot_settled.clone();
            std::thread::spawn(move || {
                for _ in 0..600 {
                    if let Ok(bytes) = std::fs::read(&handshake_path) {
                        if let Ok(hs) = serde_json::from_slice::<Handshake>(&bytes) {
                            // One-shot: don't leave the token sitting on disk.
                            let _ = std::fs::remove_file(&handshake_path);
                            let url = format!("http://127.0.0.1:{}/?token={}", hs.port, hs.token);
                            if let (Some(win), Ok(url)) = (
                                nav_handle.get_webview_window("main"),
                                url.parse::<tauri::Url>(),
                            ) {
                                if win.navigate(url).is_ok() {
                                    nav_settled.store(true, Ordering::SeqCst);
                                }
                            }
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                // The boot window elapsed with no handshake and no observed exit — surface a timeout so
                // the splash does not spin forever.
                if !nav_settled.swap(true, Ordering::SeqCst) {
                    show_boot_error(
                        &nav_handle,
                        "The background service did not become ready within 30 seconds.",
                    );
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error building the Hearsay app")
        .run(|app_handle, event| {
            if let RunEvent::ExitRequested { .. } = event {
                if let Some(child) = app_handle.state::<CoreChild>().0.lock().unwrap().take() {
                    stop_core_gracefully(child);
                }
            }
        });
}

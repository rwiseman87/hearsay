// Ship a GUI binary on Windows: without this the shell is linked for the console subsystem, so
// Windows allocates a terminal alongside the app window for the whole session. Debug builds keep
// the console, where the core's stdout/stderr is worth having. No effect on macOS. (The core
// sidecar itself never shows one -- tauri-plugin-shell spawns it with CREATE_NO_WINDOW.)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Hearsay desktop shell. Bundles the `hearsay-core` server (plus, on macOS, the Swift capture/AI
//! sidecars), spawns the core with bundle-resolved paths and a user-writable data dir, and points
//! the window at the loopback URL from its readiness handshake. On quit the core is asked to shut
//! down gracefully on macOS (SIGTERM, then a SIGKILL backstop) so the active meeting is finalized
//! and its Swift sidecars don't leak; on Windows there is no graceful signal yet, so quitting
//! mid-meeting relies on the core's startup reconciliation. If the
//! core dies during boot, the splash is replaced with an error instead of spinning.

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

/// Erase everything Hearsay stored on this computer (and on macOS, reset its permission grants).
/// Removes the data dir (db + recordings/transcripts), the downloadable model caches, and the
/// disposable WebView state; macOS additionally `tccutil reset`s so a reinstall re-prompts for
/// mic / system-audio / screen access (Windows has no per-app grants to reset). Best-effort: a
/// missing path never aborts the wipe. The caller quits via `quit_app` afterward.
#[tauri::command]
fn erase_all_data(app: tauri::AppHandle, core: tauri::State<'_, CoreChild>) -> Result<(), String> {
    // Require explicit native confirmation before wiping. Driven from the backend, so a compromised
    // webview cannot suppress or auto-confirm the dialog — the destructive action needs a real click.
    let confirmed = app
        .dialog()
        .message(
            "This permanently deletes all Hearsay recordings, transcripts, and settings on this \
             computer. This cannot be undone.",
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

    // Stop the core first so the SQLite file handle is released before the DB file is deleted out
    // from under it (graceful on macOS, so an in-progress meeting is finalized).
    if let Some(child) = core.0.lock().unwrap().take() {
        stop_core_gracefully(child);
    }

    // Hearsay's data + every re-creatable cache (models re-download; WebView state is disposable).
    #[cfg(target_os = "macos")]
    let targets = {
        let home = app.path().home_dir().map_err(|e| e.to_string())?;
        [
            app.path().app_data_dir().ok(),
            Some(home.join("Library/Application Support/FluidAudio")),
            Some(home.join(".cache/fluidaudio")),
            Some(home.join("Library/Caches/com.hearsay.app")),
            Some(home.join("Library/WebKit/com.hearsay.app")),
            Some(home.join("Library/HTTPStorages/com.hearsay.app")),
            Some(home.join("Library/Saved Application State/com.hearsay.app.savedState")),
            Some(home.join("Library/Preferences/com.hearsay.app.plist")),
        ]
    };
    // Windows: app-data (Roaming: db + recordings + models), local data (WebView2's EBWebView
    // state), and the cache dir (the handshake file).
    #[cfg(windows)]
    let targets = [
        app.path().app_data_dir().ok(),
        app.path().app_local_data_dir().ok(),
        app.path().app_cache_dir().ok(),
    ];
    for path in targets.into_iter().flatten() {
        remove_path_with_retry(&path);
    }

    // Reset TCC grants for both bundles (the app and its embedded capture helper). `reset All`
    // avoids guessing per-service names across macOS versions.
    #[cfg(target_os = "macos")]
    for bundle_id in ["com.hearsay.app", "com.hearsay.helper"] {
        let _ = std::process::Command::new("tccutil")
            .args(["reset", "All", bundle_id])
            .status();
    }
    Ok(())
}

/// Cap for the mirrored core log. Two generations are kept, so the logs occupy at most twice this.
const CORE_LOG_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Size-capped log file for the core's mirrored stdout/stderr.
///
/// The core logs every request, so an append-only file grows without bound — it reached tens of
/// megabytes in normal use. Rotation keeps at most two generations (`core.log` + `core.log.1`), so
/// the logs can never become the storage problem while still retaining enough history to explain a
/// crash that happened before the relaunch.
struct RotatingLog {
    path: std::path::PathBuf,
    file: Option<std::fs::File>,
    /// Bytes in the current file, tracked as we write. Calling `metadata()` per line would put a
    /// syscall in front of every log record.
    written: u64,
    max_bytes: u64,
}

impl RotatingLog {
    /// Open (creating/appending to) `path`, rotating immediately if it is already over the cap —
    /// which is what clears a file that grew unbounded before this existed.
    fn open(path: std::path::PathBuf, max_bytes: u64) -> Self {
        let mut log = RotatingLog {
            path,
            file: None,
            written: 0,
            max_bytes,
        };
        log.reopen();
        if log.written >= log.max_bytes {
            log.rotate();
        }
        log
    }

    fn reopen(&mut self) {
        self.file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok();
        self.written = self
            .file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map_or(0, |m| m.len());
    }

    /// Move the current file aside and start a fresh one, keeping one previous generation.
    fn rotate(&mut self) {
        // Drop the handle before renaming: Windows will not rename an open file.
        self.file = None;
        let previous = self.path.with_extension("log.1");
        if std::fs::rename(&self.path, &previous).is_err() {
            // Rotation is best-effort; if it fails, keep appending rather than losing the output.
            self.reopen();
            return;
        }
        self.reopen();
    }

    fn write(&mut self, text: &str) {
        use std::io::Write;
        if let Some(file) = self.file.as_mut() {
            let _ = write!(file, "{text}");
            self.written = self.written.saturating_add(text.len() as u64);
        }
        if self.written >= self.max_bytes {
            self.rotate();
        }
    }

    fn writeln(&mut self, text: &str) {
        self.write(&format!("{text}\n"));
    }
}

/// Remove a file or directory tree, retrying briefly on a transient failure. `child.kill()`
/// (TerminateProcess) is asynchronous on Windows — `stop_core_gracefully` only waits for the core to
/// exit on Unix — so the core's SQLite handle can outlive the call by a few milliseconds and a first
/// `remove_dir_all` then hits a sharing violation. Retrying with a short backoff lets the handle
/// release. `NotFound` is success (nothing to remove); any other error after the last attempt is
/// logged, never fatal — the wipe is best-effort.
fn remove_path_with_retry(path: &std::path::Path) {
    const ATTEMPTS: usize = 10;
    for attempt in 0..ATTEMPTS {
        let result = if path.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        match result {
            Ok(()) => return,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
            Err(err) if attempt + 1 == ATTEMPTS => {
                eprintln!("erase: could not remove {}: {err}", path.display());
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
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
fn notify_still_recording(
    app: tauri::AppHandle,
    title: String,
    body: String,
) -> Result<(), String> {
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
            // Bundle layout: externalBins are siblings of this binary; web/dist is a bundled
            // resource. The DB + recordings must be user-writable (the install dir is not).
            #[cfg(target_os = "macos")]
            let exe_dir = std::env::current_exe()?
                .parent()
                .expect("executable has a parent directory")
                .to_path_buf();
            #[cfg(target_os = "macos")]
            let helper = exe_dir.join("hearsay-helper");
            let resource_dir = app.path().resource_dir()?;
            let web_dir = resource_dir.join("web-dist");
            // Bundled GGML whisper model for the offline refine; the core defaults to a repo-relative
            // path that doesn't exist in an installed app, so point it at the resource copy. The
            // Windows default is a smaller model — no ANE/Metal on the reference hardware (the
            // Models panel overrides it per install either way).
            #[cfg(target_os = "macos")]
            let refine_model = resource_dir.join("models/ggml-large-v3-turbo.bin");
            #[cfg(windows)]
            let refine_model = resource_dir.join("models/ggml-small.en.bin");
            // Bundled FluidAudio live models (Parakeet ASR, LS-EEND diarizer, VAD, pyannote refine).
            // The core seeds these into FluidAudio's cache on first launch so the sidecars load them
            // locally instead of downloading from HuggingFace (a self-contained, offline install).
            #[cfg(target_os = "macos")]
            let fluid_models = resource_dir.join("models/fluidaudio/Models");
            // Bundled sherpa live/diarize models (the Windows backend reads them in place).
            #[cfg(windows)]
            let sherpa_models = resource_dir.join("models/sherpa");

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

            let cmd = app
                .shell()
                .sidecar("hearsay-core")?
                .env("HEARSAY_WEB_DIR", web_dir.to_string_lossy().to_string())
                .env(
                    "HEARSAY_REFINE_MODEL",
                    refine_model.to_string_lossy().to_string(),
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
                );
            #[cfg(target_os = "macos")]
            let cmd = cmd
                .env("HEARSAY_HELPER_PATH", helper.to_string_lossy().to_string())
                .env(
                    "HEARSAY_FLUID_MODELS_DIR",
                    fluid_models.to_string_lossy().to_string(),
                );
            #[cfg(windows)]
            let cmd = cmd.env(
                "HEARSAY_SHERPA_MODELS_DIR",
                sherpa_models.to_string_lossy().to_string(),
            );
            // Where the core's stdout/stderr is mirrored (see the drain task below).
            let core_log_path = data_dir.join("logs").join("core.log");
            let _ = std::fs::create_dir_all(data_dir.join("logs"));

            let (mut rx, child) = cmd.spawn()?;
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
            // Mirror the core.s output into a log file. The Windows shell is a GUI binary with no
            // console, so `eprint!` alone goes nowhere: a panic in the core would leave no trace and
            // present only as every request failing. Appended (so a crash survives the relaunch that
            // follows it) but size-capped, so it cannot grow without bound.
            let log_path = core_log_path.clone();
            tauri::async_runtime::spawn(async move {
                let mut log = RotatingLog::open(log_path.clone(), CORE_LOG_MAX_BYTES);
                log.writeln("--- core started ---");
                while let Some(event) = rx.recv().await {
                    match event {
                        CommandEvent::Stdout(bytes) | CommandEvent::Stderr(bytes) => {
                            let text = String::from_utf8_lossy(&bytes);
                            eprint!("{text}");
                            log.write(&text);
                        }
                        CommandEvent::Terminated(payload) => {
                            let detail = format!(
                                "hearsay-core exited (code {:?}, signal {:?}).",
                                payload.code, payload.signal
                            );
                            log.writeln(&format!("--- {detail} ---"));
                            // Surface it whether it died during boot or mid-session: without the
                            // core every request fails, so silently leaving the UI up makes the app
                            // look broken in a dozen unrelated ways instead of one obvious one.
                            if !drain_settled.swap(true, Ordering::SeqCst) {
                                show_boot_error(
                                    &drain_handle,
                                    &format!("{detail} During startup."),
                                );
                            } else {
                                show_boot_error(
                                    &drain_handle,
                                    &format!("{detail} See {}", log_path.display()),
                                );
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
                            // The session token is hex ([0-9a-f]), so it needs no percent-encoding
                            // here and the core parses `?token=` without decoding (see security.rs
                            // `query_token`). If the token alphabet ever changes, both ends must add
                            // encode/decode together.
                            let url = format!("http://127.0.0.1:{}/?token={}", hs.port, hs.token);
                            let navigated = match (
                                nav_handle.get_webview_window("main"),
                                url.parse::<tauri::Url>(),
                            ) {
                                (Some(win), Ok(url)) => win.navigate(url).is_ok(),
                                _ => false,
                            };
                            if navigated {
                                nav_settled.store(true, Ordering::SeqCst);
                            } else if !nav_settled.swap(true, Ordering::SeqCst) {
                                // The core is ready but the window/URL/navigate step failed; surface
                                // it instead of returning into an eternal splash spinner.
                                show_boot_error(
                                    &nav_handle,
                                    "Hearsay started but its window could not be opened.",
                                );
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

#[cfg(test)]
mod tests {
    use super::{html_escape, RotatingLog};

    #[test]
    fn rotating_log_caps_the_file_and_keeps_one_previous_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("core.log");
        let previous = tmp.path().join("core.log.1");

        let mut log = RotatingLog::open(path.clone(), 64);
        log.write(&"a".repeat(40));
        assert!(!previous.exists(), "should not rotate below the cap");

        // Crossing the cap moves the current file aside and starts fresh.
        log.write(&"b".repeat(40));
        assert!(previous.is_file(), "should have rotated");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert_eq!(std::fs::read_to_string(&previous).unwrap().len(), 80);

        // Writing continues into the new file, and a second rotation overwrites the previous
        // generation rather than accumulating .2, .3, ...
        log.write(&"c".repeat(70));
        assert!(std::fs::read_to_string(&previous).unwrap().starts_with('c'));
        assert!(!tmp.path().join("core.log.2").exists());
    }

    #[test]
    fn rotating_log_clears_a_file_that_grew_before_the_cap_existed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("core.log");
        std::fs::write(&path, "x".repeat(500)).unwrap();

        // Opening an already-oversized log rotates it immediately — this is what reclaims the space
        // on an install that has been appending forever.
        let mut log = RotatingLog::open(path.clone(), 64);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("core.log.1"))
                .unwrap()
                .len(),
            500
        );

        log.writeln("--- core started ---");
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("core started"));
    }

    #[test]
    fn rotating_log_appends_across_restarts_below_the_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("core.log");

        RotatingLog::open(path.clone(), 1024).writeln("first run");
        RotatingLog::open(path.clone(), 1024).writeln("second run");

        // A crash's output must survive the relaunch that follows it.
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("first run") && body.contains("second run"));
    }

    #[test]
    fn html_escape_neutralizes_markup_metacharacters() {
        assert_eq!(html_escape("plain text"), "plain text");
        assert_eq!(
            html_escape("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;",
        );
        // `&` is escaped first, so an existing entity is not left double-decodable on render.
        assert_eq!(html_escape("Tom & <Jerry>"), "Tom &amp; &lt;Jerry&gt;");
        assert_eq!(html_escape("a &amp; b"), "a &amp;amp; b");
    }
}

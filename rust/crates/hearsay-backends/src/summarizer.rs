//! The post-meeting notes summarizer, shared by every platform backend: resolve the effective GGUF
//! notes model + prompt, then run the summarization in the standalone `hearsay-notes` sidecar over
//! stdio (JSON in, JSON out). The sidecar is a separate process on purpose — it owns llama.cpp, so a
//! crash, stall or memory blowup in generation cannot take down the core, which owns live capture and
//! the meeting database. Always compiled (it links no ML); notes are simply unavailable at runtime when the
//! sidecar binary or a notes model is missing. Wired into the orchestrator so a meeting can
//! auto-generate notes at stop and the manual "Generate notes" route can drive the same path.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use hearsay_orchestrator::OrchestratorError;

/// Deadline for the notes sidecar (killed on expiry) so a hung generation can never wedge the notes
/// route or the auto-at-stop task. Generous: a 4B model on CPU can take minutes on a long transcript.
const NOTES_TIMEOUT: Duration = Duration::from_secs(1200);
/// How often the bounded wait polls the child for exit.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(crate) struct SubprocessSummarizer {
    pub(crate) pool: SqlitePool,
    /// The `hearsay-notes` sidecar binary (a sibling of the core; owns llama.cpp).
    pub(crate) notes_binary: PathBuf,
    /// Bundled config default; the effective model is the `models` preference's `notes_model` else
    /// this, resolved from the DB at each run so a Models-panel change or completed download applies.
    pub(crate) default_model: PathBuf,
    /// Config default prompt template; the effective template is the `models` preference's
    /// `notes_prompt` else this, resolved from the DB at each run so a Models-panel edit applies with
    /// no restart.
    pub(crate) default_prompt: String,
}

/// The request written to the sidecar's stdin (mirrors `hearsay-notes`'s `Request`).
#[derive(Serialize)]
struct NotesRequest<'a> {
    model: &'a str,
    template: &'a str,
    transcript: &'a str,
}

/// The response read from the sidecar's stdout (mirrors `hearsay-notes`'s `Response`): the model's
/// reply verbatim as the note.
#[derive(Deserialize)]
struct NotesResponse {
    content: String,
}

#[async_trait]
impl hearsay_orchestrator::Summarizer for SubprocessSummarizer {
    async fn summarize(
        &self,
        transcript: &str,
    ) -> Result<hearsay_orchestrator::NotesResult, OrchestratorError> {
        if !self.notes_binary.is_file() {
            return Err(OrchestratorError::Backend(format!(
                "notes sidecar not found at {} (build hearsay-notes)",
                self.notes_binary.display()
            )));
        }
        let (_enabled, model) =
            hearsay_db::queries::effective_notes(&self.pool, false, &self.default_model)
                .await
                .map_err(|e| OrchestratorError::Backend(format!("resolve notes model: {e}")))?;
        if model.as_os_str().is_empty() || !model.is_file() {
            return Err(OrchestratorError::Backend(format!(
                "notes model not available at {} (download or select one in Settings > Models)",
                model.display()
            )));
        }
        let template =
            hearsay_db::queries::effective_notes_prompt(&self.pool, &self.default_prompt)
                .await
                .map_err(|e| OrchestratorError::Backend(format!("resolve notes prompt: {e}")))?;

        let binary = self.notes_binary.clone();
        let transcript = transcript.to_string();
        let model = model.to_string_lossy().into_owned();
        // Spawning + waiting on the sidecar is blocking — run off the async runtime, like the refine.
        let response = tokio::task::spawn_blocking(move || {
            run_notes_sidecar(&binary, &model, &template, &transcript)
        })
        .await
        .map_err(|e| OrchestratorError::Backend(format!("notes task panicked: {e}")))?
        .map_err(OrchestratorError::Backend)?;
        Ok(hearsay_orchestrator::NotesResult {
            content: response.content,
        })
    }
}

/// Spawn `hearsay-notes`, write the JSON request to its stdin, and read the JSON response from its
/// stdout, bounded by [`NOTES_TIMEOUT`] (killed on expiry). stdin is written and stdout/stderr are
/// drained on their own threads so a large transcript or reply cannot deadlock the wait by filling a
/// pipe buffer.
fn run_notes_sidecar(
    binary: &Path,
    model: &str,
    template: &str,
    transcript: &str,
) -> Result<NotesResponse, String> {
    let request = serde_json::to_vec(&NotesRequest {
        model,
        template,
        transcript,
    })
    .map_err(|e| format!("serialize notes request: {e}"))?;

    let mut command = Command::new(binary);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("spawn hearsay-notes: {e}"))?;

    let mut stdin = child.stdin.take().expect("stdin piped");
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(&request);
        // Dropping `stdin` closes the pipe so the sidecar's read_to_string returns.
    });
    let mut out_pipe = child.stdout.take().expect("stdout piped");
    let mut err_pipe = child.stderr.take().expect("stderr piped");
    let out_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        buf
    });
    let err_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + NOTES_TIMEOUT;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("wait hearsay-notes: {e}"))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "hearsay-notes timed out after {}s",
                NOTES_TIMEOUT.as_secs()
            ));
        }
        thread::sleep(POLL_INTERVAL);
    };

    let _ = writer.join();
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!(
            "hearsay-notes failed: {}",
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    serde_json::from_slice::<NotesResponse>(&stdout)
        .map_err(|e| format!("parse notes response: {e}"))
}

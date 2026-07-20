//! The post-meeting notes summarizer, shared by every platform backend: resolve the effective
//! GGUF notes model and run the llama.cpp summarization (`hearsay-inference`), off the async
//! runtime (blocking). Gated behind the `notes` feature so a build without it never links
//! llama.cpp. Wired into the orchestrator so a meeting can auto-generate notes at stop and the
//! manual "Generate notes" route can drive the same path.

use std::path::PathBuf;

use async_trait::async_trait;
use sqlx::SqlitePool;

use hearsay_orchestrator::OrchestratorError;

pub(crate) struct LlamaSummarizer {
    pub(crate) pool: SqlitePool,
    /// Bundled config default; the effective model is the `models` preference's `notes_model` else
    /// this, resolved from the DB at each run so a Models-panel change or completed download applies.
    pub(crate) default_model: PathBuf,
    /// Config default prompt template; the effective template is the `models` preference's
    /// `notes_prompt` else this, resolved from the DB at each run so a Models-panel edit applies with
    /// no restart.
    pub(crate) default_prompt: String,
}

#[async_trait]
impl hearsay_orchestrator::Summarizer for LlamaSummarizer {
    async fn summarize(
        &self,
        transcript: &str,
    ) -> Result<hearsay_orchestrator::NotesResult, OrchestratorError> {
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
        let transcript = transcript.to_string();
        // llama.cpp is blocking — run off the async runtime, like the whisper refine.
        let notes = tokio::task::spawn_blocking(move || {
            hearsay_inference::summarize(&model, &template, &transcript)
        })
        .await
        .map_err(|e| OrchestratorError::Backend(format!("notes task panicked: {e}")))?
        .map_err(|e| OrchestratorError::Backend(format!("summarize failed: {e}")))?;
        Ok(hearsay_orchestrator::NotesResult {
            summary: notes.summary,
            action_items: notes.action_items,
        })
    }
}

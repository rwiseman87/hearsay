//! Startup reconciliation of meetings stranded in a non-terminal state.
//!
//! A graceful shutdown finalizes the active meeting, but a hard exit (SIGKILL / panic / power loss)
//! leaves its row `recording` (or `refining`, if it died mid-finalize) with no session to close it,
//! so the UI would render it as live forever. Nothing can be active at startup, so every
//! non-terminal row is stranded: [`reconcile_stranded_meetings`] finalizes each and writes its
//! `transcript.md` / `meeting.json` from the persisted segments — reusing the same
//! [`write_meeting_files`] the orchestrator's stop path uses. Best-effort per meeting: a failure is
//! logged and the sweep continues; it never fails startup.

use std::path::Path;

use sqlx::SqlitePool;

use hearsay_db::models::Meeting;
use hearsay_db::queries;
use hearsay_orchestrator::write_meeting_files;

/// Finalize every meeting left in a non-terminal state by a prior hard exit and write its transcript
/// from the persisted segments. Best-effort: logs and continues past a per-meeting failure, never
/// panics. Call once at startup, before serving.
pub async fn reconcile_stranded_meetings(pool: &SqlitePool, output_dir: &Path) {
    let stranded = match queries::list_nonterminal_meetings(pool).await {
        Ok(stranded) => stranded,
        Err(err) => {
            tracing::warn!(error = %err, "startup reconcile: could not list stranded meetings");
            return;
        }
    };
    if stranded.is_empty() {
        return;
    }
    tracing::info!(
        count = stranded.len(),
        "startup reconcile: finalizing meetings stranded by a prior hard exit"
    );
    for meeting in &stranded {
        match reconcile_one(pool, output_dir, meeting).await {
            Ok(()) => tracing::info!(meeting = %meeting.id, "reconciled stranded meeting"),
            Err(err) => tracing::warn!(
                meeting = %meeting.id,
                error = %err,
                "failed to reconcile stranded meeting; leaving row as-is"
            ),
        }
    }
}

/// Finalize one stranded meeting: mark it `finalized` (stamping `ended_at` if it was never set), then
/// re-read the row and write its transcript from the persisted segments. The row transition is done
/// first, so a later transcript-write failure still leaves the meeting terminal (not stuck live).
async fn reconcile_one(
    pool: &SqlitePool,
    output_dir: &Path,
    meeting: &Meeting,
) -> Result<(), String> {
    queries::reconcile_finalize_meeting(pool, meeting.id)
        .await
        .map_err(|e| format!("finalize row: {e}"))?;
    let finalized = queries::get_meeting(pool, meeting.id)
        .await
        .map_err(|e| format!("reload row: {e}"))?
        .ok_or_else(|| "row vanished during reconcile".to_string())?;
    let segments = queries::list_segments(pool, meeting.id)
        .await
        .map_err(|e| format!("read segments: {e}"))?;
    let dir = finalized.dir_path(output_dir);
    write_meeting_files(&dir, &finalized, &segments)
        .map_err(|e| format!("write transcript: {e}"))?;
    Ok(())
}

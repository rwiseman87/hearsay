//! Per-meeting `transcript.md` + `meeting.json` output (rendered, then atomically written).
//!
//! Written once at stop from the finalized DB segments (already ordered by `start_s`), so the folder
//! is self-describing after a meeting. Consecutive same-speaker segments group under one
//! `### HH:MM:SS — Speaker` header. A live-append path for mid-meeting crash-safety is a deferred
//! follow-up (the live transcript is on the WS + in the DB).

use std::ffi::OsString;
use std::path::Path;

use serde_json::json;

use hearsay_db::models::{Meeting, MeetingStatus, Segment};
use hearsay_db::queries::NotesResult;

use crate::error::OrchestratorError;

/// `HH:MM:SS` from meeting-relative seconds.
fn hhmmss(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    )
}

/// Render the transcript: `# {title}`, then a `### HH:MM:SS — Speaker` header at each speaker change
/// followed by that turn's text. Segments must already be ordered by `start_s`.
fn render(title: &str, segments: &[Segment]) -> String {
    let mut parts: Vec<String> = vec![format!("# {title}\n")];
    let mut last_speaker: Option<&str> = None;
    for seg in segments {
        if last_speaker != Some(seg.speaker_label.as_str()) {
            parts.push(format!(
                "\n### {} — {}\n",
                hhmmss(seg.start_s),
                seg.speaker_label
            ));
            last_speaker = Some(&seg.speaker_label);
        }
        parts.push(seg.text.clone());
    }
    parts.join("\n") + "\n"
}

/// Render the finalized transcript as speaker-attributed text for the notes step's LLM prompt — the
/// same `### HH:MM:SS — Speaker` grouping written to `transcript.md`. Segments must be `start_s`-ordered.
pub fn render_transcript(title: &str, segments: &[Segment]) -> String {
    render(title, segments)
}

/// Render the notes document: a title heading followed by the notes `content` verbatim (the model's
/// reply, which the prompt template shaped as Markdown). The content is used as-is — no structural
/// summary/action-item scaffolding.
fn render_notes(title: &str, notes: &NotesResult) -> String {
    format!("# {title} — Notes\n\n{}\n", notes.content.trim())
}

/// Write `notes.md` (the model's verbatim Markdown notes) into `dir` atomically, mirroring
/// [`write_meeting_files`]'s crash-safe temp-file-plus-rename write.
pub fn write_notes_md(
    dir: &Path,
    meeting: &Meeting,
    notes: &NotesResult,
) -> Result<(), OrchestratorError> {
    std::fs::create_dir_all(dir)?;
    write_atomic(&dir.join("notes.md"), &render_notes(&meeting.title, notes))?;
    Ok(())
}

/// Render the user-authored "My notes" body as `my-notes.md` (a heading + the raw body).
fn render_user_notes(title: &str, body: &str) -> String {
    format!("# {title} — My notes\n\n{}\n", body.trim_end())
}

/// Write `my-notes.md` (the user-authored notes body) into `dir` atomically, mirroring
/// [`write_notes_md`]. Separate from the transcript/notes export so it can be written while the
/// meeting is still recording without touching the in-progress `transcript.md`.
pub fn write_user_notes_md(
    dir: &Path,
    meeting: &Meeting,
    body: &str,
) -> Result<(), OrchestratorError> {
    std::fs::create_dir_all(dir)?;
    write_atomic(
        &dir.join("my-notes.md"),
        &render_user_notes(&meeting.title, body),
    )?;
    Ok(())
}

fn status_str(status: MeetingStatus) -> &'static str {
    match status {
        MeetingStatus::Recording => "recording",
        MeetingStatus::Refining => "refining",
        MeetingStatus::Finalized => "finalized",
    }
}

/// Write `contents` to `path` atomically (temp file + rename), matching the sink's crash-safety.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut tmp_name: OsString = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp_name);
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Write `transcript.md` (rendered from `segments`) + `meeting.json` (folder metadata) into `dir`.
pub fn write_meeting_files(
    dir: &Path,
    meeting: &Meeting,
    segments: &[Segment],
) -> Result<(), OrchestratorError> {
    std::fs::create_dir_all(dir)?;
    write_atomic(
        &dir.join("transcript.md"),
        &render(&meeting.title, segments),
    )?;

    let meta = json!({
        "id": meeting.id.to_string(),
        "title": meeting.title,
        "folder": meeting.folder,
        "status": status_str(meeting.status),
        "started_at": meeting.started_at.to_rfc3339(),
        "ended_at": meeting.ended_at.map(|t| t.to_rfc3339()),
    });
    let rendered = serde_json::to_string_pretty(&meta)
        .map_err(|e| OrchestratorError::Backend(format!("meeting.json: {e}")))?;
    write_atomic(&dir.join("meeting.json"), &(rendered + "\n"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hhmmss_formats_hours_minutes_seconds() {
        assert_eq!(hhmmss(0.0), "00:00:00");
        assert_eq!(hhmmss(65.4), "00:01:05");
        assert_eq!(hhmmss(3661.0), "01:01:01");
        assert_eq!(hhmmss(-5.0), "00:00:00");
    }

    #[test]
    fn render_notes_wraps_content_verbatim_under_a_title() {
        let notes = NotesResult {
            content: "## Summary\n\nWe agreed on the plan.\n\n- Ship it\n- Tell Bob".into(),
        };
        let md = render_notes("Sync", &notes);
        assert!(md.starts_with("# Sync — Notes\n\n"));
        // The model's Markdown is carried through as-is.
        assert!(md.contains("## Summary\n\nWe agreed on the plan.\n\n- Ship it\n- Tell Bob"));
    }

    use hearsay_db::models::Stream;

    fn seg(stream: Stream, speaker: &str, text: &str, start_s: f64) -> Segment {
        Segment {
            id: uuid::Uuid::nil(),
            meeting_id: uuid::Uuid::nil(),
            cluster_id: None,
            stream,
            speaker_label: speaker.to_string(),
            text: text.to_string(),
            start_s,
            end_s: start_s + 2.0,
            created_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            updated_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            edited: false,
        }
    }

    // Locks the transcript.md layout: `# title`, one `### HH:MM:SS — Speaker` header per speaker
    // change (consecutive same-speaker turns share a header), then each turn's text.
    #[test]
    fn transcript_render_snapshot() {
        let segments = vec![
            seg(Stream::Me, "Me", "Morning — shall we start?", 0.0),
            seg(Stream::Them, "Speaker 1", "Yes, let's do it.", 3.0),
            seg(Stream::Them, "Speaker 1", "First item is the release.", 7.0),
            seg(Stream::Them, "Speaker 2", "I have the numbers ready.", 65.0),
            seg(Stream::Me, "Me", "Great, go ahead.", 70.0),
        ];
        insta::assert_snapshot!(render_transcript("Weekly Sync", &segments));
    }
}

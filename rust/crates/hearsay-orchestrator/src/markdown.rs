//! Per-meeting `transcript.md` + `meeting.json` output. Port of
//! `src/hearsay/export/local_markdown.py` (the `LocalMarkdownSink` render + atomic write).
//!
//! Written once at stop from the finalized DB segments (already ordered by `start_s`), so the folder
//! is self-describing after a meeting. Consecutive same-speaker segments group under one
//! `### HH:MM:SS — Speaker` header. The Python sink also appends live for mid-meeting crash-safety;
//! that live-append path is a deferred follow-up (the live transcript is on the WS + in the DB).

use std::ffi::OsString;
use std::path::Path;

use serde_json::json;

use hearsay_db::models::{Meeting, MeetingStatus, Segment};

use crate::error::OrchestratorError;

/// `HH:MM:SS` from meeting-relative seconds (matches Python `_hhmmss`).
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
}

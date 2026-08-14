import { useState } from "react";

import { useEditNotes, useGenerateNotes, useMeetingNotes, useSettings } from "../api/hooks";

interface Props {
  meetingId: string;
  recording: boolean;
}

// The AI-recap block in the meeting-detail rail: local-LLM notes over the finalized transcript, shown
// verbatim as the model produced them (Markdown). The Settings prompt template dictates the format, so
// the notes are a single free-form field — not a parsed summary + action-item structure. Shows the
// stored notes when present, an inline editor, a Generate/Regenerate action, and points at Settings
// when no summarization model is configured yet. Hidden while a meeting is still recording (the
// transcript is not final, so there is nothing to summarize).
export function NotesPanel({ meetingId, recording }: Props) {
  // Skip the fetch while recording — its 404 empty-state would be meaningless mid-meeting.
  const notes = useMeetingNotes(recording ? null : meetingId);
  const generate = useGenerateNotes(meetingId);
  const edit = useEditNotes(meetingId);
  const settings = useSettings();
  const modelReady = settings.data?.models_info.notes_model_exists ?? false;
  const data = notes.data;

  // Inline edit state: the notes content as one editable Markdown field.
  const [editing, setEditing] = useState(false);
  const [contentDraft, setContentDraft] = useState("");
  // Confirm gate for a regenerate that would overwrite manual edits.
  const [confirmRegen, setConfirmRegen] = useState(false);

  if (recording) return null;

  const busy = generate.isPending;
  const label = busy ? "Generating…" : data ? "Regenerate ↻" : "Generate notes";

  const startEdit = () => {
    if (!data) return;
    edit.reset();
    setContentDraft(data.content);
    setEditing(true);
  };
  const save = () => {
    edit.mutate({ content: contentDraft.trim() }, { onSuccess: () => setEditing(false) });
  };
  const runGenerate = () => {
    setConfirmRegen(false);
    generate.mutate();
  };
  const onGenerateClick = () => {
    if (data?.edited) setConfirmRegen(true);
    else runGenerate();
  };

  return (
    <section className="recap">
      <div className="recap__head">
        <span className="recap__eyebrow">◇ AI RECAP</span>
        <div className="recap__head-actions">
          {data && !editing ? (
            <button type="button" className="recap__action" onClick={startEdit}>
              Edit
            </button>
          ) : null}
          {confirmRegen ? (
            <span className="recap__confirm">
              <span className="recap__confirm-text">Replace your edited notes?</span>
              <button type="button" onClick={runGenerate} disabled={busy}>
                Regenerate anyway
              </button>
              <button type="button" onClick={() => setConfirmRegen(false)}>
                Cancel
              </button>
            </span>
          ) : (
            <button
              type="button"
              className={
                "recap__action" +
                (data?.stale && modelReady && !editing ? " recap__action--stale" : "")
              }
              onClick={onGenerateClick}
              disabled={busy || !modelReady || editing}
              title={
                modelReady
                  ? data?.stale
                    ? "The transcript changed — regenerate to re-summarize the edited transcript"
                    : undefined
                  : "Choose a summarization model in Settings › Models"
              }
            >
              {label}
            </button>
          )}
        </div>
      </div>

      {data && !editing && data.stale ? (
        <p className="recap__stale" role="status">
          Transcript changed since these notes were generated — regenerate to update.
        </p>
      ) : null}

      {editing ? (
        <div className="recap__editor">
          <label className="recap__editor-label" htmlFor="notes-content">
            Notes (Markdown)
          </label>
          <textarea
            id="notes-content"
            className="recap__editor-content"
            value={contentDraft}
            disabled={edit.isPending}
            onChange={(event) => setContentDraft(event.target.value)}
          />
          <div className="recap__editor-actions">
            <button type="button" className="line__save" onClick={save} disabled={edit.isPending}>
              {edit.isPending ? "Saving…" : "Save"}
            </button>
            <button
              type="button"
              className="line__cancel"
              onClick={() => setEditing(false)}
              disabled={edit.isPending}
            >
              Cancel
            </button>
          </div>
          {edit.isError ? (
            <p className="recap__error" role="alert">
              {(edit.error as Error).message}
            </p>
          ) : null}
        </div>
      ) : data ? (
        <div className="recap__body">
          <div className="recap__subhead">
            NOTES
            {data.edited ? <span className="recap__badge"> · edited</span> : null}
          </div>
          <p className="recap__summary">{data.content}</p>
          <p className="recap__meta muted">Generated locally by {data.model}</p>
        </div>
      ) : !modelReady ? (
        <p className="muted recap__hint">
          Choose a summarization model in Settings › Models to generate notes for this meeting.
        </p>
      ) : (
        <p className="muted recap__hint">No notes yet — Generate to summarize this meeting.</p>
      )}

      {generate.isError ? (
        <p className="recap__error" role="alert">
          {(generate.error as Error).message}
        </p>
      ) : null}
    </section>
  );
}

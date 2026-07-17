import { useState } from "react";

import { useEditNotes, useGenerateNotes, useMeetingNotes, useSettings } from "../api/hooks";

interface Props {
  meetingId: string;
  recording: boolean;
}

// Post-meeting notes: a local-LLM summary + action items over the finalized transcript. Shows the
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

  // Inline edit state: the summary draft + action items as one-per-line text.
  const [editing, setEditing] = useState(false);
  const [summaryDraft, setSummaryDraft] = useState("");
  const [itemsDraft, setItemsDraft] = useState("");
  // Confirm gate for a regenerate that would overwrite manual edits.
  const [confirmRegen, setConfirmRegen] = useState(false);

  if (recording) return null;

  const busy = generate.isPending;
  const label = busy ? "Generating…" : data ? "Regenerate" : "Generate notes";

  const startEdit = () => {
    if (!data) return;
    edit.reset();
    setSummaryDraft(data.summary);
    setItemsDraft(data.action_items.join("\n"));
    setEditing(true);
  };
  const save = () => {
    const action_items = itemsDraft
      .split("\n")
      .map((line) => line.trim())
      .filter((line) => line.length > 0);
    edit.mutate(
      { summary: summaryDraft.trim(), action_items },
      { onSuccess: () => setEditing(false) },
    );
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
    <section className="notes">
      <div className="notes__head">
        <h3 className="notes__title">
          Notes
          {data?.edited ? <span className="notes__badge"> edited</span> : null}
        </h3>
        <div className="notes__head-actions">
          {data && !editing ? (
            <button type="button" className="notes__edit-btn" onClick={startEdit}>
              Edit
            </button>
          ) : null}
          {confirmRegen ? (
            <span className="notes__confirm">
              <span className="notes__confirm-text">Replace your edited notes?</span>
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
              className={data?.stale && modelReady && !editing ? "notes__regen--stale" : undefined}
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
        <p className="notes__stale" role="status">
          Transcript changed since these notes were generated — regenerate to update.
        </p>
      ) : null}

      {editing ? (
        <div className="notes__editor">
          <label className="notes__editor-label" htmlFor="notes-summary">
            Summary
          </label>
          <textarea
            id="notes-summary"
            className="notes__editor-summary"
            value={summaryDraft}
            disabled={edit.isPending}
            onChange={(event) => setSummaryDraft(event.target.value)}
          />
          <label className="notes__editor-label" htmlFor="notes-items">
            Action items (one per line)
          </label>
          <textarea
            id="notes-items"
            className="notes__editor-items"
            value={itemsDraft}
            disabled={edit.isPending}
            onChange={(event) => setItemsDraft(event.target.value)}
          />
          <div className="notes__editor-actions">
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
            <p className="notes__error" role="alert">
              {(edit.error as Error).message}
            </p>
          ) : null}
        </div>
      ) : data ? (
        <div className="notes__body">
          <h4 className="notes__subhead">Summary</h4>
          <p className="notes__summary">{data.summary}</p>
          {data.action_items.length > 0 ? (
            <>
              <h4 className="notes__subhead">Action items</h4>
              <ul className="notes__actions">
                {data.action_items.map((item, index) => (
                  <li key={index}>{item}</li>
                ))}
              </ul>
            </>
          ) : null}
          <p className="notes__meta muted">Generated locally by {data.model}</p>
        </div>
      ) : !modelReady ? (
        <p className="muted notes__hint">
          Choose a summarization model in Settings › Models to generate a summary and action items.
        </p>
      ) : (
        <p className="muted notes__hint">
          No notes yet — click Generate to summarize this meeting and pull out action items.
        </p>
      )}

      {generate.isError ? (
        <p className="notes__error" role="alert">
          {(generate.error as Error).message}
        </p>
      ) : null}
    </section>
  );
}

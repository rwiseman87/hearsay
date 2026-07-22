import { useUserNotesEditor } from "../hooks/useUserNotesEditor";

interface Props {
  meetingId: string;
}

// The right-hand "My notes" panel: a free-form editor the user types in during the meeting, autosaved
// to the server and exported to my-notes.md. Distinct from the post-meeting LLM notes. Mount this
// keyed by meetingId so its draft resets when the meeting changes.
export function MyNotesPanel({ meetingId }: Props) {
  const { draft, setDraft, flush, status } = useUserNotesEditor(meetingId);

  return (
    <aside className="live__notes">
      <div className="live__notes-head">
        <span className="live__notes-title">My notes</span>
        <span className="live__notes-status" aria-live="polite">
          {status}
        </span>
      </div>
      <textarea
        className="live__notes-editor"
        aria-label="My notes"
        placeholder="Jot notes as the meeting happens…"
        value={draft ?? ""}
        disabled={draft === null}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={flush}
      />
      <div className="live__notes-foot">
        <span className="live__chip" aria-disabled="true">
          + Bookmark
        </span>
        <span className="live__chip" aria-disabled="true">
          @ Mention
        </span>
      </div>
    </aside>
  );
}

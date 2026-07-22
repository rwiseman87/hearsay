import { useUserNotesEditor } from "../hooks/useUserNotesEditor";

interface Props {
  meetingId: string;
}

// The post-meeting "My notes" panel: shows (and lets you keep editing) the free-form notes taken
// during the meeting, so they are not lost once the recording view closes. Same autosaved store the
// live panel uses. Mount keyed by meetingId so the draft resets when switching meetings.
export function UserNotesSection({ meetingId }: Props) {
  const { draft, setDraft, flush, status } = useUserNotesEditor(meetingId);

  return (
    <section className="user-notes">
      <div className="user-notes__head">
        <h3 className="user-notes__title">My notes</h3>
        <span className="user-notes__status" aria-live="polite">
          {status}
        </span>
      </div>
      <textarea
        className="user-notes__editor"
        aria-label="My notes"
        placeholder="Notes you took during the meeting appear here — edit or add more."
        value={draft ?? ""}
        disabled={draft === null}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={flush}
      />
    </section>
  );
}

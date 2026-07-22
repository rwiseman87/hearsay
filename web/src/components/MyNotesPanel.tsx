import { useEffect, useRef, useState } from "react";

import { useSaveUserNotes, useUserNotes } from "../api/hooks";

const AUTOSAVE_DELAY_MS = 800;

interface Props {
  meetingId: string;
}

// The right-hand "My notes" panel: a free-form editor the user types in during the meeting, autosaved
// to the server (debounced) and exported to my-notes.md. Distinct from the post-meeting LLM notes.
// Mount this keyed by meetingId so its draft resets when the meeting changes.
export function MyNotesPanel({ meetingId }: Props) {
  const notes = useUserNotes(meetingId);
  const save = useSaveUserNotes(meetingId);
  // `null` until the initial load resolves (a 404 for "no notes yet" resolves to an empty body).
  const [draft, setDraft] = useState<string | null>(null);
  const [lastSaved, setLastSaved] = useState("");
  // useMutation returns a fresh object each render; a ref keeps the autosave effect from re-timing on
  // every render while still calling the latest mutation.
  const saveRef = useRef(save);
  saveRef.current = save;

  // Seed the draft once the read settles (loading done). 404 -> empty editor.
  useEffect(() => {
    if (draft === null && !notes.isLoading) {
      const body = notes.data?.body ?? "";
      setDraft(body);
      setLastSaved(body);
    }
  }, [draft, notes.isLoading, notes.data]);

  // Debounced autosave: persist once typing pauses and the draft differs from what was last saved.
  useEffect(() => {
    if (draft === null || draft === lastSaved) return;
    const timer = setTimeout(() => {
      saveRef.current.mutate(draft, { onSuccess: () => setLastSaved(draft) });
    }, AUTOSAVE_DELAY_MS);
    return () => clearTimeout(timer);
  }, [draft, lastSaved]);

  // Flush immediately when focus leaves the editor (e.g. the user clicks End), so the last edits are
  // not lost inside the debounce window.
  const flush = () => {
    if (draft !== null && draft !== lastSaved && !save.isPending) {
      saveRef.current.mutate(draft, { onSuccess: () => setLastSaved(draft) });
    }
  };

  const dirty = draft !== null && draft !== lastSaved;
  const status = save.isPending
    ? "saving…"
    : save.isError
      ? "save failed"
      : dirty
        ? "unsaved"
        : draft
          ? "autosaved"
          : "";

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

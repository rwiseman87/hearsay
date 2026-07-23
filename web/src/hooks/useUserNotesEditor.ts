import { useEffect, useRef, useState } from "react";

import { useSaveUserNotes, useUserNotes } from "../api/hooks";

const AUTOSAVE_DELAY_MS = 800;

export interface UserNotesEditor {
  // The current text, or `null` until the initial load resolves (render a disabled field meanwhile).
  draft: string | null;
  setDraft: (value: string) => void;
  // Persist immediately (e.g. on blur), so the last edits are not lost inside the debounce window.
  flush: () => void;
  // A short status word for the "autosaved" indicator: "" | "autosaved" | "unsaved" | "saving…" | "save failed".
  status: string;
}

// Shared autosave logic for the user-authored "My notes" (both the live panel and the post-meeting
// section). Loads the stored body, debounces saves as the user types, and exposes a status word.
// Mount the consumer keyed by meetingId so the draft resets when the meeting changes.
export function useUserNotesEditor(meetingId: string): UserNotesEditor {
  const notes = useUserNotes(meetingId);
  const save = useSaveUserNotes(meetingId);
  const [draft, setDraft] = useState<string | null>(null);
  const [lastSaved, setLastSaved] = useState("");
  // useMutation returns a fresh object each render; a ref keeps the autosave effect from re-timing on
  // every render while still calling the latest mutation.
  const saveRef = useRef(save);
  saveRef.current = save;
  // Latest draft / last-saved in refs so the unmount flush below sees current values (its cleanup has
  // empty deps, so it would otherwise capture the mount-time draft).
  const draftRef = useRef(draft);
  draftRef.current = draft;
  const lastSavedRef = useRef(lastSaved);
  lastSavedRef.current = lastSaved;

  // Seed the draft once the read settles. A 404 (no notes yet) resolves to an empty body.
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

  // Flush any unsaved edits when the editor unmounts. A blur normally covers this, but the live view
  // can be torn down without one — e.g. the inactivity watchdog auto-ends the meeting (recording ->
  // refining) while the user is mid-type; React does not fire the focused textarea's blur on unmount,
  // so the last debounce window of notes would be lost. Fire the save (its hook-level onSuccess seeds
  // the cache; no component state is touched, so it is safe post-unmount).
  useEffect(() => {
    return () => {
      const current = draftRef.current;
      if (current !== null && current !== lastSavedRef.current) {
        saveRef.current.mutate(current);
      }
    };
  }, []);

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

  return { draft, setDraft, flush, status };
}

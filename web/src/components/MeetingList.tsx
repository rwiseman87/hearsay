import { useState } from "react";

import { ApiError } from "../api/client";
import { useDeleteMeeting, useRenameMeeting, useStartMeeting, useStatus } from "../api/hooks";
import type { MeetingRead } from "../api/types";

interface Props {
  meetings: MeetingRead[];
  isLoading: boolean;
  error: unknown;
  selectedId: string | null;
  onSelect: (id: string) => void;
}

function errorMessage(error: unknown): string {
  if (error instanceof ApiError || error instanceof Error) return error.message;
  return String(error);
}

export function MeetingList({ meetings, isLoading, error, selectedId, onSelect }: Props) {
  const [title, setTitle] = useState("");
  // Which meeting is awaiting delete confirmation. Delete is permanent (DB rows + recordings
  // folder), so it takes a second click rather than firing on the first.
  const [confirmingId, setConfirmingId] = useState<string | null>(null);
  // Which meeting is being renamed inline, and the in-progress title. Only one row edits at a time.
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editTitle, setEditTitle] = useState("");
  const start = useStartMeeting();
  const remove = useDeleteMeeting();
  const rename = useRenameMeeting();
  const status = useStatus();

  const startEdit = (meeting: MeetingRead) => {
    rename.reset();
    setConfirmingId(null);
    setEditingId(meeting.id);
    setEditTitle(meeting.title);
  };
  const cancelEdit = () => {
    rename.reset();
    setEditingId(null);
    setEditTitle("");
  };
  const submitEdit = (meeting: MeetingRead) => {
    const trimmed = editTitle.trim();
    if (!trimmed || trimmed === meeting.title) {
      cancelEdit();
      return;
    }
    rename.mutate({ id: meeting.id, title: trimmed }, { onSuccess: cancelEdit });
  };
  // A meeting is already recording (only one runs at a time). The warm pool is intentionally empty
  // during a meeting — it re-warms once this one stops — so don't read that as "models loading".
  const recording = meetings.some((meeting) => meeting.status === "recording");
  // Gate "Start" on the transcription sidecars having loaded their models — starting before then
  // records ~30 s of audio the live view can't transcribe. Treat an errored status probe as ready so
  // a status-endpoint problem never bricks the button (a real broken engine still fails at start).
  const sidecarsReady = status.isError || (status.data?.sidecars_ready ?? false);
  const canStart = !recording && sidecarsReady && !start.isPending;
  const startLabel = start.isPending
    ? "Starting…"
    : recording
      ? "Recording…"
      : sidecarsReady
        ? "Start"
        : "Preparing models…";

  const onStart = () => {
    if (!canStart) return;
    start.mutate(
      { title: title.trim() || null },
      {
        onSuccess: (meeting) => {
          setTitle("");
          onSelect(meeting.id);
        },
      },
    );
  };

  return (
    <aside className="meetings">
      <div className="meetings__new">
        <input
          value={title}
          placeholder="Meeting title (optional)"
          aria-label="Meeting title"
          onChange={(event) => setTitle(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") onStart();
          }}
        />
        <button
          type="button"
          onClick={onStart}
          disabled={!canStart}
          title={
            recording || sidecarsReady
              ? undefined
              : "Loading transcription models — ready to record in a moment"
          }
        >
          {startLabel}
        </button>
      </div>
      {!recording && !sidecarsReady ? (
        <p className="muted meetings__preparing" role="status">
          Loading transcription models… you can start recording once they're ready.
        </p>
      ) : null}
      {start.error ? <p className="error">{errorMessage(start.error)}</p> : null}
      {remove.error ? <p className="error">{errorMessage(remove.error)}</p> : null}
      {rename.error ? <p className="error">{errorMessage(rename.error)}</p> : null}
      {isLoading ? <p className="muted">Loading meetings…</p> : null}
      {error ? <p className="error">{errorMessage(error)}</p> : null}
      <ul className="meetings__list">
        {meetings.map((meeting) => {
          // Scope the pending/disabled state to the row being deleted (react-query exposes the
          // in-flight mutation's argument as `variables`), so one delete doesn't freeze every button.
          const deleting = remove.isPending && remove.variables === meeting.id;
          const renaming = rename.isPending && editingId === meeting.id;
          return (
            <li
              key={meeting.id}
              className={meeting.id === selectedId ? "meetings__item is-selected" : "meetings__item"}
            >
              {editingId === meeting.id ? (
                <form
                  className="meetings__edit"
                  onSubmit={(event) => {
                    event.preventDefault();
                    submitEdit(meeting);
                  }}
                >
                  <input
                    className="meetings__edit-input"
                    value={editTitle}
                    autoFocus
                    aria-label={`Rename ${meeting.title}`}
                    disabled={renaming}
                    onChange={(event) => setEditTitle(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Escape") cancelEdit();
                    }}
                  />
                  <button
                    type="submit"
                    className="meetings__confirm-yes"
                    disabled={renaming || editTitle.trim() === ""}
                  >
                    {renaming ? "Saving…" : "Save"}
                  </button>
                  <button
                    type="button"
                    className="meetings__confirm-no"
                    disabled={renaming}
                    onClick={cancelEdit}
                  >
                    Cancel
                  </button>
                </form>
              ) : (
                <>
                  <button
                    type="button"
                    className="meetings__open"
                    onClick={() => onSelect(meeting.id)}
                  >
                    <span className="meetings__title">{meeting.title}</span>
                    <span className={`badge badge--${meeting.status}`}>{meeting.status}</span>
                  </button>
                  {confirmingId === meeting.id ? (
                    <span className="meetings__confirm">
                      <button
                        type="button"
                        className="meetings__confirm-yes"
                        aria-label={`Confirm delete ${meeting.title}`}
                        disabled={deleting}
                        onClick={() =>
                          remove.mutate(meeting.id, { onSettled: () => setConfirmingId(null) })
                        }
                      >
                        {deleting ? "Deleting…" : "Delete"}
                      </button>
                      <button
                        type="button"
                        className="meetings__confirm-no"
                        aria-label="Cancel delete"
                        disabled={deleting}
                        onClick={() => setConfirmingId(null)}
                      >
                        Cancel
                      </button>
                    </span>
                  ) : (
                    <>
                      <button
                        type="button"
                        className="meetings__edit-btn"
                        aria-label={`Rename ${meeting.title}`}
                        onClick={() => startEdit(meeting)}
                      >
                        ✎
                      </button>
                      <button
                        type="button"
                        className="meetings__delete"
                        aria-label={`Delete ${meeting.title}`}
                        onClick={() => setConfirmingId(meeting.id)}
                      >
                        ✕
                      </button>
                    </>
                  )}
                </>
              )}
            </li>
          );
        })}
        {meetings.length === 0 && !isLoading ? <li className="muted">No meetings yet.</li> : null}
      </ul>
    </aside>
  );
}

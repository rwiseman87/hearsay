import { useState } from "react";

import { ApiError } from "../api/client";
import { useDeleteMeeting, useStartMeeting } from "../api/hooks";
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
  const start = useStartMeeting();
  const remove = useDeleteMeeting();

  const onStart = () => {
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
        <button type="button" onClick={onStart} disabled={start.isPending}>
          {start.isPending ? "Starting…" : "Start"}
        </button>
      </div>
      {start.error ? <p className="error">{errorMessage(start.error)}</p> : null}
      {isLoading ? <p className="muted">Loading meetings…</p> : null}
      {error ? <p className="error">{errorMessage(error)}</p> : null}
      <ul className="meetings__list">
        {meetings.map((meeting) => (
          <li
            key={meeting.id}
            className={meeting.id === selectedId ? "meetings__item is-selected" : "meetings__item"}
          >
            <button type="button" className="meetings__open" onClick={() => onSelect(meeting.id)}>
              <span className="meetings__title">{meeting.title}</span>
              <span className={`badge badge--${meeting.status}`}>{meeting.status}</span>
            </button>
            <button
              type="button"
              className="meetings__delete"
              aria-label={`Delete ${meeting.title}`}
              disabled={remove.isPending}
              onClick={() => remove.mutate(meeting.id)}
            >
              ✕
            </button>
          </li>
        ))}
        {meetings.length === 0 && !isLoading ? <li className="muted">No meetings yet.</li> : null}
      </ul>
    </aside>
  );
}

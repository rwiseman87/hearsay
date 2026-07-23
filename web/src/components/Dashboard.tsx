import { useMemo } from "react";

import type { MeetingRead } from "../api/types";
import { SearchBox } from "./SearchBox";

interface Props {
  meetings: MeetingRead[];
  isLoading: boolean;
  error: unknown;
  onSelect: (id: string) => void;
  // Open a meeting at a matched moment (drives the dashboard search bar).
  onJump: (meetingId: string, startS: number) => void;
}

const RECENT_LIMIT = 12;

// A short "when" label for a recent row: Today / Yesterday / a weekday within the last week / else a
// "Mon D" date. Compared by calendar day, not elapsed hours.
function relativeDay(iso: string, now: Date): string {
  const then = new Date(iso);
  const startOfDay = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const days = Math.round((startOfDay(now) - startOfDay(then)) / 86_400_000);
  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 7) return then.toLocaleDateString("en-US", { weekday: "short" });
  return then.toLocaleDateString("en-US", { month: "short", day: "numeric" });
}

// The home dashboard, shown when no meeting is selected. A greeting, the global transcript search,
// and the most recent meetings as a flat list — the folder tree and per-meeting organisation live in
// the recording flow (the record popover's folder picker), not here.
export function Dashboard({ meetings, isLoading, error, onSelect, onJump }: Props) {
  const now = new Date();
  const recent = useMemo(
    () =>
      [...meetings]
        .sort((a, b) => b.started_at.localeCompare(a.started_at))
        .slice(0, RECENT_LIMIT),
    [meetings],
  );

  return (
    <section className="dashboard">
      <div className="dashboard__hero">
        <h1 className="dashboard__title">What do you need from your meetings?</h1>
        <div className="dashboard__search">
          <SearchBox onJump={onJump} />
        </div>
      </div>
      {error ? (
        <p className="dashboard__error" role="alert">
          {error instanceof Error ? error.message : String(error)}
        </p>
      ) : null}
      <div className="dashboard__recent">
        <p className="dashboard__recent-label">Recent</p>
        {isLoading ? (
          <p className="muted dashboard__recent-empty">Loading…</p>
        ) : recent.length === 0 ? (
          <p className="muted dashboard__recent-empty">No meetings yet.</p>
        ) : (
          <ul className="recent">
            {recent.map((meeting) => (
              <li key={meeting.id}>
                <button
                  type="button"
                  className="recent__row"
                  onClick={() => onSelect(meeting.id)}
                >
                  <span className="recent__when">{relativeDay(meeting.started_at, now)}</span>
                  <span className="recent__title">{meeting.title}</span>
                  {meeting.status !== "finalized" ? (
                    <span className={`badge badge--${meeting.status}`}>{meeting.status}</span>
                  ) : null}
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}

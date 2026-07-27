import { useEffect, useRef, useState, type ReactNode } from "react";

import { useSearch } from "../api/hooks";
import type { SearchHit } from "../api/types";
import { formatClock } from "../hooks/clock";

// Sentinels the server's snippet() wraps around each match (private-use code points U+E000/U+E001,
// so they never collide with transcript text). Split on them to render <mark> without any HTML in
// the payload.
const MARK_START = String.fromCharCode(0xe000);
const MARK_END = String.fromCharCode(0xe001);

// Turn a snippet with the sentinel-wrapped matches into React nodes, bolding the matched spans.
function renderSnippet(snippet: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  let rest = snippet;
  let key = 0;
  while (rest.length > 0) {
    const start = rest.indexOf(MARK_START);
    if (start === -1) {
      nodes.push(rest);
      break;
    }
    if (start > 0) nodes.push(rest.slice(0, start));
    const end = rest.indexOf(MARK_END, start + 1);
    if (end === -1) {
      nodes.push(rest.slice(start + 1));
      break;
    }
    nodes.push(<mark key={key++}>{rest.slice(start + 1, end)}</mark>);
    rest = rest.slice(end + 1);
  }
  return nodes;
}

// Group hits by meeting, preserving the server's relevance order (first appearance wins).
function groupByMeeting(items: SearchHit[]): { id: string; title: string; hits: SearchHit[] }[] {
  const order: string[] = [];
  const byId = new Map<string, { id: string; title: string; hits: SearchHit[] }>();
  for (const hit of items) {
    let group = byId.get(hit.meeting_id);
    if (!group) {
      group = { id: hit.meeting_id, title: hit.meeting_title, hits: [] };
      byId.set(hit.meeting_id, group);
      order.push(hit.meeting_id);
    }
    group.hits.push(hit);
  }
  return order.map((id) => byId.get(id) as { id: string; title: string; hits: SearchHit[] });
}

interface Props {
  // Open a meeting and scroll to a matched moment.
  onJump: (meetingId: string, startS: number) => void;
  // Dismiss the enclosing popover (Escape with an empty box). Omitted for the inline dashboard bar,
  // which has nothing to close.
  onClose?: () => void;
  // Focus the box on mount — the rail popover wants this; the always-present dashboard bar does not.
  autoFocus?: boolean;
}

export function SearchBox({ onJump, onClose, autoFocus = false }: Props) {
  const [text, setText] = useState("");
  const [debounced, setDebounced] = useState("");
  const [open, setOpen] = useState(false);
  const blurTimer = useRef<number | undefined>(undefined);
  const inputRef = useRef<HTMLInputElement>(null);

  // The popover opens on demand, so focus the box as soon as it mounts.
  useEffect(() => {
    if (autoFocus) inputRef.current?.focus();
  }, [autoFocus]);

  // Debounce so each keystroke doesn't hit the endpoint.
  useEffect(() => {
    const t = setTimeout(() => setDebounced(text), 200);
    return () => clearTimeout(t);
  }, [text]);

  const search = useSearch(debounced);
  const groups = search.data ? groupByMeeting(search.data.items) : [];
  const showPanel = open && debounced.trim().length > 0;

  const select = (hit: SearchHit) => {
    onJump(hit.meeting_id, hit.start_s);
    setOpen(false);
  };

  return (
    <div className="search">
      <input
        ref={inputRef}
        className="search__input"
        type="search"
        placeholder="Search transcripts…"
        aria-label="Search transcripts across meetings"
        value={text}
        onChange={(event) => {
          setText(event.target.value);
          setOpen(true);
        }}
        onFocus={() => setOpen(true)}
        onBlur={() => {
          // Delay so a result's click (mouse down → blur → click) still registers.
          blurTimer.current = window.setTimeout(() => setOpen(false), 150);
        }}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            // First Escape clears a query; a second one (empty box) dismisses the popover.
            if (text) {
              setText("");
              setOpen(false);
            } else {
              onClose?.();
            }
          }
        }}
      />
      {showPanel ? (
        <div
          className="search__results"
          // Keep the panel open across the blur that a click inside it triggers.
          onMouseDown={() => window.clearTimeout(blurTimer.current)}
        >
          {search.isLoading ? (
            <p className="search__state muted">Searching…</p>
          ) : search.isError ? (
            <p className="search__state error">{(search.error as Error).message}</p>
          ) : groups.length === 0 ? (
            <p className="search__state muted">No matches.</p>
          ) : (
            groups.map((group) => (
              <div key={group.id} className="search__group">
                <p className="search__group-title">{group.title}</p>
                <ul className="search__hits">
                  {group.hits.map((hit) => (
                    <li key={hit.segment_id}>
                      <button type="button" className="search__hit" onClick={() => select(hit)}>
                        <span className="search__hit-time">{formatClock(hit.start_s)}</span>
                        <span className="search__hit-speaker">{hit.speaker_label}</span>
                        <span className="search__hit-snippet">{renderSnippet(hit.snippet)}</span>
                      </button>
                    </li>
                  ))}
                </ul>
              </div>
            ))
          )}
        </div>
      ) : null}
    </div>
  );
}

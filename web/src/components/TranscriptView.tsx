import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
  type Ref,
} from "react";

import {
  useEditSegment,
  useFolders,
  useIdentities,
  useKeepRecording,
  useReassignSpeaker,
  useRediarize,
  useRevealMeeting,
  useSpeakers,
  useStopMeeting,
} from "../api/hooks";
import { getToken } from "../api/token";
import type {
  FolderRead,
  MeetingRead,
  PageIdentity,
  RefineGapRead,
  SpeakerRead,
} from "../api/types";
import { useTranscript, type TranscriptLine } from "../hooks/useTranscript";
import { NotesPanel } from "./NotesPanel";
import { ME_FILTER_KEY, SpeakerPanel } from "./SpeakerPanel";
import { speakerColorVar } from "./speakerColors";
import { UserNotesSection } from "./UserNotesSection";

const RECAP_MIN = 280;
const RECAP_MAX = 620;
const RECAP_DEFAULT = 360;
const RECAP_KEY = "hearsay.recapWidth";

// datalist of known people offered as autocomplete in the per-line reassign popover (distinct from
// SpeakerPanel's own list id so the two don't collide when both are mounted).
const REASSIGN_SUGGESTIONS_ID = "reassign-identity-suggestions";

const clampRecap = (value: number) => Math.min(Math.max(value, RECAP_MIN), RECAP_MAX);

// AI-recap rail width, persisted across sessions in localStorage so a resize sticks (the horizontal
// analogue of the meeting-list sidebar width).
function useRecapWidth() {
  const [width, setWidth] = useState(() => {
    const stored = Number(localStorage.getItem(RECAP_KEY));
    return Number.isFinite(stored) && stored > 0 ? clampRecap(stored) : RECAP_DEFAULT;
  });
  useEffect(() => {
    localStorage.setItem(RECAP_KEY, String(width));
  }, [width]);
  return [width, setWidth] as const;
}

// The folder chain a meeting sits in, root-first (["Clients", "Northwind"]), walked up parent_id
// from the meeting's folder. Empty when the meeting is unfiled.
function folderChain(folderId: string | null | undefined, folders: FolderRead[]): string[] {
  const byId = new Map(folders.map((folder) => [folder.id, folder]));
  const names: string[] = [];
  const seen = new Set<string>();
  let current = folderId ?? null;
  while (current && byId.has(current) && !seen.has(current)) {
    seen.add(current);
    const folder = byId.get(current)!;
    names.unshift(folder.name);
    current = folder.parent_id ?? null;
  }
  return names;
}

// Per-speaker avatar color, matching SpeakerLine and the speaker chips so a speaker keeps one color
// across the live and finalized views: Me is fixed, every Them speaker comes from the shared map.
function colorVar(line: TranscriptLine): string {
  if (line.stream === "me") return "--me";
  return speakerColorVar(line.speaker_label);
}

// Up to two initials from a speaker label ("Dana Reyes" -> "DR", "Speaker 1" -> "S1", "Me" -> "M").
function initials(label: string): string {
  const parts = label.trim().split(/\s+/).filter(Boolean);
  if (parts.length === 0) return "?";
  const first = parts[0][0] ?? "";
  const second = parts.length > 1 ? (parts[1][0] ?? "") : "";
  return (first + second).toUpperCase();
}

function formatTime(seconds: number): string {
  const whole = Math.max(0, Math.floor(seconds));
  const minutes = Math.floor(whole / 60)
    .toString()
    .padStart(2, "0");
  const secs = (whole % 60).toString().padStart(2, "0");
  return `${minutes}:${secs}`;
}

const MAX_LISTED_GAPS = 5;

// The truncated-refine notice: where the untranscribed audio is, and how to check it by ear.
function RefineNotice({
  coverage,
  gaps,
  onPlay,
}: {
  coverage: number | null;
  gaps: RefineGapRead[] | null;
  onPlay: (seconds: number) => void;
}) {
  const percent = coverage === null ? null : Math.round(coverage * 100);
  if (gaps === null) {
    return (
      <span className="inactivity-banner__text">
        Only {percent === null ? "part" : `about ${percent}%`} of the other side&rsquo;s audible audio
        was transcribed, so part of their transcript may be missing. Refine again to find exactly
        where any untranscribed stretch is.
      </span>
    );
  }
  const totalS = Math.round(gaps.reduce((sum, g) => sum + (g.end_s - g.start_s), 0));
  const listed = gaps.slice(0, MAX_LISTED_GAPS);
  return (
    <span className="inactivity-banner__text">
      {gaps.length === 1
        ? `One stretch of the other side’s audio (${totalS} s) has sound but no transcript`
        : `${gaps.length} stretches of the other side’s audio (${totalS} s in total) have sound but no transcript`}
      {percent === null ? "." : `; about ${percent}% of their audible audio was transcribed.`} Play
      each one to check: if you hear them speaking, the transcriber missed it and refining again
      re-reads it from the recording. If it is music, noise or silence, nothing is missing.
      <span className="refine-gaps">
        {listed.map((g) => (
          <button
            type="button"
            key={g.start_s}
            onClick={() => onPlay(g.start_s)}
            aria-label={`Play the untranscribed stretch at ${formatTime(g.start_s)}`}
          >
            ▶ {formatTime(g.start_s)}–{formatTime(g.end_s)} ({Math.round(g.end_s - g.start_s)} s)
          </button>
        ))}
        {gaps.length > listed.length ? (
          <span>and {gaps.length - listed.length} more</span>
        ) : null}
      </span>
    </span>
  );
}

// The meeting's calendar date for the detail header ("Jul 21, 2026").
function formatMeetingDate(iso: string): string {
  return new Date(iso).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    year: "numeric",
  });
}

// The meeting's wall-clock length ("42 min", "1 hr 5 min") from its start/end, or null while it has
// no end time yet.
function formatDuration(startedAt: string, endedAt: string | null | undefined): string | null {
  if (!endedAt) return null;
  const seconds = (new Date(endedAt).getTime() - new Date(startedAt).getTime()) / 1000;
  if (!Number.isFinite(seconds) || seconds <= 0) return null;
  const mins = Math.round(seconds / 60);
  if (mins < 1) return "<1 min";
  if (mins < 60) return `${mins} min`;
  const hours = Math.floor(mins / 60);
  const rem = mins % 60;
  return rem === 0 ? `${hours} hr` : `${hours} hr ${rem} min`;
}

// Wrap each case-insensitive occurrence of `query` in `text` with a <mark> (the in-meeting find
// highlight). Returns the raw text when there is no query.
function highlightMatches(text: string, query: string): ReactNode {
  const q = query.trim();
  if (!q) return text;
  const lower = text.toLowerCase();
  const needle = q.toLowerCase();
  const out: ReactNode[] = [];
  let i = 0;
  let key = 0;
  while (i < text.length) {
    const idx = lower.indexOf(needle, i);
    if (idx === -1) {
      out.push(text.slice(i));
      break;
    }
    if (idx > i) out.push(text.slice(i, idx));
    out.push(
      <mark key={key++} className="find-hit">
        {text.slice(idx, idx + q.length)}
      </mark>,
    );
    i = idx + q.length;
  }
  return out;
}

type IdentityItem = PageIdentity["items"][number];

// Stable empty fallbacks so a row's `speakers`/`identities` props keep referential identity while the
// underlying query has no data yet (a fresh `[]` each render would defeat the row memo).
const NO_SPEAKERS: SpeakerRead[] = [];
const NO_IDENTITIES: IdentityItem[] = [];

// The "no speaker filter" set, hoisted so clearing the filter restores a stable reference.
const NO_FILTER: ReadonlySet<string> = new Set();

interface TranscriptRowProps {
  line: TranscriptLine;
  index: number;
  active: boolean;
  isJump: boolean;
  isMatch: boolean;
  isCurrentMatch: boolean;
  canEdit: boolean;
  editing: boolean;
  // Only meaningful while `editing` (else ""/false): the draft text + the save-in-flight flag for
  // this row, scoped so a keystroke re-renders only the row being edited, not the whole transcript.
  editText: string;
  editPending: boolean;
  reassignOpen: boolean;
  reassignName: string;
  reassignPending: boolean;
  reassignError: string | null;
  findQuery: string;
  speakers: SpeakerRead[];
  identities: IdentityItem[];
  // Attached only to the active (currently-playing) row so the parent can scroll it into view.
  rowRef?: Ref<HTMLLIElement>;
  onSeek: (startS: number) => void;
  onStartEdit: (id: string, text: string) => void;
  onCancelEdit: () => void;
  onSaveEdit: (id: string, text: string) => void;
  onEditTextChange: (value: string) => void;
  onStartReassign: (id: string) => void;
  onCancelReassign: () => void;
  onReassignToCluster: (segmentId: string, clusterId: string) => void;
  onReassignToName: (segmentId: string, name: string) => void;
  onReassignNameChange: (value: string) => void;
}

// One finalized-transcript row. Memoized so that during live recording (when find/playback/edit state
// is all inert) only the <=2 changed rows re-render per WebSocket event rather than the whole meeting.
// The parent hands it stable callbacks (useCallback) and scopes the edit/reassign draft state to the
// active row so an unrelated row's props never change. Mirrors SpeakerLine's memoization for the live
// view; the two together are finding #1's render-path fix.
const TranscriptRow = memo(function TranscriptRow({
  line,
  index,
  active,
  isJump,
  isMatch,
  isCurrentMatch,
  canEdit,
  editing,
  editText,
  editPending,
  reassignOpen,
  reassignName,
  reassignPending,
  reassignError,
  findQuery,
  speakers,
  identities,
  rowRef,
  onSeek,
  onStartEdit,
  onCancelEdit,
  onSaveEdit,
  onEditTextChange,
  onStartReassign,
  onCancelReassign,
  onReassignToCluster,
  onReassignToName,
  onReassignNameChange,
}: TranscriptRowProps) {
  const className =
    "live-line" +
    (line.kind === "partial" ? " live-line--partial" : "") +
    (active ? " line--active" : "") +
    (isJump ? " line--jump" : "") +
    (isMatch ? " line--match" : "") +
    (isCurrentMatch ? " line--match-current" : "");
  return (
    <li
      data-index={index}
      ref={rowRef}
      className={className}
      style={{ ["--spk" as string]: `var(${colorVar(line)})` } as CSSProperties}
      onClick={() => {
        if (!editing) onSeek(line.start_s);
      }}
      title={editing ? undefined : "Jump to this moment"}
    >
      <span className="live-line__avatar" aria-hidden="true">
        {initials(line.speaker_label)}
      </span>
      <div className="live-line__body">
        <div className="live-line__head">
          <span className="live-line__name">{line.speaker_label}</span>
          <span className="live-line__time">{formatTime(line.start_s)}</span>
        </div>
        {editing ? (
          <form
            className="line__edit"
            onClick={(event) => event.stopPropagation()}
            onSubmit={(event) => {
              event.preventDefault();
              if (line.id) onSaveEdit(line.id, editText);
            }}
          >
            <textarea
              className="line__edit-input"
              value={editText}
              autoFocus
              aria-label="Edit transcript line"
              disabled={editPending}
              onChange={(event) => onEditTextChange(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Escape") onCancelEdit();
              }}
            />
            <div className="line__edit-actions">
              <button
                type="submit"
                className="line__save"
                disabled={editPending || editText.trim() === ""}
              >
                {editPending ? "Saving…" : "Save"}
              </button>
              <button
                type="button"
                className="line__cancel"
                disabled={editPending}
                onClick={(event) => {
                  event.stopPropagation();
                  onCancelEdit();
                }}
              >
                Cancel
              </button>
            </div>
          </form>
        ) : (
          <div className="live-line__text">
            {highlightMatches(line.text, findQuery)}
            {line.edited ? <span className="line__edited-pill">edited</span> : null}
          </div>
        )}
      </div>
      {canEdit && !editing ? (
        <div className="line__actions">
          <button
            type="button"
            className="line__edit-btn"
            aria-label="Edit this line"
            title="Edit this line"
            onClick={(event) => {
              event.stopPropagation();
              if (line.id) onStartEdit(line.id, line.text);
            }}
          >
            <span className="line__edit-icon" aria-hidden="true">
              ✎
            </span>
            <span className="line__edit-label">Edit</span>
          </button>
          {line.stream === "them" ? (
            <button
              type="button"
              className="line__edit-btn"
              aria-label="Reassign speaker"
              title="Reassign speaker"
              onClick={(event) => {
                event.stopPropagation();
                if (line.id) onStartReassign(line.id);
              }}
            >
              <span className="line__edit-icon" aria-hidden="true">
                ⇄
              </span>
              <span className="line__edit-label">Speaker</span>
            </button>
          ) : null}
        </div>
      ) : null}
      {reassignOpen ? (
        <div
          className="line__reassign"
          onClick={(event) => event.stopPropagation()}
          onKeyDown={(event) => {
            if (event.key === "Escape") onCancelReassign();
          }}
        >
          <div className="line__reassign-title">Reassign to</div>
          <ul className="line__reassign-list">
            {speakers.map((speaker) => (
              <li key={speaker.id}>
                <button
                  type="button"
                  className="line__reassign-option"
                  aria-current={speaker.id === line.cluster_id}
                  disabled={reassignPending || speaker.id === line.cluster_id}
                  onClick={() => {
                    if (line.id) onReassignToCluster(line.id, speaker.id);
                  }}
                >
                  {speaker.label}
                  {speaker.id === line.cluster_id ? " (current)" : ""}
                </button>
              </li>
            ))}
          </ul>
          <form
            className="line__reassign-new"
            onSubmit={(event) => {
              event.preventDefault();
              if (line.id) onReassignToName(line.id, reassignName);
            }}
          >
            <input
              className="line__reassign-input"
              aria-label="New speaker name"
              placeholder="New speaker…"
              list={REASSIGN_SUGGESTIONS_ID}
              value={reassignName}
              disabled={reassignPending}
              onChange={(event) => onReassignNameChange(event.target.value)}
            />
            <button
              type="submit"
              className="line__reassign-save"
              disabled={reassignPending || reassignName.trim() === ""}
            >
              {reassignPending ? "…" : "Add"}
            </button>
          </form>
          {reassignError ? <div className="line__reassign-error">{reassignError}</div> : null}
          <button
            type="button"
            className="line__reassign-cancel"
            disabled={reassignPending}
            onClick={onCancelReassign}
          >
            Cancel
          </button>
          <datalist id={REASSIGN_SUGGESTIONS_ID}>
            {identities.map((identity) => (
              <option key={identity.id} value={identity.display_name} />
            ))}
          </datalist>
        </div>
      ) : null}
    </li>
  );
});

interface Props {
  meeting: MeetingRead | null;
  // A request to scroll to and highlight the line nearest `startS` (from a global search result).
  // `nonce` changes on every jump so repeated jumps to the same moment re-trigger.
  jumpTo?: { startS: number; nonce: number } | null;
}

export function TranscriptView({ meeting, jumpTo }: Props) {
  const stop = useStopMeeting();
  const keepRecording = useKeepRecording();
  const rediarize = useRediarize(meeting?.id ?? "");
  const reveal = useRevealMeeting();
  const editSegment = useEditSegment(meeting?.id ?? "");
  const reassign = useReassignSpeaker(meeting?.id ?? "");
  const speakers = useSpeakers(meeting?.id ?? null, meeting?.status === "recording");
  const identities = useIdentities();
  const folders = useFolders();
  const { lines, connection, preparing, inactivityPrompt, micSilent, dismissInactivityPrompt } =
    useTranscript(meeting);

  // Resizable AI-recap rail: its width is driven by --recap-width (a drag on .detail__resizer,
  // persisted in localStorage).
  const [recapWidth, setRecapWidth] = useRecapWidth();
  const recapDrag = useRef<{ startX: number; startWidth: number } | null>(null);
  const onRecapResizeStart = (event: ReactPointerEvent<HTMLDivElement>) => {
    event.preventDefault();
    recapDrag.current = { startX: event.clientX, startWidth: recapWidth };
    event.currentTarget.setPointerCapture(event.pointerId);
  };
  const onRecapResizeMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!recapDrag.current) return;
    // Dragging the divider left widens the rail (the transcript gives up the space).
    setRecapWidth(clampRecap(recapDrag.current.startWidth - (event.clientX - recapDrag.current.startX)));
  };
  const onRecapResizeEnd = (event: ReactPointerEvent<HTMLDivElement>) => {
    recapDrag.current = null;
    event.currentTarget.releasePointerCapture(event.pointerId);
  };
  const onRecapResizeKey = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key === "ArrowLeft") setRecapWidth((w) => clampRecap(w + 16));
    else if (event.key === "ArrowRight") setRecapWidth((w) => clampRecap(w - 16));
  };

  const audioRef = useRef<HTMLAudioElement>(null);
  const activeRef = useRef<HTMLLIElement>(null);
  const linesRef = useRef<HTMLOListElement>(null);
  // Whether the lines list is scrolled to (near) the bottom; when it is, live lines keep pinning the
  // latest into view. Set false once the user scrolls up to read earlier history, so we don't yank it.
  const pinnedToBottom = useRef(true);
  const [currentTime, setCurrentTime] = useState(0);
  const [hasAudio, setHasAudio] = useState(true);
  const [volume, setVolume] = useState(1);
  const [isPlaying, setIsPlaying] = useState(false);
  const [duration, setDuration] = useState(0);
  const audioCtxRef = useRef<AudioContext | null>(null);
  const gainRef = useRef<GainNode | null>(null);
  const volumeRef = useRef(1);

  // In-meeting find (client-side over the loaded lines).
  const [findQuery, setFindQuery] = useState("");
  const [findIndex, setFindIndex] = useState(0);
  // Speaker filter: cluster ids (plus ME_FILTER_KEY) to show. Empty = show everything.
  const [speakerFilter, setSpeakerFilter] = useState<ReadonlySet<string>>(NO_FILTER);
  // Inline text edit: the id of the segment being edited plus its draft text.
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editText, setEditText] = useState("");
  // Per-line speaker reassignment: the id of the segment whose popover is open plus the draft name
  // for the "new speaker" field.
  const [reassigningId, setReassigningId] = useState<string | null>(null);
  const [reassignName, setReassignName] = useState("");
  // Confirm gate for a re-diarize that would discard manual edits to remote-speaker lines.
  const [confirmRefine, setConfirmRefine] = useState(false);
  // The line briefly highlighted after a search jump (cleared on a timer).
  const [jumpIndex, setJumpIndex] = useState<number | null>(null);
  const handledJump = useRef(0);

  // The lines actually rendered. Everything index-based below — playback highlighting, find, the
  // search jump, `data-index` — is computed over THIS list, so there is one index space rather than
  // a visible/underlying pair to keep in step. With no filter this returns `lines` itself, so the
  // live render path is unchanged by reference as well as by value.
  //
  // A Them line with no cluster cannot belong to any chip, so it drops out while filtering. In a
  // finalized meeting every Them line has one (the refine re-creates them, and the live pipeline
  // creates them as it goes), so this only bites on unusual rows.
  const visibleLines = useMemo(() => {
    if (speakerFilter.size === 0) return lines;
    return lines.filter((line) =>
      line.stream === "me"
        ? speakerFilter.has(ME_FILTER_KEY)
        : line.cluster_id != null && speakerFilter.has(line.cluster_id),
    );
  }, [lines, speakerFilter]);

  // The audio.wav timeline is meeting-relative (sample N = second N), so the currently-playing
  // line is the last one whose start time has passed.
  const activeIndex = useMemo(() => {
    if (currentTime <= 0) return -1;
    let index = -1;
    for (let i = 0; i < visibleLines.length; i++) {
      if (visibleLines[i].start_s <= currentTime) index = i;
    }
    return index;
  }, [visibleLines, currentTime]);

  // Indices of lines matching the find query, in document order. Over the visible lines, so the
  // "3/12" counter and the ↑/↓ navigation scope themselves to the filter for free.
  const matchIndices = useMemo(() => {
    const q = findQuery.trim().toLowerCase();
    if (!q) return [] as number[];
    const out: number[] = [];
    for (let i = 0; i < visibleLines.length; i++) {
      if (visibleLines[i].text.toLowerCase().includes(q)) out.push(i);
    }
    return out;
  }, [visibleLines, findQuery]);

  // A set view of the matches for O(1) per-line membership tests in the render loop below.
  const matchSet = useMemo(() => new Set(matchIndices), [matchIndices]);

  // Stable references (see NO_SPEAKERS/NO_IDENTITIES): react-query keeps `data` referentially stable
  // across unchanged refetches, so passing these to every row does not bust the row memo. Declared
  // above the early return so the prune effect below can be an unconditional hook.
  const speakerItems = speakers.data?.items ?? NO_SPEAKERS;
  const identityItems = identities.data?.items ?? NO_IDENTITIES;

  // Drop filter entries whose cluster no longer exists — after a merge (the source cluster is
  // deleted) or a re-diarize (every cluster is replaced). Without this the filter stays non-empty
  // while matching nothing, and the transcript silently empties with no visible cause. Returning the
  // previous set unchanged when nothing was pruned is what keeps this from looping.
  useEffect(() => {
    setSpeakerFilter((prev) => {
      if (prev.size === 0) return prev;
      const valid = new Set(speakerItems.map((speaker) => speaker.id));
      valid.add(ME_FILTER_KEY);
      const next = new Set([...prev].filter((key) => valid.has(key)));
      return next.size === prev.size ? prev : next;
    });
  }, [speakerItems]);

  const onToggleSpeaker = useCallback((key: string) => {
    setSpeakerFilter((prev) => {
      const next = new Set(prev);
      if (!next.delete(key)) next.add(key);
      return next;
    });
  }, []);

  const onClearSpeakerFilter = useCallback(() => setSpeakerFilter(NO_FILTER), []);

  // Scroll a specific line (by its render index) into the middle of the list viewport.
  const scrollToLine = (index: number) => {
    linesRef.current
      ?.querySelector<HTMLElement>(`[data-index="${index}"]`)
      ?.scrollIntoView({ block: "center" });
  };

  // Reset per-meeting UI state when switching meetings.
  //
  // The Web Audio graph is deliberately NOT torn down here. This view is mounted once for the whole
  // session (no `key` on it or on the <audio>), so switching meetings only swaps the element's `src`
  // — the element itself, and the MediaElementAudioSourceNode bound to it, outlive the meeting.
  // Closing the context here would leave the surviving element routed into a closed graph (silent
  // playback) and let the next play call `createMediaElementSource` on it a second time, which
  // throws InvalidStateError out of the onPlay handler. The context is closed on unmount, below.
  useEffect(() => {
    setCurrentTime(0);
    setHasAudio(true);
    // Swapping the <audio> src (no key/remount) stops playback but fires no `pause` event, so reset
    // the transport state by hand; the new src re-reports duration via onLoadedMetadata.
    setIsPlaying(false);
    setDuration(0);
    setFindQuery("");
    setFindIndex(0);
    setSpeakerFilter(NO_FILTER);
    setEditingId(null);
    setConfirmRefine(false);
    setJumpIndex(null);
    pinnedToBottom.current = true; // a freshly opened meeting follows the latest by default
  }, [meeting?.id]);

  // Follow the live transcript: when new lines arrive during recording, keep the newest in view —
  // unless the user has scrolled up (pinnedToBottom is cleared by onScroll below).
  useEffect(() => {
    if (meeting?.status !== "recording") return;
    const el = linesRef.current;
    if (el && pinnedToBottom.current) el.scrollTop = el.scrollHeight;
  }, [lines, meeting?.status]);

  // Close the audio context when the view unmounts.
  useEffect(() => () => void audioCtxRef.current?.close(), []);

  // Keep the highlighted line in view as playback advances.
  useEffect(() => {
    activeRef.current?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  // A search jump: once the transcript has loaded, scroll to and highlight the line nearest the
  // target moment. Keyed on the jump nonce so it fires once per request.
  // A jump comes from cross-meeting search and must always land on its line, so it clears any
  // speaker filter first — otherwise the target may not be on screen at all.
  useEffect(() => {
    if (!jumpTo || jumpTo.nonce === handledJump.current || lines.length === 0) return;
    handledJump.current = jumpTo.nonce;
    setSpeakerFilter(NO_FILTER);
    let target = 0;
    for (let i = 0; i < lines.length; i++) {
      if (lines[i].start_s <= jumpTo.startS) target = i;
    }
    setJumpIndex(target);
    pinnedToBottom.current = false;
    scrollToLine(target);
  }, [jumpTo, lines]);

  // Clear the jump highlight after a moment.
  useEffect(() => {
    if (jumpIndex === null) return;
    const t = setTimeout(() => setJumpIndex(null), 2500);
    return () => clearTimeout(t);
  }, [jumpIndex]);

  // Scroll to the current find match as the pointer moves through the matches.
  useEffect(() => {
    if (matchIndices.length === 0) return;
    const clamped = Math.min(findIndex, matchIndices.length - 1);
    scrollToLine(matchIndices[clamped]);
  }, [findIndex, matchIndices]);

  // Row callbacks handed to every memoized TranscriptRow: kept referentially stable (the react-query
  // mutate/reset handles and the state setters are all stable) so an unrelated row's props never
  // change on a keystroke or a live event. Draft text lives in parent state and is threaded back in
  // via the save/reassign args, so these never close over the changing editText/reassignName. Defined
  // above the early return so the hook order is unconditional.
  const editReset = editSegment.reset;
  const editMutate = editSegment.mutate;
  const reassignReset = reassign.reset;
  const reassignMutate = reassign.mutate;

  const onSeek = useCallback((seconds: number) => {
    const audio = audioRef.current;
    if (!audio) return;
    audio.currentTime = seconds;
    void audio.play();
  }, []);
  const onStartEdit = useCallback(
    (id: string, text: string) => {
      setEditingId(id);
      setEditText(text);
      editReset();
    },
    [editReset],
  );
  const onCancelEdit = useCallback(() => {
    setEditingId(null);
    setEditText("");
  }, []);
  const onSaveEdit = useCallback(
    (id: string, text: string) => {
      const trimmed = text.trim();
      if (!trimmed) return;
      editMutate(
        { segmentId: id, text: trimmed },
        {
          onSuccess: () => {
            setEditingId(null);
            setEditText("");
          },
        },
      );
    },
    [editMutate],
  );
  const onEditTextChange = useCallback((value: string) => setEditText(value), []);

  const onStartReassign = useCallback(
    (id: string) => {
      setReassigningId(id);
      setReassignName("");
      reassignReset();
    },
    [reassignReset],
  );
  const onCancelReassign = useCallback(() => {
    setReassigningId(null);
    setReassignName("");
  }, []);
  const onReassignToCluster = useCallback(
    (segmentId: string, clusterId: string) => {
      reassignMutate(
        { segmentId, clusterId },
        {
          onSuccess: () => {
            setReassigningId(null);
            setReassignName("");
          },
        },
      );
    },
    [reassignMutate],
  );
  const onReassignToName = useCallback(
    (segmentId: string, name: string) => {
      const trimmed = name.trim();
      if (!trimmed) return;
      reassignMutate(
        { segmentId, displayName: trimmed },
        {
          onSuccess: () => {
            setReassigningId(null);
            setReassignName("");
          },
        },
      );
    },
    [reassignMutate],
  );
  const onReassignNameChange = useCallback((value: string) => setReassignName(value), []);

  if (!meeting) {
    return (
      <section className="transcript transcript--empty">
        <p className="muted">Start a meeting or pick one from the list.</p>
      </section>
    );
  }

  const recording = meeting.status === "recording";
  const audioUrl = `/api/meetings/${meeting.id}/audio?token=${encodeURIComponent(getToken())}`;
  const editedThemCount = lines.filter((line) => line.stream === "them" && line.edited).length;
  const currentMatch = matchIndices.length > 0 ? matchIndices[Math.min(findIndex, matchIndices.length - 1)] : -1;
  const hasMe = lines.some((line) => line.stream === "me");

  const stepMatch = (delta: number) => {
    if (matchIndices.length === 0) return;
    setFindIndex((prev) => (prev + delta + matchIndices.length) % matchIndices.length);
  };

  const onRefine = () => {
    if (editedThemCount > 0) {
      setConfirmRefine(true);
      return;
    }
    rediarize.mutate();
  };

  // Route the element through a Web Audio gain node so the slider can boost past 100% (native
  // <audio> volume only attenuates). Built lazily on first play — a user gesture, so the context is
  // allowed to start; `createMediaElementSource` is once-per-element, guarded by the ref.
  const ensureAudioGraph = () => {
    const audio = audioRef.current;
    if (!audio || audioCtxRef.current) return;
    // Boosting past 100% is a nicety; playing the meeting is not. Anything that goes wrong building
    // the graph degrades to the element's own output rather than throwing out of the play handler,
    // and never leaves a half-built context behind to leak.
    let ctx: AudioContext | null = null;
    try {
      ctx = new AudioContext();
      const gain = ctx.createGain();
      gain.gain.value = volumeRef.current;
      ctx.createMediaElementSource(audio).connect(gain).connect(ctx.destination);
      audioCtxRef.current = ctx;
      gainRef.current = gain;
    } catch (error) {
      void ctx?.close();
      console.warn("web audio unavailable; falling back to native volume", error);
    }
  };

  const handleVolume = (value: number) => {
    setVolume(value);
    volumeRef.current = value;
    if (gainRef.current) gainRef.current.gain.value = value;
  };

  const togglePlay = () => {
    const audio = audioRef.current;
    if (!audio) return;
    if (audio.paused) void audio.play();
    else audio.pause();
  };

  const handleSeek = (seconds: number) => {
    const audio = audioRef.current;
    if (!audio) return;
    audio.currentTime = seconds;
    setCurrentTime(seconds);
  };

  const seekMax = duration || 0;
  const seekPct = seekMax > 0 ? (Math.min(currentTime, seekMax) / seekMax) * 100 : 0;
  const durationLabel = formatDuration(meeting.started_at, meeting.ended_at);
  const metaLine = durationLabel
    ? `${formatMeetingDate(meeting.started_at)} · ${durationLabel}`
    : formatMeetingDate(meeting.started_at);
  const crumb = `Meetings${folderChain(meeting.folder_id, folders.data?.items ?? [])
    .map((name) => ` / ${name}`)
    .join("")} /`;

  return (
    <section className="detail">
      <header className="detail__header">
        <span className="detail__crumb" title={crumb}>
          {crumb}
        </span>
        <div className="detail__titleblock">
          <div className="detail__title-row">
            <h2 className="detail__title">{meeting.title}</h2>
            {meeting.status !== "finalized" ? (
              <span className={`badge badge--${meeting.status}`}>{meeting.status}</span>
            ) : null}
          </div>
          <div className="detail__meta">{metaLine}</div>
        </div>
        <div className="detail__actions">
          <button
            type="button"
            className="transcript__reveal"
            onClick={() => reveal.mutate(meeting.id)}
            disabled={reveal.isPending}
            title="Open this meeting's folder (audio, transcript, notes) in Finder"
          >
            {reveal.isPending ? "Opening…" : "Show files"}
          </button>
          {recording ? (
            <button type="button" onClick={() => stop.mutate(meeting.id)} disabled={stop.isPending}>
              {stop.isPending ? "Stopping…" : "Stop"}
            </button>
          ) : (
            <>
              {confirmRefine ? (
                <span className="transcript__confirm">
                  <span className="transcript__confirm-text">
                    Discard {editedThemCount} edit{editedThemCount === 1 ? "" : "s"}?
                  </span>
                  <button
                    type="button"
                    onClick={() => {
                      setConfirmRefine(false);
                      rediarize.mutate();
                    }}
                    disabled={rediarize.isPending}
                  >
                    Refine anyway
                  </button>
                  <button type="button" onClick={() => setConfirmRefine(false)}>
                    Cancel
                  </button>
                </span>
              ) : (
                <button type="button" onClick={onRefine} disabled={rediarize.isPending}>
                  {rediarize.isPending ? "Refining…" : "Refine speakers"}
                </button>
              )}
            </>
          )}
        </div>
      </header>
      {!recording && meeting.refine_incomplete ? (
        <div className="inactivity-banner" role="alert">
          <RefineNotice
            coverage={meeting.refine_coverage ?? null}
            gaps={meeting.refine_gaps ?? null}
            onPlay={onSeek}
          />
          <div className="inactivity-banner__actions">
            <button type="button" onClick={onRefine} disabled={rediarize.isPending}>
              {rediarize.isPending ? "Refining…" : "Refine again"}
            </button>
          </div>
        </div>
      ) : null}
      {recording && micSilent ? (
        <div className="inactivity-banner" role="alert">
          <span className="inactivity-banner__text">
            Your microphone is not being heard — it is sending silence. Check that it is not muted
            and that the right input device is selected. Anything transcribed as "Me" until this
            clears is unreliable.
          </span>
        </div>
      ) : null}
      {recording && inactivityPrompt ? (
        <div className="inactivity-banner" role="alert">
          <span className="inactivity-banner__text">
            Still recording? No speech detected for about{" "}
            {Math.max(1, Math.round(inactivityPrompt.silentSeconds / 60))} minute
            {Math.max(1, Math.round(inactivityPrompt.silentSeconds / 60)) === 1 ? "" : "s"} — this
            meeting will end automatically if the silence continues.
          </span>
          <div className="inactivity-banner__actions">
            <button
              type="button"
              className="inactivity-banner__keep"
              onClick={() => {
                keepRecording.mutate(meeting.id);
                dismissInactivityPrompt();
              }}
            >
              Keep recording
            </button>
            <button
              type="button"
              className="inactivity-banner__stop"
              onClick={() => {
                dismissInactivityPrompt();
                stop.mutate(meeting.id);
              }}
              disabled={stop.isPending}
            >
              {stop.isPending ? "Stopping…" : "Stop now"}
            </button>
          </div>
        </div>
      ) : null}
      {!recording && hasAudio ? (
        <div className="detail__scrubber">
          <audio
            ref={audioRef}
            src={audioUrl}
            preload="metadata"
            onLoadedMetadata={(event) => setDuration(event.currentTarget.duration)}
            onPlay={() => {
              ensureAudioGraph();
              void audioCtxRef.current?.resume();
              setIsPlaying(true);
            }}
            onPause={() => setIsPlaying(false)}
            onEnded={() => setIsPlaying(false)}
            onTimeUpdate={(event) => setCurrentTime(event.currentTarget.currentTime)}
            onError={() => setHasAudio(false)}
          />
          <button
            type="button"
            className="detail__play"
            onClick={togglePlay}
            aria-label={isPlaying ? "Pause" : "Play"}
          >
            {isPlaying ? (
              <svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true" fill="currentColor">
                <rect x="4" y="3" width="3" height="10" rx="1" />
                <rect x="9" y="3" width="3" height="10" rx="1" />
              </svg>
            ) : (
              <svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true" fill="currentColor">
                <path d="M4.5 3.2 12.5 8l-8 4.8z" />
              </svg>
            )}
          </button>
          <span className="detail__time">{formatTime(currentTime)}</span>
          <input
            type="range"
            className="detail__seek"
            min={0}
            max={seekMax}
            step={0.1}
            value={Math.min(currentTime, seekMax)}
            aria-label="Seek"
            style={{
              background: `linear-gradient(to right, var(--accent) ${seekPct}%, var(--surface-2) ${seekPct}%)`,
            }}
            onChange={(event) => handleSeek(event.currentTarget.valueAsNumber)}
          />
          <span className="detail__time detail__time--total">{formatTime(duration)}</span>
          <span className="detail__volume">
            <span className="detail__volume-icon" aria-hidden="true">
              <svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor">
                <path d="M8 2.5 4.6 5.3H2v5.4h2.6L8 13.5z" />
                <path
                  d="M10.6 5.4a3.4 3.4 0 0 1 0 5.2M12.4 3.8a5.8 5.8 0 0 1 0 8.4"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.2"
                  strokeLinecap="round"
                />
              </svg>
            </span>
            <input
              type="range"
              className="detail__volume-slider"
              min={0}
              max={2}
              step={0.01}
              value={volume}
              aria-label="Playback volume"
              title={`Volume ${Math.round(volume * 100)}%`}
              onChange={(event) => handleVolume(event.currentTarget.valueAsNumber)}
            />
          </span>
        </div>
      ) : null}
      <SpeakerPanel
        meetingId={meeting.id}
        live={recording}
        selected={speakerFilter}
        hasMe={hasMe}
        onToggle={onToggleSpeaker}
        onClear={onClearSpeakerFilter}
        visibleCount={visibleLines.length}
        totalCount={lines.length}
      />
      {recording && connection && connection !== "open" ? (
        <p className="transcript__status" role="status">
          {connection === "reconnecting"
            ? "Reconnecting to the live transcript…"
            : "Connecting to the live transcript…"}
        </p>
      ) : null}
      {stop.isError ? (
        <p className="transcript__error" role="alert">
          Could not stop the meeting: {(stop.error as Error).message}
        </p>
      ) : null}
      {rediarize.isError ? (
        <p className="transcript__error" role="alert">
          {(rediarize.error as Error).message}
        </p>
      ) : null}
      {reveal.isError ? (
        <p className="transcript__error" role="alert">
          Could not open the folder: {(reveal.error as Error).message}
        </p>
      ) : null}
      {editSegment.isError ? (
        <p className="transcript__error" role="alert">
          Could not save the edit: {(editSegment.error as Error).message}
        </p>
      ) : null}
      <div
        className="detail__body"
        style={{ ["--recap-width" as string]: `${recapWidth}px` } as CSSProperties}
      >
        <div className="detail__transcript">
          <div className="transcript__toolbar">
            <span className="transcript__toolbar-title">
              Transcript
              {!recording && lines.length > 0 ? (
                <span className="transcript__toolbar-hint"> · hover a line to edit</span>
              ) : null}
            </span>
            {!recording && lines.length > 0 ? (
              <div className="find" role="search">
                <input
                  className="find__input"
                  type="search"
                  placeholder="Find in transcript…"
                  aria-label="Find in transcript"
                  value={findQuery}
                  onChange={(event) => {
                    setFindQuery(event.target.value);
                    setFindIndex(0);
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") {
                      event.preventDefault();
                      stepMatch(event.shiftKey ? -1 : 1);
                    } else if (event.key === "Escape") {
                      setFindQuery("");
                    }
                  }}
                />
                <span className="find__count" aria-live="polite">
                  {findQuery.trim()
                    ? `${matchIndices.length ? Math.min(findIndex, matchIndices.length - 1) + 1 : 0}/${matchIndices.length}`
                    : ""}
                </span>
                <button
                  type="button"
                  className="find__nav"
                  aria-label="Previous match"
                  disabled={matchIndices.length === 0}
                  onClick={() => stepMatch(-1)}
                >
                  ↑
                </button>
                <button
                  type="button"
                  className="find__nav"
                  aria-label="Next match"
                  disabled={matchIndices.length === 0}
                  onClick={() => stepMatch(1)}
                >
                  ↓
                </button>
              </div>
            ) : null}
          </div>
          <ol
            className="transcript__lines"
            ref={linesRef}
            onScroll={(event) => {
              const el = event.currentTarget;
              pinnedToBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
            }}
          >
            {visibleLines.map((line, index) => {
              const active = index === activeIndex;
              const editing = editingId != null && line.id === editingId;
              const reassignOpen = reassigningId != null && line.id === reassigningId;
              return (
                <TranscriptRow
                  key={`${line.stream}:${line.start_s}:${line.kind}`}
                  line={line}
                  index={index}
                  active={active}
                  isJump={index === jumpIndex}
                  isMatch={matchSet.has(index)}
                  isCurrentMatch={index === currentMatch}
                  canEdit={!recording && !!line.id}
                  editing={editing}
                  editText={editing ? editText : ""}
                  editPending={editing ? editSegment.isPending : false}
                  reassignOpen={reassignOpen}
                  reassignName={reassignOpen ? reassignName : ""}
                  reassignPending={reassignOpen ? reassign.isPending : false}
                  reassignError={
                    reassignOpen && reassign.isError ? (reassign.error as Error).message : null
                  }
                  findQuery={findQuery}
                  speakers={speakerItems}
                  identities={identityItems}
                  rowRef={active ? activeRef : undefined}
                  onSeek={onSeek}
                  onStartEdit={onStartEdit}
                  onCancelEdit={onCancelEdit}
                  onSaveEdit={onSaveEdit}
                  onEditTextChange={onEditTextChange}
                  onStartReassign={onStartReassign}
                  onCancelReassign={onCancelReassign}
                  onReassignToCluster={onReassignToCluster}
                  onReassignToName={onReassignToName}
                  onReassignNameChange={onReassignNameChange}
                />
              );
            })}
            {visibleLines.length === 0 ? (
              <li className="muted">
                {lines.length > 0 ? (
                  <>
                    No lines from the selected speakers.{" "}
                    {/* Worded differently from the strip's "Clear filter" on purpose: both are on
                        screen at once here, and two controls sharing an accessible name is
                        ambiguous to screen readers and to anything selecting by name. */}
                    <button
                      type="button"
                      className="settings__link-btn"
                      onClick={onClearSpeakerFilter}
                    >
                      Show all lines
                    </button>
                  </>
                ) : recording ? (
                  preparing ? (
                    "Preparing transcription (loading models)…"
                  ) : (
                    "Listening…"
                  )
                ) : (
                  "No transcript."
                )}
              </li>
            ) : null}
          </ol>
          <div className="detail__fade" aria-hidden="true" />
        </div>
        <div
          className="detail__resizer"
          role="separator"
          aria-orientation="vertical"
          aria-label="Resize recap panel"
          aria-valuenow={recapWidth}
          aria-valuemin={RECAP_MIN}
          aria-valuemax={RECAP_MAX}
          tabIndex={0}
          onPointerDown={onRecapResizeStart}
          onPointerMove={onRecapResizeMove}
          onPointerUp={onRecapResizeEnd}
          onKeyDown={onRecapResizeKey}
        />
        <aside className="detail__recap">
          <NotesPanel meetingId={meeting.id} recording={recording} />
          {!recording ? <UserNotesSection key={meeting.id} meetingId={meeting.id} /> : null}
        </aside>
      </div>
    </section>
  );
}

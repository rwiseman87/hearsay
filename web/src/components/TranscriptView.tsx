import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
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
import type { FolderRead, MeetingRead } from "../api/types";
import { useTranscript, type TranscriptLine } from "../hooks/useTranscript";
import { NotesPanel } from "./NotesPanel";
import { SpeakerPanel } from "./SpeakerPanel";
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
// analogue of the meeting-list sidebar width the app used to keep).
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

// Per-speaker avatar color, matching SpeakerLine so a speaker keeps one color across the live and
// finalized views: Me is fixed; each Them speaker rotates deterministically by a hash of its label.
const SPEAKER_COLORS = ["--spk-1", "--spk-2", "--spk-3", "--spk-4"] as const;

function hashLabel(text: string): number {
  let acc = 0;
  for (let i = 0; i < text.length; i++) acc = (acc * 31 + text.charCodeAt(i)) >>> 0;
  return acc;
}

function colorVar(line: TranscriptLine): string {
  if (line.stream === "me") return "--me";
  return SPEAKER_COLORS[hashLabel(line.speaker_label) % SPEAKER_COLORS.length];
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
  const speakers = useSpeakers(meeting?.id ?? null);
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

  // The audio.wav timeline is meeting-relative (sample N = second N), so the currently-playing
  // line is the last one whose start time has passed.
  const activeIndex = useMemo(() => {
    if (currentTime <= 0) return -1;
    let index = -1;
    for (let i = 0; i < lines.length; i++) {
      if (lines[i].start_s <= currentTime) index = i;
    }
    return index;
  }, [lines, currentTime]);

  // Indices of lines matching the find query, in document order.
  const matchIndices = useMemo(() => {
    const q = findQuery.trim().toLowerCase();
    if (!q) return [] as number[];
    const out: number[] = [];
    for (let i = 0; i < lines.length; i++) {
      if (lines[i].text.toLowerCase().includes(q)) out.push(i);
    }
    return out;
  }, [lines, findQuery]);

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
  // Closing the context here used to leave the surviving element routed into a closed graph (silent
  // playback) and let the next play call `createMediaElementSource` on it a second time, which
  // throws InvalidStateError out of the onPlay handler. The context is closed on unmount, below.
  useEffect(() => {
    setCurrentTime(0);
    setHasAudio(true);
    setFindQuery("");
    setFindIndex(0);
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
  useEffect(() => {
    if (!jumpTo || jumpTo.nonce === handledJump.current || lines.length === 0) return;
    handledJump.current = jumpTo.nonce;
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

  const stepMatch = (delta: number) => {
    if (matchIndices.length === 0) return;
    setFindIndex((prev) => (prev + delta + matchIndices.length) % matchIndices.length);
  };

  const startEdit = (id: string, text: string) => {
    setEditingId(id);
    setEditText(text);
    editSegment.reset();
  };
  const cancelEdit = () => {
    setEditingId(null);
    setEditText("");
  };
  const saveEdit = (id: string) => {
    const trimmed = editText.trim();
    if (!trimmed) return;
    editSegment.mutate({ segmentId: id, text: trimmed }, { onSuccess: cancelEdit });
  };

  const startReassign = (id: string) => {
    setReassigningId(id);
    setReassignName("");
    reassign.reset();
  };
  const cancelReassign = () => {
    setReassigningId(null);
    setReassignName("");
  };
  const reassignToCluster = (segmentId: string, clusterId: string) => {
    reassign.mutate({ segmentId, clusterId }, { onSuccess: cancelReassign });
  };
  const reassignToName = (segmentId: string) => {
    const trimmed = reassignName.trim();
    if (!trimmed) return;
    reassign.mutate({ segmentId, displayName: trimmed }, { onSuccess: cancelReassign });
  };

  const onRefine = () => {
    if (editedThemCount > 0) {
      setConfirmRefine(true);
      return;
    }
    rediarize.mutate();
  };

  const seekTo = (seconds: number) => {
    const audio = audioRef.current;
    if (!audio) return;
    audio.currentTime = seconds;
    void audio.play();
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
      <SpeakerPanel meetingId={meeting.id} />
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
            {lines.map((line, index) => {
              const active = index === activeIndex;
              const editing = editingId != null && line.id === editingId;
              const canEdit = !recording && !!line.id;
              const className =
                "live-line" +
                (line.kind === "partial" ? " live-line--partial" : "") +
                (active ? " line--active" : "") +
                (index === jumpIndex ? " line--jump" : "") +
                (matchIndices.includes(index) ? " line--match" : "") +
                (index === currentMatch ? " line--match-current" : "");
              return (
                <li
                  key={`${line.stream}:${line.start_s}:${line.kind}`}
                  data-index={index}
                  ref={active ? activeRef : null}
                  className={className}
                  style={{ ["--spk" as string]: `var(${colorVar(line)})` } as CSSProperties}
                  onClick={() => {
                    if (!editing) seekTo(line.start_s);
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
                          if (line.id) saveEdit(line.id);
                        }}
                      >
                        <textarea
                          className="line__edit-input"
                          value={editText}
                          autoFocus
                          aria-label="Edit transcript line"
                          disabled={editSegment.isPending}
                          onChange={(event) => setEditText(event.target.value)}
                          onKeyDown={(event) => {
                            if (event.key === "Escape") cancelEdit();
                          }}
                        />
                        <div className="line__edit-actions">
                          <button
                            type="submit"
                            className="line__save"
                            disabled={editSegment.isPending || editText.trim() === ""}
                          >
                            {editSegment.isPending ? "Saving…" : "Save"}
                          </button>
                          <button
                            type="button"
                            className="line__cancel"
                            disabled={editSegment.isPending}
                            onClick={(event) => {
                              event.stopPropagation();
                              cancelEdit();
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
                          if (line.id) startEdit(line.id, line.text);
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
                            if (line.id) startReassign(line.id);
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
                  {reassigningId === line.id ? (
                    <div
                      className="line__reassign"
                      onClick={(event) => event.stopPropagation()}
                      onKeyDown={(event) => {
                        if (event.key === "Escape") cancelReassign();
                      }}
                    >
                      <div className="line__reassign-title">Reassign to</div>
                      <ul className="line__reassign-list">
                        {(speakers.data?.items ?? []).map((speaker) => (
                          <li key={speaker.id}>
                            <button
                              type="button"
                              className="line__reassign-option"
                              aria-current={speaker.id === line.cluster_id}
                              disabled={reassign.isPending || speaker.id === line.cluster_id}
                              onClick={() => {
                                if (line.id) reassignToCluster(line.id, speaker.id);
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
                          if (line.id) reassignToName(line.id);
                        }}
                      >
                        <input
                          className="line__reassign-input"
                          aria-label="New speaker name"
                          placeholder="New speaker…"
                          list={REASSIGN_SUGGESTIONS_ID}
                          value={reassignName}
                          disabled={reassign.isPending}
                          onChange={(event) => setReassignName(event.target.value)}
                        />
                        <button
                          type="submit"
                          className="line__reassign-save"
                          disabled={reassign.isPending || reassignName.trim() === ""}
                        >
                          {reassign.isPending ? "…" : "Add"}
                        </button>
                      </form>
                      {reassign.isError ? (
                        <div className="line__reassign-error">
                          {(reassign.error as Error).message}
                        </div>
                      ) : null}
                      <button
                        type="button"
                        className="line__reassign-cancel"
                        disabled={reassign.isPending}
                        onClick={cancelReassign}
                      >
                        Cancel
                      </button>
                      <datalist id={REASSIGN_SUGGESTIONS_ID}>
                        {(identities.data?.items ?? []).map((identity) => (
                          <option key={identity.id} value={identity.display_name} />
                        ))}
                      </datalist>
                    </div>
                  ) : null}
                </li>
              );
            })}
            {lines.length === 0 ? (
              <li className="muted">
                {recording
                  ? preparing
                    ? "Preparing transcription (loading models)…"
                    : "Listening…"
                  : "No transcript."}
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

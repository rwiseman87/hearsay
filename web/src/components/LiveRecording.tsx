import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";

import { useKeepRecording, usePauseMeeting, useResumeMeeting, useStopMeeting } from "../api/hooks";
import type { MeetingRead } from "../api/types";
import { formatClock } from "../hooks/clock";
import { useTranscript, type LevelStore, type TranscriptLine } from "../hooks/useTranscript";
import { MyNotesPanel } from "./MyNotesPanel";
import { SpeakerLine } from "./SpeakerLine";

const lineKey = (line: TranscriptLine): string =>
  `${line.stream}:${line.start_s}:${line.kind}`;

// Per-bar shape multipliers so the seven topbar bars form a waveform silhouette rather than a flat
// block; scaled by the live input amplitude. RMS is small for speech, so multiply into a visible range.
const WAVE_WEIGHTS = [0.55, 0.85, 1, 0.7, 0.95, 0.6, 0.8];
const WAVE_GAIN = 6;

// Seven topbar bars whose heights track the live input level (RMS over the WS `level` frames).
// Subscribes to the level store directly, so it re-renders on level updates without touching the
// transcript. Flat and low until audio flows, and flattened while paused.
function Waveform({ levels, paused }: { levels: LevelStore; paused: boolean }) {
  const rms = useSyncExternalStore(levels.subscribe, levels.getSnapshot, levels.getSnapshot);
  const amp = paused ? 0 : Math.min(1, rms * WAVE_GAIN);
  return (
    <div className={"live-wave" + (paused ? " live-wave--paused" : "")} aria-hidden="true">
      {WAVE_WEIGHTS.map((weight, i) => (
        <span
          key={i}
          className="live-wave__bar"
          style={{ transform: `scaleY(${Math.max(0.16, amp * weight)})` }}
        />
      ))}
    </div>
  );
}

interface Props {
  meeting: MeetingRead;
}

// The live recording screen (design direction 1a "Signal"): a topbar with the REC pill + elapsed
// timer + waveform + controls, the live transcript as the hero, a quiet AI footer strip, and a notes
// panel on the right. Consumes the same live transcript stream as the finalized detail view.
export function LiveRecording({ meeting }: Props) {
  const stop = useStopMeeting();
  const keepRecording = useKeepRecording();
  const pause = usePauseMeeting();
  const resume = useResumeMeeting();
  const {
    lines,
    connection,
    preparing,
    inactivityPrompt,
    micSilent,
    dismissInactivityPrompt,
    levels,
    paused,
    pausedMs,
    pausedSince,
  } = useTranscript(meeting);

  // Live timer that stays in step with the server's gap-free timeline: it ticks each second, subtracts
  // the total paused span, and freezes at the moment the current pause began.
  const [nowMs, setNowMs] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNowMs(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  const startedMs = useMemo(() => new Date(meeting.started_at).getTime(), [meeting.started_at]);
  const clockMs = paused && pausedSince !== null ? pausedSince : nowMs;
  const elapsed = Math.max(0, Math.floor((clockMs - startedMs - pausedMs) / 1000));

  const linesRef = useRef<HTMLOListElement>(null);
  // Follow the newest line unless the user has scrolled up to read history (cleared by onScroll).
  const pinnedToBottom = useRef(true);

  useEffect(() => {
    pinnedToBottom.current = true;
  }, [meeting.id]);

  useEffect(() => {
    const el = linesRef.current;
    if (el && pinnedToBottom.current) el.scrollTop = el.scrollHeight;
  }, [lines]);

  const speakerCount = useMemo(() => {
    const set = new Set<string>();
    for (const line of lines) if (line.stream === "them") set.add(line.speaker_label);
    return set.size;
  }, [lines]);

  const meta =
    speakerCount > 0 ? `${speakerCount} speaker${speakerCount === 1 ? "" : "s"} · recording` : "Recording";

  return (
    <section className="live">
      <div className="live__main">
        <header className="live__topbar">
          <div className={"live__rec" + (paused ? " live__rec--paused" : "")}>
            <span className="live__rec-dot" aria-hidden="true" />
            <span className="live__rec-label">{paused ? "PAUSED" : "REC"}</span>
            <span className="live__rec-time">{formatClock(elapsed)}</span>
          </div>
          <div className="live__title">
            <div className="live__title-name" title={meeting.title}>
              {meeting.title}
            </div>
            <div className="live__title-meta">{meta}</div>
          </div>
          <Waveform levels={levels} paused={paused} />
          <button
            type="button"
            className="live__ghost"
            onClick={() => (paused ? resume.mutate(meeting.id) : pause.mutate(meeting.id))}
            disabled={pause.isPending || resume.isPending}
            title={paused ? "Resume recording" : "Pause recording"}
          >
            {paused ? "Resume" : "Pause"}
          </button>
          <button
            type="button"
            className="live__end"
            onClick={() => stop.mutate(meeting.id)}
            disabled={stop.isPending}
          >
            {stop.isPending ? "Ending…" : "End"}
          </button>
        </header>

        {micSilent ? (
          <div className="inactivity-banner" role="alert">
            <span className="inactivity-banner__text">
              Your microphone is not being heard — it is sending silence. Check that it is not muted
              and that the right input device is selected. Anything transcribed as "Me" until this
              clears is unreliable.
            </span>
          </div>
        ) : null}
        {inactivityPrompt ? (
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
                {stop.isPending ? "Ending…" : "End now"}
              </button>
            </div>
          </div>
        ) : null}
        {connection && connection !== "open" ? (
          <p className="live__status" role="status">
            {connection === "reconnecting"
              ? "Reconnecting to the live transcript…"
              : "Connecting to the live transcript…"}
          </p>
        ) : null}

        <div className="live__transcript">
          <div className="live__transcript-label">LIVE TRANSCRIPT</div>
          <ol
            className="live__lines"
            ref={linesRef}
            onScroll={(event) => {
              const el = event.currentTarget;
              pinnedToBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
            }}
          >
            {lines.map((line, index) => (
              <SpeakerLine key={lineKey(line)} line={line} newest={index === lines.length - 1} />
            ))}
            {lines.length === 0 ? (
              <li className="live__empty muted">
                {preparing ? "Preparing transcription (loading models)…" : "Listening…"}
              </li>
            ) : null}
          </ol>
          <div className="live__fade" aria-hidden="true" />
        </div>

        <div className="live__ai">
          <span className="live__ai-label">AI</span>
          <span className="live__ai-hint">
            Notes can be generated after the meeting ends.
          </span>
        </div>
        {stop.isError ? (
          <p className="live__error" role="alert">
            Could not end the meeting: {(stop.error as Error).message}
          </p>
        ) : null}
      </div>

      <MyNotesPanel key={meeting.id} meetingId={meeting.id} />
    </section>
  );
}

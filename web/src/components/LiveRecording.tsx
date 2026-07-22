import { useEffect, useMemo, useRef } from "react";

import { useKeepRecording, useStopMeeting } from "../api/hooks";
import type { MeetingRead } from "../api/types";
import { formatClock, useElapsed } from "../hooks/useElapsed";
import { useTranscript, type TranscriptLine } from "../hooks/useTranscript";
import { SpeakerLine } from "./SpeakerLine";

const lineKey = (line: TranscriptLine): string =>
  `${line.stream}:${line.start_s}:${line.kind}`;

// Seven decorative equalizer bars in the topbar. A real amplitude waveform replaces this in Phase 3
// (the helper already emits per-stream RMS; it is not yet forwarded to the browser).
function Waveform() {
  return (
    <div className="live-wave" aria-hidden="true">
      {Array.from({ length: 7 }, (_, i) => (
        <span key={i} className="live-wave__bar" />
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
  const { lines, connection, preparing, inactivityPrompt, micSilent, dismissInactivityPrompt } =
    useTranscript(meeting);
  const elapsed = useElapsed(meeting.started_at);

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
          <div className="live__rec">
            <span className="live__rec-dot" aria-hidden="true" />
            <span className="live__rec-label">REC</span>
            <span className="live__rec-time">{formatClock(elapsed)}</span>
          </div>
          <div className="live__title">
            <div className="live__title-name" title={meeting.title}>
              {meeting.title}
            </div>
            <div className="live__title-meta">{meta}</div>
          </div>
          <Waveform />
          <button
            type="button"
            className="live__ghost"
            disabled
            title="Pause is coming in a later update"
          >
            Pause
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
            Highlights and action items appear after the meeting ends.
          </span>
        </div>
        {stop.isError ? (
          <p className="live__error" role="alert">
            Could not end the meeting: {(stop.error as Error).message}
          </p>
        ) : null}
      </div>

      <aside className="live__notes">
        <div className="live__notes-head">
          <span className="live__notes-title">My notes</span>
        </div>
        <div className="live__notes-empty muted">No notes yet.</div>
        <div className="live__notes-foot">
          <span className="live__chip" aria-disabled="true">
            + Bookmark
          </span>
          <span className="live__chip" aria-disabled="true">
            @ Mention
          </span>
        </div>
      </aside>
    </section>
  );
}

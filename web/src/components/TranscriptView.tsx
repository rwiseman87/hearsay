import { useRediarize, useStopMeeting } from "../api/hooks";
import type { MeetingRead } from "../api/types";
import { useTranscript } from "../hooks/useTranscript";
import { SpeakerPanel } from "./SpeakerPanel";

function formatTime(seconds: number): string {
  const whole = Math.max(0, Math.floor(seconds));
  const minutes = Math.floor(whole / 60)
    .toString()
    .padStart(2, "0");
  const secs = (whole % 60).toString().padStart(2, "0");
  return `${minutes}:${secs}`;
}

interface Props {
  meeting: MeetingRead | null;
}

export function TranscriptView({ meeting }: Props) {
  const stop = useStopMeeting();
  const rediarize = useRediarize(meeting?.id ?? "");
  const lines = useTranscript(meeting);

  if (!meeting) {
    return (
      <section className="transcript transcript--empty">
        <p className="muted">Start a meeting or pick one from the list.</p>
      </section>
    );
  }

  const recording = meeting.status === "recording";

  return (
    <section className="transcript">
      <header className="transcript__header">
        <h2>{meeting.title}</h2>
        {recording ? (
          <button type="button" onClick={() => stop.mutate(meeting.id)} disabled={stop.isPending}>
            {stop.isPending ? "Stopping…" : "Stop"}
          </button>
        ) : (
          <div className="transcript__actions">
            <span className="badge badge--finalized">finalized</span>
            <button type="button" onClick={() => rediarize.mutate()} disabled={rediarize.isPending}>
              {rediarize.isPending ? "Refining…" : "Refine speakers"}
            </button>
          </div>
        )}
      </header>
      {rediarize.isError ? (
        <p className="transcript__error" role="alert">
          {(rediarize.error as Error).message}
        </p>
      ) : null}
      <SpeakerPanel meetingId={meeting.id} />
      <ol className="transcript__lines">
        {lines.map((line) => (
          <li
            key={`${line.stream}:${line.start_s}:${line.kind}`}
            className={`line line--${line.stream}${line.kind === "partial" ? " line--partial" : ""}`}
          >
            <span className="line__time">{formatTime(line.start_s)}</span>
            <span className="line__speaker">{line.speaker_label}</span>
            <span className="line__text">{line.text}</span>
          </li>
        ))}
        {lines.length === 0 ? (
          <li className="muted">{recording ? "Listening…" : "No transcript."}</li>
        ) : null}
      </ol>
    </section>
  );
}

import { useEffect, useMemo, useRef, useState } from "react";

import { useRediarize, useStopMeeting } from "../api/hooks";
import { getToken } from "../api/token";
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

  const audioRef = useRef<HTMLAudioElement>(null);
  const activeRef = useRef<HTMLLIElement>(null);
  const [currentTime, setCurrentTime] = useState(0);
  const [hasAudio, setHasAudio] = useState(true);

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

  // Reset playback state when switching meetings.
  useEffect(() => {
    setCurrentTime(0);
    setHasAudio(true);
  }, [meeting?.id]);

  // Keep the highlighted line in view as playback advances.
  useEffect(() => {
    activeRef.current?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  if (!meeting) {
    return (
      <section className="transcript transcript--empty">
        <p className="muted">Start a meeting or pick one from the list.</p>
      </section>
    );
  }

  const recording = meeting.status === "recording";
  const audioUrl = `/api/meetings/${meeting.id}/audio?token=${encodeURIComponent(getToken())}`;

  const seekTo = (seconds: number) => {
    const audio = audioRef.current;
    if (!audio) return;
    audio.currentTime = seconds;
    void audio.play();
  };

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
      {!recording && hasAudio ? (
        <audio
          ref={audioRef}
          className="transcript__audio"
          src={audioUrl}
          controls
          preload="metadata"
          onTimeUpdate={(event) => setCurrentTime(event.currentTarget.currentTime)}
          onError={() => setHasAudio(false)}
        />
      ) : null}
      {rediarize.isError ? (
        <p className="transcript__error" role="alert">
          {(rediarize.error as Error).message}
        </p>
      ) : null}
      <SpeakerPanel meetingId={meeting.id} />
      <ol className="transcript__lines">
        {lines.map((line, index) => {
          const active = index === activeIndex;
          return (
            <li
              key={`${line.stream}:${line.start_s}:${line.kind}`}
              ref={active ? activeRef : null}
              className={
                `line line--${line.stream}` +
                (line.kind === "partial" ? " line--partial" : "") +
                (active ? " line--active" : "")
              }
              onClick={() => seekTo(line.start_s)}
              title="Jump to this moment"
            >
              <span className="line__time">{formatTime(line.start_s)}</span>
              <span className="line__speaker">{line.speaker_label}</span>
              <span className="line__text">{line.text}</span>
            </li>
          );
        })}
        {lines.length === 0 ? (
          <li className="muted">{recording ? "Listening…" : "No transcript."}</li>
        ) : null}
      </ol>
    </section>
  );
}

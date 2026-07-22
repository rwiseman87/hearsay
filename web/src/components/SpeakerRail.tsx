import { useMemo } from "react";

import type { TranscriptLine } from "../hooks/useTranscript";
import { speakerColorVar } from "./SpeakerLine";

interface RailSpeaker {
  stream: string;
  label: string;
}

interface Props {
  lines: TranscriptLine[];
}

// The 1d "Command" speaker legend: the meeting's roster derived from the live transcript, colored to
// match the transcript avatars, with a pulsing dot on whoever spoke most recently. Read-only during
// recording (renaming is the post-meeting "Refine speakers" flow); the dictionary is a placeholder
// until a custom-vocabulary feature exists.
export function SpeakerRail({ lines }: Props) {
  // Distinct speakers in first-seen order. The pre-diarization "Them" partial label is not a real
  // speaker yet, so it never becomes a roster entry.
  const roster = useMemo(() => {
    const seen = new Map<string, RailSpeaker>();
    for (const line of lines) {
      if (line.stream === "them" && line.speaker_label === "Them") continue;
      const key = `${line.stream}:${line.speaker_label}`;
      if (!seen.has(key)) seen.set(key, { stream: line.stream, label: line.speaker_label });
    }
    return [...seen.values()];
  }, [lines]);

  // The most recent line's speaker is "currently speaking" (a generic Them partial matches no entry).
  const speakingKey = useMemo(() => {
    const last = lines[lines.length - 1];
    if (!last || (last.stream === "them" && last.speaker_label === "Them")) return null;
    return `${last.stream}:${last.speaker_label}`;
  }, [lines]);

  return (
    <aside className="spk-rail">
      <div className="spk-rail__label">SPEAKERS</div>
      <ul className="spk-rail__list">
        {roster.length === 0 ? <li className="spk-rail__empty muted">No speakers yet</li> : null}
        {roster.map((speaker) => {
          const key = `${speaker.stream}:${speaker.label}`;
          return (
            <li
              key={key}
              className="spk-rail__row"
              style={{ ["--spk" as string]: `var(${speakerColorVar(speaker.stream, speaker.label)})` }}
            >
              <span className="spk-rail__dot" aria-hidden="true" />
              <span className="spk-rail__name">{speaker.label}</span>
              {speaker.stream === "me" ? <span className="spk-rail__tag">you</span> : null}
              {key === speakingKey ? (
                <span className="spk-rail__speaking" aria-label="speaking" />
              ) : null}
            </li>
          );
        })}
      </ul>
      <div className="spk-rail__label spk-rail__label--sub">DICTIONARY</div>
      <span className="spk-rail__dict" aria-disabled="true" title="Custom vocabulary — not yet available">
        + add
      </span>
    </aside>
  );
}

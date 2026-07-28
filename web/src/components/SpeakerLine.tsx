import { memo } from "react";

import type { TranscriptLine } from "../hooks/useTranscript";
import { formatClock } from "../hooks/clock";
import { speakerColorVar } from "./speakerColors";

// The CSS variable name for a line's speaker color. Me is fixed; each Them speaker rotates
// deterministically by a hash of its label, so it keeps one color for the whole meeting.
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

interface Props {
  line: TranscriptLine;
  // The most recent line in the stream: rendered brighter and capped with a blinking caret.
  newest: boolean;
}

// One transcript row in the live view: a colored initials avatar, a header (name +
// meeting-relative timestamp), and the body. The newest line ends in a blinking caret.
//
// Memoized: the reducer preserves each unchanged final's object identity across events, so on a live
// partial only the <=2 changed rows (the partial itself and the row losing/gaining `newest`) re-render
// instead of reconciling the whole meeting. Load-bearing for long-meeting live-transcript performance.
export const SpeakerLine = memo(function SpeakerLine({ line, newest }: Props) {
  const className =
    "live-line" +
    (line.kind === "partial" ? " live-line--partial" : "") +
    (newest ? " live-line--newest" : "");
  return (
    <li className={className} style={{ ["--spk" as string]: `var(${colorVar(line)})` }}>
      <span className="live-line__avatar" aria-hidden="true">
        {initials(line.speaker_label)}
      </span>
      <div className="live-line__body">
        <div className="live-line__head">
          <span className="live-line__name">{line.speaker_label}</span>
          <span className="live-line__time">{formatClock(line.start_s)}</span>
        </div>
        <div className="live-line__text">
          {line.text}
          {newest ? <span className="live-line__caret" aria-hidden="true" /> : null}
        </div>
      </div>
    </li>
  );
});

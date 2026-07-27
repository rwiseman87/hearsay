// Rotating "Them" speaker colors (CSS custom properties defined in index.css). "Me" gets the
// dedicated --me color; every remote speaker maps to one of these by a stable hash of its label, so a
// given speaker keeps the same color for the whole meeting. Shared by SpeakerLine (transcript rows)
// and SpeakerPanel (the speaker chips) so a speaker's dot never desyncs from their lines.
const SPEAKER_COLORS = ["--spk-1", "--spk-2", "--spk-3", "--spk-4"] as const;

function hash(text: string): number {
  let acc = 0;
  for (let i = 0; i < text.length; i++) acc = (acc * 31 + text.charCodeAt(i)) >>> 0;
  return acc;
}

// The CSS variable name for a Them speaker's color, chosen deterministically from its label.
export function speakerColorVar(label: string): string {
  return SPEAKER_COLORS[hash(label) % SPEAKER_COLORS.length];
}

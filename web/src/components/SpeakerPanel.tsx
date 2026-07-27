import { type CSSProperties, type FormEvent, useState } from "react";

import { useIdentities, useRenameSpeaker, useSpeakers } from "../api/hooks";
import type { SpeakerRead } from "../api/types";
import { speakerColorVar } from "./speakerColors";

const SUGGESTIONS_ID = "identity-suggestions";

function SpeakerChip({ meetingId, speaker }: { meetingId: string; speaker: SpeakerRead }) {
  const rename = useRenameSpeaker(meetingId);
  const [editing, setEditing] = useState(false);
  const [name, setName] = useState("");

  const startEdit = () => {
    rename.reset();
    setName("");
    setEditing(true);
  };
  const submit = (event: FormEvent) => {
    event.preventDefault();
    const trimmed = name.trim();
    if (trimmed === "") {
      setEditing(false);
      return;
    }
    rename.mutate(
      { clusterId: speaker.id, displayName: trimmed },
      { onSuccess: () => setEditing(false) },
    );
  };

  return (
    <li
      className="speaker-chip"
      style={{ ["--spk" as string]: `var(${speakerColorVar(speaker.label)})` } as CSSProperties}
    >
      <span className="speaker-chip__dot" aria-hidden="true" />
      {editing ? (
        <form className="speaker-chip__form" onSubmit={submit}>
          <input
            className="speaker-chip__input"
            aria-label={`Rename ${speaker.label}`}
            list={SUGGESTIONS_ID}
            placeholder="name…"
            autoFocus
            value={name}
            disabled={rename.isPending}
            onChange={(event) => setName(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") setEditing(false);
            }}
          />
          <button
            type="submit"
            className="speaker-chip__save"
            disabled={rename.isPending || name.trim() === ""}
          >
            {rename.isPending ? "…" : "Save"}
          </button>
        </form>
      ) : (
        <button
          type="button"
          className="speaker-chip__name"
          onClick={startEdit}
          title={`Rename ${speaker.label}`}
        >
          {speaker.label}
        </button>
      )}
    </li>
  );
}

// A readable, editable speaker strip above the transcript: one pill per diarized Them speaker (a
// color-matched dot + the current label). Clicking a label renames that speaker, which relabels their
// transcript lines and pre-seeds the name for the next meeting.
export function SpeakerPanel({ meetingId }: { meetingId: string }) {
  const speakers = useSpeakers(meetingId);
  const identities = useIdentities();
  const items = speakers.data?.items ?? [];

  if (items.length === 0) {
    return null;
  }

  return (
    <section className="speakers-bar" aria-label="Speakers">
      <span className="speakers-bar__label">Speakers</span>
      <ul className="speakers-bar__list">
        {items.map((speaker) => (
          <SpeakerChip key={speaker.id} meetingId={meetingId} speaker={speaker} />
        ))}
      </ul>
      <datalist id={SUGGESTIONS_ID}>
        {(identities.data?.items ?? []).map((identity) => (
          <option key={identity.id} value={identity.display_name} />
        ))}
      </datalist>
    </section>
  );
}

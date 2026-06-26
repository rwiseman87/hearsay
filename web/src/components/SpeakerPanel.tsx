import { type FormEvent, useState } from "react";

import { useIdentities, useRenameSpeaker, useSpeakers } from "../api/hooks";
import type { SpeakerRead } from "../api/types";

interface Props {
  meetingId: string;
}

const SUGGESTIONS_ID = "identity-suggestions";

function SpeakerRow({ meetingId, speaker }: { meetingId: string; speaker: SpeakerRead }) {
  const [name, setName] = useState("");
  const rename = useRenameSpeaker(meetingId);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const trimmed = name.trim();
    if (trimmed !== "") {
      rename.mutate({ clusterId: speaker.id, displayName: trimmed });
    }
  };

  return (
    <li className="speaker">
      <span className="speaker__label">{speaker.label}</span>
      <form className="speaker__rename" onSubmit={submit}>
        <input
          aria-label={`Rename ${speaker.label}`}
          list={SUGGESTIONS_ID}
          placeholder="name…"
          value={name}
          onChange={(event) => setName(event.target.value)}
        />
        <button type="submit" disabled={rename.isPending || name.trim() === ""}>
          {rename.isPending ? "…" : "Rename"}
        </button>
      </form>
    </li>
  );
}

// Lists a meeting's diarized Them speakers and lets the user name each one; renaming
// relabels that speaker's transcript lines (and pre-seeds the name for next meeting).
export function SpeakerPanel({ meetingId }: Props) {
  const speakers = useSpeakers(meetingId);
  const identities = useIdentities();
  const items = speakers.data?.items ?? [];

  if (items.length === 0) {
    return null;
  }

  return (
    <section className="speakers">
      <h3 className="speakers__title">Speakers</h3>
      <ul className="speakers__list">
        {items.map((speaker) => (
          <SpeakerRow key={speaker.id} meetingId={meetingId} speaker={speaker} />
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

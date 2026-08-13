import { type CSSProperties, type FormEvent, useState } from "react";

import { useIdentities, useMergeSpeakers, useRenameSpeaker, useSpeakers } from "../api/hooks";
import type { SpeakerRead } from "../api/types";
import { speakerColorVar } from "./speakerColors";

const SUGGESTIONS_ID = "identity-suggestions";

// The filter key for the microphone channel. Me lines have no cluster, so they need a key of their
// own; a uuid can never collide with it.
export const ME_FILTER_KEY = "me";

// The inline "name this speaker" field, shared by the live and finalized chips. The `aria-label` and
// the save button's class are part of the e2e contract — don't rename them.
function RenameForm({
  label,
  pending,
  onSubmit,
  onCancel,
}: {
  label: string;
  pending: boolean;
  onSubmit: (name: string) => void;
  onCancel: () => void;
}) {
  const [name, setName] = useState("");

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const trimmed = name.trim();
    if (trimmed === "") {
      onCancel();
      return;
    }
    onSubmit(trimmed);
  };

  return (
    <form className="speaker-chip__form" onSubmit={submit}>
      <input
        className="speaker-chip__input"
        aria-label={`Rename ${label}`}
        list={SUGGESTIONS_ID}
        placeholder="name…"
        autoFocus
        value={name}
        disabled={pending}
        onChange={(event) => setName(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Escape") onCancel();
        }}
      />
      <button
        type="submit"
        className="speaker-chip__save"
        disabled={pending || name.trim() === ""}
      >
        {pending ? "…" : "Save"}
      </button>
    </form>
  );
}

// While recording, a chip only renames: the diarizer is still splitting and merging clusters
// underneath us, so filtering and merging are finalized-only.
function LiveSpeakerChip({ meetingId, speaker }: { meetingId: string; speaker: SpeakerRead }) {
  const rename = useRenameSpeaker(meetingId);
  const [editing, setEditing] = useState(false);

  return (
    <li
      className="speaker-chip"
      style={{ ["--spk" as string]: `var(${speakerColorVar(speaker.label)})` } as CSSProperties}
    >
      <span className="speaker-chip__dot" aria-hidden="true" />
      {editing ? (
        <RenameForm
          label={speaker.label}
          pending={rename.isPending}
          onCancel={() => setEditing(false)}
          onSubmit={(name) =>
            rename.mutate(
              { clusterId: speaker.id, displayName: name },
              { onSuccess: () => setEditing(false) },
            )
          }
        />
      ) : (
        <button
          type="button"
          className="speaker-chip__name"
          onClick={() => {
            rename.reset();
            setEditing(true);
          }}
          title={`Rename ${speaker.label}`}
        >
          {speaker.label}
        </button>
      )}
    </li>
  );
}

// What the chip's popover is showing. `merge` lists the other speakers; picking one moves to the
// confirm step, since a merge relabels many lines at once and only a full re-diarize undoes it.
type ChipMode = "closed" | "menu" | "rename" | "merge";

function SpeakerChip({
  meetingId,
  speaker,
  others,
  selected,
  onToggle,
}: {
  meetingId: string;
  speaker: SpeakerRead;
  others: SpeakerRead[];
  selected: boolean;
  onToggle: (key: string) => void;
}) {
  const rename = useRenameSpeaker(meetingId);
  const merge = useMergeSpeakers(meetingId);
  const [mode, setMode] = useState<ChipMode>("closed");
  const [target, setTarget] = useState<SpeakerRead | null>(null);

  const close = () => {
    setMode("closed");
    setTarget(null);
  };

  return (
    <li
      className={`speaker-chip${selected ? " is-active" : ""}`}
      style={{ ["--spk" as string]: `var(${speakerColorVar(speaker.label)})` } as CSSProperties}
      onKeyDown={(event) => {
        if (event.key === "Escape") close();
      }}
    >
      {mode === "rename" ? (
        <>
          <span className="speaker-chip__dot" aria-hidden="true" />
          <RenameForm
            label={speaker.label}
            pending={rename.isPending}
            onCancel={close}
            onSubmit={(name) =>
              rename.mutate({ clusterId: speaker.id, displayName: name }, { onSuccess: close })
            }
          />
        </>
      ) : (
        <>
          {/* The accessible name stays exactly the label: the filter affordance is carried by
              aria-pressed and the tooltip, not by decorating the name. */}
          <button
            type="button"
            className="speaker-chip__toggle"
            aria-pressed={selected}
            title={selected ? `Stop showing only ${speaker.label}` : `Show only ${speaker.label}`}
            onClick={() => onToggle(speaker.id)}
          >
            <span className="speaker-chip__dot" aria-hidden="true" />
            <span className="speaker-chip__name">{speaker.label}</span>
          </button>
          <button
            type="button"
            className="speaker-chip__more"
            aria-label={`Speaker actions for ${speaker.label}`}
            aria-expanded={mode !== "closed"}
            onClick={() => {
              rename.reset();
              merge.reset();
              setMode(mode === "closed" ? "menu" : "closed");
            }}
          >
            <span aria-hidden="true">⋯</span>
          </button>
        </>
      )}

      {mode === "menu" ? (
        <div className="speaker-chip__pop">
          <div className="line__reassign-title">{speaker.label}</div>
          <ul className="line__reassign-list">
            <li>
              <button
                type="button"
                className="line__reassign-option"
                onClick={() => setMode("rename")}
              >
                Rename
              </button>
            </li>
            <li>
              <button
                type="button"
                className="line__reassign-option"
                disabled={others.length === 0}
                onClick={() => setMode("merge")}
              >
                Merge into…
              </button>
            </li>
          </ul>
          <button type="button" className="line__reassign-cancel" onClick={close}>
            Cancel
          </button>
        </div>
      ) : null}

      {mode === "merge" ? (
        <div className="speaker-chip__pop">
          {target ? (
            <>
              <div className="line__reassign-title">Merge into {target.label}?</div>
              <p className="speaker-chip__warn">
                Every {speaker.label} line becomes {target.label}, and the voice sample stored for{" "}
                {speaker.label} in this meeting is discarded. Only a re-diarize undoes this.
              </p>
              <div className="speaker-chip__confirm">
                <button
                  type="button"
                  className="speaker-chip__save"
                  disabled={merge.isPending}
                  onClick={() =>
                    merge.mutate(
                      { clusterId: speaker.id, into: target.id },
                      { onSuccess: close },
                    )
                  }
                >
                  {merge.isPending ? "Merging…" : "Merge"}
                </button>
                <button
                  type="button"
                  className="line__reassign-cancel"
                  disabled={merge.isPending}
                  onClick={() => setTarget(null)}
                >
                  Back
                </button>
              </div>
              {merge.isError ? (
                <p className="speaker-chip__error" role="alert">
                  {(merge.error as Error).message}
                </p>
              ) : null}
            </>
          ) : (
            <>
              <div className="line__reassign-title">Merge {speaker.label} into</div>
              <ul className="line__reassign-list">
                {others.map((other) => (
                  <li key={other.id}>
                    <button
                      type="button"
                      className="line__reassign-option"
                      onClick={() => setTarget(other)}
                    >
                      {other.label}
                    </button>
                  </li>
                ))}
              </ul>
              <button type="button" className="line__reassign-cancel" onClick={close}>
                Cancel
              </button>
            </>
          )}
        </div>
      ) : null}
    </li>
  );
}

// The speaker strip above the transcript: one pill per diarized Them speaker (a color-matched dot +
// the current label), plus Me once the meeting is finalized. A pill both filters the transcript to
// that speaker and, behind its ⋯ menu, renames or merges them. While recording it only renames.
export function SpeakerPanel({
  meetingId,
  live,
  selected,
  hasMe,
  onToggle,
  onClear,
  visibleCount,
  totalCount,
}: {
  meetingId: string;
  live: boolean;
  selected: ReadonlySet<string>;
  hasMe: boolean;
  onToggle: (key: string) => void;
  onClear: () => void;
  visibleCount: number;
  totalCount: number;
}) {
  const speakers = useSpeakers(meetingId, live);
  const identities = useIdentities();
  const items = speakers.data?.items ?? [];

  if (items.length === 0) {
    return null;
  }

  const filtering = selected.size > 0;

  return (
    <section className="speakers-bar" aria-label="Speakers">
      <span className="speakers-bar__label">Speakers</span>
      <ul className="speakers-bar__list">
        {items.map((speaker) =>
          live ? (
            <LiveSpeakerChip key={speaker.id} meetingId={meetingId} speaker={speaker} />
          ) : (
            <SpeakerChip
              key={speaker.id}
              meetingId={meetingId}
              speaker={speaker}
              others={items.filter((other) => other.id !== speaker.id)}
              selected={selected.has(speaker.id)}
              onToggle={onToggle}
            />
          ),
        )}
        {!live && hasMe ? (
          <li
            className={`speaker-chip${selected.has(ME_FILTER_KEY) ? " is-active" : ""}`}
            style={{ ["--spk" as string]: "var(--me)" } as CSSProperties}
          >
            <button
              type="button"
              className="speaker-chip__toggle"
              aria-pressed={selected.has(ME_FILTER_KEY)}
              title={selected.has(ME_FILTER_KEY) ? "Stop showing only Me" : "Show only Me"}
              onClick={() => onToggle(ME_FILTER_KEY)}
            >
              <span className="speaker-chip__dot" aria-hidden="true" />
              <span className="speaker-chip__name">Me</span>
            </button>
          </li>
        ) : null}
      </ul>
      {filtering ? (
        <>
          <span className="speakers-bar__count" aria-live="polite">
            {visibleCount} of {totalCount} lines
          </span>
          <button type="button" className="speakers-bar__clear" onClick={onClear}>
            Clear filter
          </button>
        </>
      ) : null}
      <datalist id={SUGGESTIONS_ID}>
        {(identities.data?.items ?? []).map((identity) => (
          <option key={identity.id} value={identity.display_name} />
        ))}
      </datalist>
    </section>
  );
}

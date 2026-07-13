import { useEffect, useState } from "react";

import { useSettings, useUpdateRecording, useUpdateSpeakers } from "../api/hooks";
import type { SpeakerSettings } from "../api/types";

interface Props {
  onClose: () => void;
}

// Panels land here as they are built; the nav + content shell already supports more than one.
const PANELS = [
  { id: "recording", label: "Recording & Privacy" },
  { id: "speakers", label: "Speakers" },
] as const;
type PanelId = (typeof PANELS)[number]["id"];

function RecordingPanel() {
  const settings = useSettings();
  const update = useUpdateRecording();
  const record = settings.data?.recording.record ?? true;

  return (
    <div className="settings__panel">
      <h3 className="settings__panel-title">Recording &amp; Privacy</h3>
      {settings.isLoading ? (
        <p className="muted">Loading…</p>
      ) : (
        <label className="settings__row">
          <input
            type="checkbox"
            checked={record}
            disabled={update.isPending || settings.isError}
            onChange={(event) => update.mutate({ record: event.target.checked })}
          />
          <span className="settings__row-body">
            <span className="settings__row-label">Keep meeting audio</span>
            <span className="settings__row-hint muted">
              Records one WAV per meeting for playback and the post-meeting refine. Turn off to
              retain no raw audio. Applies to your next meeting.
            </span>
          </span>
        </label>
      )}
      {update.isError ? (
        <p className="settings__error" role="alert">
          {(update.error as Error).message}
        </p>
      ) : null}
    </div>
  );
}

function SpeakersPanel() {
  const settings = useSettings();
  const update = useUpdateSpeakers();
  const speakers = settings.data?.speakers;
  // Local slider value so dragging is smooth; the mutation commits only on release.
  const [threshold, setThreshold] = useState(0.6);

  useEffect(() => {
    if (speakers) setThreshold(speakers.recognition_threshold);
  }, [speakers?.recognition_threshold]);

  if (settings.isLoading || !speakers) return <p className="muted">Loading…</p>;

  const commit = (patch: Partial<SpeakerSettings>) => update.mutate({ ...speakers, ...patch });

  return (
    <div className="settings__panel">
      <h3 className="settings__panel-title">Speakers</h3>
      <label className="settings__row">
        <input
          type="checkbox"
          checked={speakers.auto_refine}
          disabled={update.isPending}
          onChange={(event) => commit({ auto_refine: event.target.checked })}
        />
        <span className="settings__row-body">
          <span className="settings__row-label">Auto-refine speakers</span>
          <span className="settings__row-hint muted">
            Re-diarize each meeting when it finishes for accurate speaker labels. Applies to your
            next meeting.
          </span>
        </span>
      </label>
      <div className="settings__field">
        <span className="settings__row-label">Recognition threshold</span>
        <div className="settings__slider">
          <input
            type="range"
            min={0}
            max={1}
            step={0.05}
            value={threshold}
            disabled={update.isPending}
            aria-label="Recognition threshold"
            onChange={(event) => setThreshold(event.currentTarget.valueAsNumber)}
            onPointerUp={(event) =>
              commit({ recognition_threshold: event.currentTarget.valueAsNumber })
            }
            onKeyUp={(event) =>
              commit({ recognition_threshold: event.currentTarget.valueAsNumber })
            }
          />
          <span className="settings__slider-value">{threshold.toFixed(2)}</span>
        </div>
        <span className="settings__row-hint muted">
          How similar a voice must be to auto-match someone named in a past meeting. Higher is
          stricter.
        </span>
      </div>
      {update.isError ? (
        <p className="settings__error" role="alert">
          {(update.error as Error).message}
        </p>
      ) : null}
    </div>
  );
}

export default function SettingsPage({ onClose }: Props) {
  const [active, setActive] = useState<PanelId>("recording");

  return (
    <div className="settings-overlay" role="dialog" aria-modal="true" aria-label="Settings">
      <button
        type="button"
        className="settings-overlay__backdrop"
        aria-label="Close settings"
        onClick={onClose}
      />
      <div className="settings">
        <header className="settings__header">
          <h2>Settings</h2>
          <button
            type="button"
            className="settings__close"
            aria-label="Close settings"
            onClick={onClose}
          >
            ✕
          </button>
        </header>
        <div className="settings__body">
          <nav className="settings__nav">
            {PANELS.map((panel) => (
              <button
                key={panel.id}
                type="button"
                className={
                  panel.id === active ? "settings__nav-item is-active" : "settings__nav-item"
                }
                onClick={() => setActive(panel.id)}
              >
                {panel.label}
              </button>
            ))}
          </nav>
          <div className="settings__content">
            {active === "recording" ? <RecordingPanel /> : null}
            {active === "speakers" ? <SpeakersPanel /> : null}
          </div>
        </div>
      </div>
    </div>
  );
}

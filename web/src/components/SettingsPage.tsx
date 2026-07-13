import { useState } from "react";

import { useSettings, useUpdateRecording } from "../api/hooks";

interface Props {
  onClose: () => void;
}

// Panels land here as they are built; the nav + content shell already supports more than one.
const PANELS = [{ id: "recording", label: "Recording & Privacy" }] as const;
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
          </div>
        </div>
      </div>
    </div>
  );
}

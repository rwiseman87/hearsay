import { useEffect, useState } from "react";

import {
  useSettings,
  useUpdateRecording,
  useUpdateSpeakers,
  useUpdateStorage,
} from "../api/hooks";
import type { SpeakerSettings } from "../api/types";

interface Props {
  onClose: () => void;
}

// Panels land here as they are built; the nav + content shell already supports more than one.
const PANELS = [
  { id: "recording", label: "Recording & Privacy" },
  { id: "speakers", label: "Speakers" },
  { id: "storage", label: "Storage" },
] as const;
type PanelId = (typeof PANELS)[number]["id"];

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(1)} ${units[unit]}`;
}

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

function StoragePanel() {
  const settings = useSettings();
  const update = useUpdateStorage();
  const storage = settings.data?.storage;
  const info = settings.data?.storage_info;
  const [dir, setDir] = useState("");

  useEffect(() => {
    if (storage) setDir(storage.output_dir);
  }, [storage?.output_dir]);

  if (settings.isLoading || !storage || !info) return <p className="muted">Loading…</p>;

  const onSave = () => {
    const trimmed = dir.trim();
    if (trimmed) update.mutate({ output_dir: trimmed });
  };

  return (
    <div className="settings__panel">
      <h3 className="settings__panel-title">Storage</h3>
      <div className="settings__field">
        <span className="settings__row-label">Default recordings location</span>
        <div className="settings__inline">
          <input
            value={dir}
            spellCheck={false}
            disabled={update.isPending}
            aria-label="Default recordings location"
            onChange={(event) => setDir(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") onSave();
            }}
          />
          <button
            type="button"
            onClick={onSave}
            disabled={update.isPending || dir.trim() === storage.output_dir}
          >
            {update.isPending ? "Checking…" : "Save"}
          </button>
        </div>
        <span className="settings__row-hint muted">
          Where new meetings are written. Existing meetings keep their location — relocate one
          from its own view. Must be an existing, writable folder.
        </span>
        {update.isError ? (
          <p className="settings__error" role="alert">
            {(update.error as Error).message}
          </p>
        ) : null}
      </div>
      <dl className="settings__facts">
        <div>
          <dt>Recorded data</dt>
          <dd>
            {formatBytes(info.tracked_bytes)} across {info.meeting_count}{" "}
            {info.meeting_count === 1 ? "meeting" : "meetings"}
          </dd>
        </div>
        <div>
          <dt>Database</dt>
          <dd>
            <code>{info.database_path}</code>
            <span className="settings__row-hint muted"> — change via env + restart</span>
          </dd>
        </div>
      </dl>
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
            {active === "storage" ? <StoragePanel /> : null}
          </div>
        </div>
      </div>
    </div>
  );
}

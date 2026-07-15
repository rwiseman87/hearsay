import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { api } from "../api/client";
import {
  usePermissions,
  useResetModels,
  useSettings,
  useUpdateModels,
  useUpdateRecording,
  useUpdateSpeakers,
  useUpdateStorage,
} from "../api/hooks";
import type { SpeakerSettings } from "../api/types";

// The Danger Zone's erase/quit actions are Tauri IPC (desktop shell), not the loopback HTTP API, so
// they only exist in the packaged app. In a plain browser (dev) the shell isn't there.
const IS_DESKTOP = "__TAURI_INTERNALS__" in window;

interface Props {
  onClose: () => void;
}

// Panels land here as they are built; the nav + content shell already supports more than one.
const PANELS = [
  { id: "recording", label: "Recording & Privacy" },
  { id: "speakers", label: "Speakers" },
  { id: "models", label: "Models" },
  { id: "storage", label: "Storage" },
  { id: "permissions", label: "Permissions" },
  { id: "about", label: "About" },
  { id: "danger", label: "Data & Uninstall" },
] as const;
type PanelId = (typeof PANELS)[number]["id"];

// Each permission maps to a label and a one-line rationale. `key` indexes the PermissionsInfo
// status fields. (Users open the matching pane themselves via System Settings > Privacy & Security.)
const PERMISSION_ROWS = [
  {
    key: "microphone",
    label: "Microphone",
    hint: "Captures your voice — the Me stream.",
  },
  {
    key: "audio_capture",
    label: "System audio",
    hint: "Captures the other participants — the Them stream.",
  },
  {
    key: "screen_recording",
    label: "Screen recording",
    hint: "Reads on-screen active-speaker hints (later phase).",
  },
  {
    key: "accessibility",
    label: "Accessibility",
    hint: "Opt-in Zoom active-speaker path (later phase).",
  },
  {
    key: "calendar",
    label: "Calendar",
    hint: "Reads the meeting roster to help label speakers (later phase).",
  },
] as const;

const STATUS_META: Record<string, { label: string; kind: string }> = {
  granted: { label: "Granted", kind: "granted" },
  denied: { label: "Denied", kind: "denied" },
  undetermined: { label: "Not requested", kind: "undetermined" },
  unknown: { label: "Unknown", kind: "unknown" },
};

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

  // Re-sync the slider when the server value changes. Depend on the primitive we actually read, not
  // the settings object (a fresh reference on every fetch).
  const recognitionThreshold = speakers?.recognition_threshold;
  useEffect(() => {
    if (recognitionThreshold !== undefined) setThreshold(recognitionThreshold);
  }, [recognitionThreshold]);

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

function ModelsPanel() {
  const settings = useSettings();
  const update = useUpdateModels();
  const reset = useResetModels();
  const models = settings.data?.models;
  const info = settings.data?.models_info;
  const [path, setPath] = useState("");

  // Re-sync the input when the server value changes; depend on the primitive, not the settings object.
  const refineModel = models?.refine_model;
  useEffect(() => {
    if (refineModel !== undefined) setPath(refineModel);
  }, [refineModel]);

  if (settings.isLoading || !models || !info) return <p className="muted">Loading…</p>;

  const busy = update.isPending || reset.isPending;
  const onSave = () => {
    const trimmed = path.trim();
    if (trimmed) update.mutate({ refine_model: trimmed });
  };

  // Native open-file dialog (desktop only — the shell surfaces it via Tauri IPC). Picking a file
  // applies it immediately; the core still validates the GGML magic before persisting.
  const onBrowse = async () => {
    let picked: string | null;
    try {
      picked = await invoke<string | null>("pick_refine_model");
    } catch {
      return; // picker unavailable — the text input remains the fallback
    }
    if (picked) {
      setPath(picked);
      update.mutate({ refine_model: picked });
    }
  };

  const isDefault = models.refine_model === info.default_refine_model;

  return (
    <div className="settings__panel">
      <h3 className="settings__panel-title">Models</h3>
      <div className="settings__field">
        <span className="settings__row-label">Refine transcription model</span>
        <div className="settings__inline">
          <input
            value={path}
            spellCheck={false}
            disabled={busy}
            aria-label="Refine transcription model path"
            onChange={(event) => setPath(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") onSave();
            }}
          />
          {IS_DESKTOP ? (
            <button type="button" onClick={() => void onBrowse()} disabled={busy}>
              Choose…
            </button>
          ) : null}
          <button type="button" onClick={onSave} disabled={busy || path.trim() === models.refine_model}>
            {update.isPending ? "Checking…" : "Save"}
          </button>
        </div>
        <span className="settings__row-hint muted">
          Absolute path to a downloaded GGML whisper model (a <code>ggml-*.bin</code> file). Used
          only for the post-meeting refine — the higher-accuracy re-transcription behind the “Refine
          speakers” button and auto-refine. Live transcription is unaffected. Applies to your next
          refine.
        </span>
        {!info.refine_model_exists ? (
          <p className="settings__error" role="alert">
            The current model file was not found on disk. Refining will fail until this points at an
            existing model.
          </p>
        ) : null}
        {update.isError ? (
          <p className="settings__error" role="alert">
            {(update.error as Error).message}
          </p>
        ) : null}
      </div>
      <dl className="settings__facts">
        <div>
          <dt>Bundled default</dt>
          <dd>
            <code>{info.default_refine_model}</code>
            {isDefault ? (
              <span className="settings__row-hint muted"> — in use</span>
            ) : (
              <button
                type="button"
                className="settings__link-btn"
                disabled={busy}
                onClick={() => reset.mutate()}
              >
                {reset.isPending ? "Resetting…" : "Reset to default"}
              </button>
            )}
          </dd>
        </div>
      </dl>
    </div>
  );
}

function StoragePanel() {
  const settings = useSettings();
  const update = useUpdateStorage();
  const storage = settings.data?.storage;
  const info = settings.data?.storage_info;
  const [dir, setDir] = useState("");

  // Re-sync the input when the server value changes; depend on the primitive, not the settings object.
  const outputDir = storage?.output_dir;
  useEffect(() => {
    if (outputDir !== undefined) setDir(outputDir);
  }, [outputDir]);

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
          Where new meetings are written. Existing meetings keep their location. Must be an
          existing, writable folder.
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

function PermissionsPanel() {
  const perms = usePermissions();
  const data = perms.data;

  return (
    <div className="settings__panel">
      <h3 className="settings__panel-title">Permissions</h3>
      <div className="settings__perm-bar">
        <span className="muted">
          {perms.isLoading
            ? "Checking the capture helper…"
            : data?.helper_available
              ? `Capture helper v${data.helper_version ?? "?"}`
              : "Capture helper not found — build it to see live status."}
        </span>
        <button type="button" onClick={() => perms.refetch()} disabled={perms.isFetching}>
          {perms.isFetching ? "Checking…" : "Recheck"}
        </button>
      </div>
      {perms.isError ? (
        <p className="settings__error" role="alert">
          {(perms.error as Error).message}
        </p>
      ) : null}
      <div className="settings__perms">
        {PERMISSION_ROWS.map((row) => {
          const status = data ? data[row.key] : "unknown";
          const meta = STATUS_META[status] ?? STATUS_META.unknown;
          return (
            <div key={row.key} className="settings__perm">
              <div className="settings__perm-head">
                <span className="settings__row-label">{row.label}</span>
                <span className={`settings__status settings__status--${meta.kind}`}>
                  {meta.label}
                </span>
              </div>
              <span className="settings__row-hint muted">{row.hint}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function AboutPanel() {
  const settings = useSettings();
  const about = settings.data?.about;

  if (settings.isLoading || !about) return <p className="muted">Loading…</p>;

  return (
    <div className="settings__panel">
      <h3 className="settings__panel-title">About</h3>
      <dl className="settings__facts">
        <div>
          <dt>Version</dt>
          <dd>{about.app_version}</dd>
        </div>
        <div>
          <dt>Environment</dt>
          <dd>{about.environment}</dd>
        </div>
        <div>
          <dt>IPC protocol</dt>
          <dd>v{about.protocol_version}</dd>
        </div>
        <div>
          <dt>Database</dt>
          <dd>
            <code>{about.database_path}</code>
          </dd>
        </div>
      </dl>
    </div>
  );
}

// "Reveal data folder" goes through the core's same-origin HTTP API (the channel the rest of
// Settings uses and which works in the packaged app), NOT Tauri invoke() — the webview runs on the
// core's remote loopback origin, from which custom shell commands are not reliably reachable, which
// is why every invoke()-based reveal did nothing. The core (a native process) opens Finder itself,
// and surfaces the reason on failure. Erase + quit stay Tauri IPC (only the shell can quit the app /
// reset macOS permissions).
function DangerZonePanel() {
  const [confirm, setConfirm] = useState("");
  const [phase, setPhase] = useState<"idle" | "erasing" | "done">("idle");
  const [error, setError] = useState<string | null>(null);

  const reveal = async () => {
    setError(null);
    try {
      await api.post("/api/settings/reveal");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const erase = async () => {
    setError(null);
    setPhase("erasing");
    try {
      await invoke("erase_all_data");
      setPhase("done");
    } catch (err) {
      setPhase("idle");
      setError(String(err));
    }
  };

  return (
    <div className="settings__panel">
      <h3 className="settings__panel-title">Data &amp; Uninstall</h3>
      {!IS_DESKTOP ? (
        <p className="muted">Available in the desktop app.</p>
      ) : phase === "done" ? (
        <div className="settings__done">
          <p>Your recordings, transcripts, caches, and macOS permissions have been erased.</p>
          <span className="settings__row-hint muted">
            To finish uninstalling, quit Hearsay and drag it from Applications to the Trash.
          </span>
          <button
            type="button"
            className="settings__danger-btn"
            onClick={() => void invoke("quit_app")}
          >
            Quit Hearsay
          </button>
        </div>
      ) : (
        <>
          <div className="settings__field">
            <span className="settings__row-label">Keep my recordings</span>
            <span className="settings__row-hint muted">
              Your meetings live outside the app. To uninstall but keep them, just drag Hearsay from
              Applications to the Trash — your recordings and transcripts stay on disk.
            </span>
            <button type="button" className="settings__reveal-btn" onClick={() => void reveal()}>
              Reveal data folder in Finder
            </button>
          </div>
          <div className="settings__danger">
            <span className="settings__row-label">Erase everything</span>
            <span className="settings__row-hint muted">
              Permanently deletes all recordings, transcripts, and the database, clears downloaded
              models and app caches, and resets Hearsay's macOS permissions so a reinstall re-prompts.
              This cannot be undone. Type <code>erase</code> to confirm.
            </span>
            <div className="settings__inline">
              <input
                value={confirm}
                spellCheck={false}
                placeholder="erase"
                aria-label="Type erase to confirm"
                disabled={phase === "erasing"}
                onChange={(event) => setConfirm(event.target.value)}
              />
              <button
                type="button"
                className="settings__danger-btn"
                disabled={confirm.trim().toLowerCase() !== "erase" || phase === "erasing"}
                onClick={() => void erase()}
              >
                {phase === "erasing" ? "Erasing…" : "Erase all data & reset permissions"}
              </button>
            </div>
          </div>
        </>
      )}
      {error ? (
        <p className="settings__error" role="alert">
          {error}
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
            {active === "models" ? <ModelsPanel /> : null}
            {active === "storage" ? <StoragePanel /> : null}
            {active === "permissions" ? <PermissionsPanel /> : null}
            {active === "about" ? <AboutPanel /> : null}
            {active === "danger" ? <DangerZonePanel /> : null}
          </div>
        </div>
      </div>
    </div>
  );
}

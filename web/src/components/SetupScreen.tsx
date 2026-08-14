import { useState } from "react";

import { useModelCatalog, useSetup, useStartSetup } from "../api/hooks";
import type { SetupState, SetupStep } from "../api/types";
import { formatBytes } from "./format";

// First-run model setup. The download is never automatic — it is billions of bytes, so a metered or
// offline first run stays the user's call.

// Progress bar + byte counter for one asset.
function StepRow({ step }: { step: SetupStep }) {
  const done = step.status === "done";
  const value = done ? step.total_bytes : step.downloaded_bytes;
  return (
    <li className="setup__step">
      <div className="setup__step-head">
        <span>{step.label}</span>
        <span className="muted">
          {step.status === "verifying"
            ? "Verifying…"
            : done
              ? formatBytes(step.total_bytes)
              : `${formatBytes(value)} / ${formatBytes(step.total_bytes)}`}
        </span>
      </div>
      <progress aria-label={`${step.label} progress`} value={value} max={step.total_bytes || 1} />
    </li>
  );
}

function totalBytes(setup: SetupState): number {
  return setup.steps.reduce((sum, s) => sum + s.total_bytes, 0);
}

export function SetupScreen() {
  const setup = useSetup();
  const catalog = useModelCatalog();
  const start = useStartSetup();
  // Offered here so notes are one download instead of two, but never assumed.
  const [withNotes, setWithNotes] = useState(false);
  const [notesId, setNotesId] = useState<string | null>(null);

  const state = setup.data;
  if (!state) return null;

  const running = state.status === "running";
  const failed = state.status === "error";
  const items = catalog.data?.items ?? [];
  const recommended = items.find((m) => m.recommended) ?? items[0];
  const selectedNotes = withNotes ? (notesId ?? recommended?.id ?? null) : null;
  const notesBytes =
    selectedNotes != null ? (items.find((m) => m.id === selectedNotes)?.size_bytes ?? 0) : 0;
  const size = totalBytes(state) + (running ? 0 : notesBytes);

  return (
    <div className="setup">
      <div className="setup__card">
        <h1 className="setup__title">Set up Hearsay</h1>
        <p className="setup__lead">
          Hearsay transcribes on this machine, so it needs its speech models before the first
          meeting. This is a one-time download of about {formatBytes(size)}; nothing is uploaded, and
          the models stay on this Mac.
        </p>

        <ul className="setup__steps">
          {state.steps.map((step) => (
            <StepRow key={step.id} step={step} />
          ))}
        </ul>

        {!running && items.length > 0 ? (
          <div className="setup__option">
            <label className="setup__check">
              <input
                type="checkbox"
                checked={withNotes}
                onChange={(e) => setWithNotes(e.target.checked)}
              />
              Also download a model for meeting notes (optional)
            </label>
            {withNotes ? (
              <select
                aria-label="Notes model"
                value={selectedNotes ?? ""}
                onChange={(e) => setNotesId(e.target.value)}
              >
                {items.map((m) => (
                  <option key={m.id} value={m.id}>
                    {m.name} — {formatBytes(m.size_bytes)}
                    {m.recommended ? " (recommended)" : ""}
                  </option>
                ))}
              </select>
            ) : null}
          </div>
        ) : null}

        {failed ? (
          <p className="setup__error" role="alert">
            {state.message ?? "The download failed."} Downloads resume where they stopped, so trying
            again does not start over.
          </p>
        ) : null}

        <div className="setup__actions">
          <button
            type="button"
            className="setup__start"
            onClick={() => start.mutate(selectedNotes)}
            disabled={running || start.isPending}
          >
            {running ? "Downloading…" : failed ? "Try again" : "Download models"}
          </button>
        </div>

        <p className="setup__note muted">
          Needs an internet connection this once. An interrupted download resumes on the next
          attempt, and you can quit and come back.
        </p>
      </div>
    </div>
  );
}

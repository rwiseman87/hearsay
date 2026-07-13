import { useState } from "react";

import { ApiError } from "../api/client";
import { useRelocateMeeting } from "../api/hooks";
import type { MeetingRead } from "../api/types";

interface Props {
  meeting: MeetingRead;
}

// The relocation 422 carries a structured `{ message, missing }` detail; pull out the file list so
// the user sees exactly which artifacts weren't found at the target.
function missingFiles(error: unknown): string[] {
  if (
    error instanceof ApiError &&
    error.detail &&
    typeof error.detail === "object" &&
    "missing" in error.detail
  ) {
    const missing = (error.detail as { missing: unknown }).missing;
    if (Array.isArray(missing)) return missing.map(String);
  }
  return [];
}

function errorMessage(error: unknown): string {
  const missing = missingFiles(error);
  if (missing.length > 0) return `Couldn't find at that location: ${missing.join(", ")}`;
  if (error instanceof ApiError || error instanceof Error) return error.message;
  return String(error);
}

export function RelocateStorage({ meeting }: Props) {
  const [open, setOpen] = useState(false);
  const [root, setRoot] = useState(meeting.storage_root);
  const relocate = useRelocateMeeting(meeting.id);

  const onSave = () => {
    const trimmed = root.trim();
    if (!trimmed) return;
    relocate.mutate(trimmed, { onSuccess: () => setOpen(false) });
  };

  const onCancel = () => {
    relocate.reset();
    setRoot(meeting.storage_root);
    setOpen(false);
  };

  return (
    <div className="relocate">
      <div className="relocate__current">
        <span className="relocate__label">Storage</span>
        <code className="relocate__path" title={meeting.storage_root}>
          {meeting.storage_root}
        </code>
        {!open ? (
          <button type="button" className="relocate__toggle" onClick={() => setOpen(true)}>
            Relocate…
          </button>
        ) : null}
      </div>
      {open ? (
        <div className="relocate__form">
          <input
            value={root}
            aria-label="New storage location (absolute path)"
            placeholder="/absolute/path/the/folder/moved/to"
            spellCheck={false}
            onChange={(event) => setRoot(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") onSave();
              if (event.key === "Escape") onCancel();
            }}
          />
          <button type="button" onClick={onSave} disabled={relocate.isPending}>
            {relocate.isPending ? "Checking…" : "Save"}
          </button>
          <button type="button" className="relocate__cancel" onClick={onCancel}>
            Cancel
          </button>
        </div>
      ) : null}
      <p className="relocate__hint muted">
        The app validates the artifacts are there; it does not move files.
      </p>
      {relocate.isError ? (
        <p className="relocate__error" role="alert">
          {errorMessage(relocate.error)}
        </p>
      ) : null}
    </div>
  );
}

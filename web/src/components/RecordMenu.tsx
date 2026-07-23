import { useMemo, useState } from "react";

import { useFolders, useMoveMeeting, useStartMeeting } from "../api/hooks";
import type { FolderRead } from "../api/types";

interface Props {
  // Whether a meeting is already recording (only one runs at a time) — gates the Start button.
  recording: boolean;
  // Whether the transcription sidecars have loaded their models (App polls this from launch). While
  // false the Start button is disabled and shows a "preparing" state, so early untranscribable audio
  // is never recorded.
  sidecarsReady: boolean;
  // Called with the new meeting's id once it starts (App selects it → the live view opens).
  onStarted: (meetingId: string) => void;
  onClose: () => void;
}

// The timestamp name the box is pre-filled with, e.g. "Jul 23, 2026 — 10:42 AM". Editable; if the
// user clears it we fall back to the server's own timestamp-derived default (title: null).
function defaultTitle(now: Date): string {
  const date = now.toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" });
  const time = now.toLocaleTimeString("en-US", { hour: "numeric", minute: "2-digit" });
  return `${date} — ${time}`;
}

// Folders flattened depth-first for the picker, each labelled with its nesting depth so sub-folders
// read as indented options.
function flattenFolders(folders: FolderRead[]): { folder: FolderRead; depth: number }[] {
  const childrenByParent = new Map<string | null, FolderRead[]>();
  for (const folder of folders) {
    const key = folder.parent_id ?? null;
    const list = childrenByParent.get(key);
    if (list) list.push(folder);
    else childrenByParent.set(key, [folder]);
  }
  const out: { folder: FolderRead; depth: number }[] = [];
  const walk = (parent: string | null, depth: number) => {
    for (const folder of childrenByParent.get(parent) ?? []) {
      out.push({ folder, depth });
      walk(folder.id, depth + 1);
    }
  };
  walk(null, 0);
  return out;
}

export function RecordMenu({ recording, sidecarsReady, onStarted, onClose }: Props) {
  const [title, setTitle] = useState(() => defaultTitle(new Date()));
  const [folderId, setFolderId] = useState<string | null>(null);

  const start = useStartMeeting();
  const move = useMoveMeeting();
  const folders = useFolders();

  const folderOptions = useMemo(
    () => flattenFolders(folders.data?.items ?? []),
    [folders.data?.items],
  );

  // Same gate as the meeting-list Start button: one meeting records at a time, and the sidecars must
  // have loaded their models first.
  const canStart = !recording && sidecarsReady && !start.isPending;
  // The transcription models load in the background (~seconds, longer on first run) — pulse the dot
  // while we wait so the disabled button reads as "in progress", not hung.
  const preparing = !recording && !sidecarsReady && !start.isPending;
  const startLabel = start.isPending
    ? "Starting…"
    : recording
      ? "Recording…"
      : sidecarsReady
        ? "Start recording"
        : "Preparing models…";

  const onStart = () => {
    if (!canStart) return;
    start.mutate(
      { title: title.trim() || null },
      {
        onSuccess: (meeting) => {
          // File it under the chosen folder (fire-and-forget: navigation shouldn't wait on the move).
          if (folderId) move.mutate({ id: meeting.id, folderId });
          onStarted(meeting.id);
        },
      },
    );
  };

  return (
    <div className="record-menu">
      <p className="record-menu__label">New recording</p>
      <input
        className="record-menu__title"
        value={title}
        aria-label="Recording name"
        placeholder="Recording name"
        autoFocus
        onChange={(event) => setTitle(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter") onStart();
          if (event.key === "Escape") onClose();
        }}
      />
      <select
        className="record-menu__folder"
        aria-label="Folder"
        value={folderId ?? ""}
        onChange={(event) => setFolderId(event.target.value || null)}
      >
        <option value="">No folder</option>
        {folderOptions.map(({ folder, depth }) => (
          <option key={folder.id} value={folder.id}>
            {`${"  ".repeat(depth)}${folder.name}`}
          </option>
        ))}
      </select>
      <button
        type="button"
        className="record-menu__start"
        onClick={onStart}
        disabled={!canStart}
        title={
          recording || sidecarsReady
            ? undefined
            : "Loading transcription models — ready to record in a moment"
        }
      >
        <span
          className={"record-menu__start-dot" + (preparing ? " is-preparing" : "")}
          aria-hidden="true"
        />
        {startLabel}
      </button>
      {start.error ? (
        <p className="record-menu__error" role="alert">
          {(start.error as Error).message}
        </p>
      ) : null}
    </div>
  );
}

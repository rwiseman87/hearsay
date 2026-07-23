import { useMemo, useState, type DragEvent as ReactDragEvent } from "react";

import { ApiError } from "../api/client";
import {
  useCreateFolder,
  useDeleteFolder,
  useDeleteMeeting,
  useFolders,
  useMoveMeeting,
  useRenameFolder,
  useRenameMeeting,
  useReparentFolder,
  useStartMeeting,
  useStatus,
} from "../api/hooks";
import type { FolderRead, MeetingRead } from "../api/types";

interface Props {
  meetings: MeetingRead[];
  isLoading: boolean;
  error: unknown;
  selectedId: string | null;
  onSelect: (id: string) => void;
  // Rendered inside the home dashboard rather than as the standalone sidebar: drops the sidebar
  // chrome (border/background) and hides the start-a-meeting composer, since the dashboard's Record
  // button is the one start affordance.
  embedded?: boolean;
}

function errorMessage(error: unknown): string {
  if (error instanceof ApiError || error instanceof Error) return error.message;
  return String(error);
}

// Native HTML5 drag-and-drop. The `${kind}:${id}` payload rides on the dataTransfer (read on drop);
// the lifted `Drag` state lets a folder drop-target refuse an illegal move (into itself or one of
// its own descendants) while the drag is in flight. Each level indents by INDENT px.
const DND_MIME = "application/x-hearsay-item";
const INDENT = 14;
type Drag = { kind: "meeting" | "folder"; id: string } | null;

function readDrag(event: ReactDragEvent): Drag {
  const [kind, id] = event.dataTransfer.getData(DND_MIME).split(":");
  if ((kind === "meeting" || kind === "folder") && id) return { kind, id };
  return null;
}

/// Whether `targetId` is `ancestorId` itself or sits inside its subtree — walked up the parent
/// chain. Used to forbid dropping a folder into its own descendant (which would orphan the subtree).
function isSelfOrDescendant(
  targetId: string,
  ancestorId: string,
  foldersById: Map<string, FolderRead>,
): boolean {
  let current: string | null = targetId;
  const seen = new Set<string>();
  while (current) {
    if (current === ancestorId) return true;
    if (seen.has(current)) break;
    seen.add(current);
    current = foldersById.get(current)?.parent_id ?? null;
  }
  return false;
}

// Shared, read-only context threaded to every node so the recursive tree doesn't prop-drill. Row
// and folder mutations live in the leaf components (colocated with their own inline-edit state).
interface SidebarCtx {
  selectedId: string | null;
  onSelect: (id: string) => void;
  childrenByParent: Map<string | null, FolderRead[]>;
  meetingsByFolder: Map<string | null, MeetingRead[]>;
  foldersById: Map<string, FolderRead>;
  collapsed: Set<string>;
  toggleCollapse: (id: string) => void;
  expand: (id: string) => void;
  dragging: Drag;
  setDragging: (drag: Drag) => void;
}

// A crisp disclosure chevron that rotates from ▸ (collapsed) to ▾ (expanded); an SVG reads far
// cleaner at this size than a font glyph (which looked like a stray dot).
function Chevron({ open }: { open: boolean }) {
  return (
    <svg
      className="folder__chevron-svg"
      style={{ transform: open ? "rotate(90deg)" : "none" }}
      width="9"
      height="9"
      viewBox="0 0 16 16"
      aria-hidden="true"
    >
      <path
        d="M5.5 3L11 8l-5.5 5"
        fill="none"
        stroke="currentColor"
        strokeWidth="2.2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

// A small filled folder glyph, so a folder reads unmistakably as a folder.
function FolderGlyph() {
  return (
    <svg width="15" height="13" viewBox="0 0 16 14" aria-hidden="true" fill="currentColor">
      <path d="M0 2.4A1.4 1.4 0 011.4 1h3.9c.37 0 .72.15.99.41L7.6 2.6h7A1.4 1.4 0 0116 4v7.6A1.4 1.4 0 0114.6 13H1.4A1.4 1.4 0 010 11.6V2.4z" />
    </svg>
  );
}

function MeetingRow({
  meeting,
  depth,
  ctx,
}: {
  meeting: MeetingRead;
  depth: number;
  ctx: SidebarCtx;
}) {
  const rename = useRenameMeeting();
  const remove = useDeleteMeeting();
  const [editing, setEditing] = useState(false);
  const [editTitle, setEditTitle] = useState("");
  const [confirming, setConfirming] = useState(false);

  const startEdit = () => {
    rename.reset();
    setConfirming(false);
    setEditTitle(meeting.title);
    setEditing(true);
  };
  const cancelEdit = () => {
    rename.reset();
    setEditing(false);
    setEditTitle("");
  };
  const submitEdit = () => {
    const trimmed = editTitle.trim();
    if (!trimmed || trimmed === meeting.title) {
      cancelEdit();
      return;
    }
    rename.mutate({ id: meeting.id, title: trimmed }, { onSuccess: cancelEdit });
  };

  const renaming = rename.isPending;
  const dragged = ctx.dragging?.kind === "meeting" && ctx.dragging.id === meeting.id;
  const classes = ["meetings__item"];
  if (meeting.id === ctx.selectedId) classes.push("is-selected");
  if (dragged) classes.push("meetings__item--dragging");

  return (
    <li
      className={classes.join(" ")}
      style={{ paddingLeft: `${depth * INDENT}px` }}
      // The inline edit input must stay draggable-free so text selection works; drag the row only
      // when not editing.
      draggable={!editing}
      onDragStart={(event) => {
        event.dataTransfer.setData(DND_MIME, `meeting:${meeting.id}`);
        event.dataTransfer.effectAllowed = "move";
        ctx.setDragging({ kind: "meeting", id: meeting.id });
      }}
      onDragEnd={() => ctx.setDragging(null)}
    >
      {editing ? (
        <form
          className="meetings__edit"
          onSubmit={(event) => {
            event.preventDefault();
            submitEdit();
          }}
        >
          <input
            className="meetings__edit-input"
            value={editTitle}
            autoFocus
            aria-label={`Rename ${meeting.title}`}
            disabled={renaming}
            onChange={(event) => setEditTitle(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") cancelEdit();
            }}
          />
          <button
            type="submit"
            className="meetings__confirm-yes"
            disabled={renaming || editTitle.trim() === ""}
          >
            {renaming ? "Saving…" : "Save"}
          </button>
          <button
            type="button"
            className="meetings__confirm-no"
            disabled={renaming}
            onClick={cancelEdit}
          >
            Cancel
          </button>
        </form>
      ) : (
        <>
          <span className="meetings__lead" aria-hidden="true" />
          <button type="button" className="meetings__open" onClick={() => ctx.onSelect(meeting.id)}>
            <span className="meetings__title">{meeting.title}</span>
            {meeting.status !== "finalized" ? (
              <span className={`badge badge--${meeting.status}`}>{meeting.status}</span>
            ) : null}
          </button>
          {confirming ? (
            <span className="meetings__confirm">
              <button
                type="button"
                className="meetings__confirm-yes"
                aria-label={`Confirm delete ${meeting.title}`}
                disabled={remove.isPending}
                onClick={() => remove.mutate(meeting.id, { onSettled: () => setConfirming(false) })}
              >
                {remove.isPending ? "Deleting…" : "Delete"}
              </button>
              <button
                type="button"
                className="meetings__confirm-no"
                aria-label="Cancel delete"
                disabled={remove.isPending}
                onClick={() => setConfirming(false)}
              >
                Cancel
              </button>
            </span>
          ) : (
            <>
              <button
                type="button"
                className="meetings__edit-btn"
                aria-label={`Rename ${meeting.title}`}
                onClick={startEdit}
              >
                ✎
              </button>
              <button
                type="button"
                className="meetings__delete"
                aria-label={`Delete ${meeting.title}`}
                onClick={() => setConfirming(true)}
              >
                ✕
              </button>
            </>
          )}
        </>
      )}
    </li>
  );
}

function FolderNode({
  folder,
  depth,
  ctx,
}: {
  folder: FolderRead;
  depth: number;
  ctx: SidebarCtx;
}) {
  const rename = useRenameFolder();
  const remove = useDeleteFolder();
  const createSub = useCreateFolder();
  const moveMeeting = useMoveMeeting();
  const reparent = useReparentFolder();

  const [editing, setEditing] = useState(false);
  const [editName, setEditName] = useState("");
  const [confirming, setConfirming] = useState(false);
  const [addingSub, setAddingSub] = useState(false);
  const [subName, setSubName] = useState("");
  const [isOver, setIsOver] = useState(false);

  const childFolders = ctx.childrenByParent.get(folder.id) ?? [];
  const folderMeetings = ctx.meetingsByFolder.get(folder.id) ?? [];
  const collapsed = ctx.collapsed.has(folder.id);

  // A meeting can always drop here; a folder cannot drop onto itself or one of its descendants.
  const canDrop =
    ctx.dragging !== null &&
    !(
      ctx.dragging.kind === "folder" &&
      isSelfOrDescendant(folder.id, ctx.dragging.id, ctx.foldersById)
    );

  const onDrop = (event: ReactDragEvent) => {
    event.preventDefault();
    setIsOver(false);
    const drag = readDrag(event);
    if (!drag) return;
    if (drag.kind === "meeting") {
      if (folder.id !== drag.id) moveMeeting.mutate({ id: drag.id, folderId: folder.id });
    } else {
      // Re-check the cycle guard on drop (the server also rejects it), and skip a no-op move.
      if (isSelfOrDescendant(folder.id, drag.id, ctx.foldersById)) return;
      if (ctx.foldersById.get(drag.id)?.parent_id === folder.id) return;
      reparent.mutate({ id: drag.id, parentId: folder.id });
    }
    ctx.expand(folder.id);
  };

  const submitRename = () => {
    const trimmed = editName.trim();
    if (!trimmed || trimmed === folder.name) {
      setEditing(false);
      return;
    }
    rename.mutate({ id: folder.id, name: trimmed }, { onSuccess: () => setEditing(false) });
  };
  const submitSub = () => {
    const trimmed = subName.trim();
    if (!trimmed) {
      setAddingSub(false);
      return;
    }
    createSub.mutate(
      { name: trimmed, parent_id: folder.id },
      {
        onSuccess: () => {
          setSubName("");
          setAddingSub(false);
          ctx.expand(folder.id);
        },
      },
    );
  };

  return (
    <div className="folder">
      <div
        className={`folder__header${isOver && canDrop ? " folder--drop-active" : ""}`}
        style={{ paddingLeft: `${depth * INDENT}px` }}
        draggable={!editing}
        onDragStart={(event) => {
          event.stopPropagation();
          event.dataTransfer.setData(DND_MIME, `folder:${folder.id}`);
          event.dataTransfer.effectAllowed = "move";
          ctx.setDragging({ kind: "folder", id: folder.id });
        }}
        onDragEnd={() => ctx.setDragging(null)}
        onDragOver={(event) => {
          if (!canDrop) return;
          event.preventDefault();
          event.dataTransfer.dropEffect = "move";
        }}
        onDragEnter={() => {
          if (canDrop) setIsOver(true);
        }}
        onDragLeave={() => setIsOver(false)}
        onDrop={onDrop}
      >
        <button
          type="button"
          className="folder__toggle"
          aria-expanded={!collapsed}
          aria-label={collapsed ? `Expand ${folder.name}` : `Collapse ${folder.name}`}
          onClick={() => ctx.toggleCollapse(folder.id)}
        >
          <Chevron open={!collapsed} />
        </button>
        <span className="folder__icon" aria-hidden="true">
          <FolderGlyph />
        </span>
        {editing ? (
          <form
            className="meetings__edit"
            onSubmit={(event) => {
              event.preventDefault();
              submitRename();
            }}
          >
            <input
              className="meetings__edit-input"
              value={editName}
              autoFocus
              aria-label={`Rename folder ${folder.name}`}
              disabled={rename.isPending}
              onChange={(event) => setEditName(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Escape") setEditing(false);
              }}
            />
            <button
              type="submit"
              className="meetings__confirm-yes"
              disabled={rename.isPending || editName.trim() === ""}
            >
              {rename.isPending ? "Saving…" : "Save"}
            </button>
            <button
              type="button"
              className="meetings__confirm-no"
              disabled={rename.isPending}
              onClick={() => setEditing(false)}
            >
              Cancel
            </button>
          </form>
        ) : (
          <>
            <button
              type="button"
              className="folder__name"
              onClick={() => ctx.toggleCollapse(folder.id)}
            >
              {folder.name}
            </button>
            {folderMeetings.length > 0 ? (
              <span className="folder__count">{folderMeetings.length}</span>
            ) : null}
            {confirming ? (
              <span className="meetings__confirm">
                {childFolders.length > 0 ? (
                  <span className="meetings__confirm-hint" role="alert">
                    Delete this folder and all its sub-folders? Meetings inside move to Unfiled.
                  </span>
                ) : null}
                <button
                  type="button"
                  className="meetings__confirm-yes"
                  aria-label={`Confirm delete folder ${folder.name}`}
                  disabled={remove.isPending}
                  onClick={() =>
                    remove.mutate(folder.id, { onSettled: () => setConfirming(false) })
                  }
                >
                  {remove.isPending ? "Deleting…" : "Delete"}
                </button>
                <button
                  type="button"
                  className="meetings__confirm-no"
                  aria-label="Cancel delete"
                  disabled={remove.isPending}
                  onClick={() => setConfirming(false)}
                >
                  Cancel
                </button>
              </span>
            ) : (
              <span className="folder__actions">
                <button
                  type="button"
                  className="meetings__edit-btn"
                  aria-label={`New sub-folder in ${folder.name}`}
                  title="New sub-folder"
                  onClick={() => {
                    createSub.reset();
                    setAddingSub(true);
                    ctx.expand(folder.id);
                  }}
                >
                  ＋
                </button>
                <button
                  type="button"
                  className="meetings__edit-btn"
                  aria-label={`Rename folder ${folder.name}`}
                  onClick={() => {
                    rename.reset();
                    setConfirming(false);
                    setEditName(folder.name);
                    setEditing(true);
                  }}
                >
                  ✎
                </button>
                <button
                  type="button"
                  className="meetings__delete"
                  aria-label={`Delete folder ${folder.name}`}
                  onClick={() => setConfirming(true)}
                >
                  ✕
                </button>
              </span>
            )}
          </>
        )}
      </div>
      {!collapsed ? (
        <>
          {addingSub ? (
            <form
              className="folder__new-sub"
              style={{ paddingLeft: `${(depth + 1) * INDENT}px` }}
              onSubmit={(event) => {
                event.preventDefault();
                submitSub();
              }}
            >
              <input
                value={subName}
                autoFocus
                placeholder="Sub-folder name"
                aria-label={`New sub-folder in ${folder.name}`}
                disabled={createSub.isPending}
                onChange={(event) => setSubName(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Escape") setAddingSub(false);
                }}
              />
              <button
                type="submit"
                className="meetings__confirm-yes"
                disabled={createSub.isPending || subName.trim() === ""}
              >
                Add
              </button>
              <button
                type="button"
                className="meetings__confirm-no"
                disabled={createSub.isPending}
                onClick={() => setAddingSub(false)}
              >
                Cancel
              </button>
            </form>
          ) : null}
          <ul className="folder__meetings">
            {folderMeetings.map((meeting) => (
              <MeetingRow key={meeting.id} meeting={meeting} depth={depth + 1} ctx={ctx} />
            ))}
          </ul>
          {childFolders.map((child) => (
            <FolderNode key={child.id} folder={child} depth={depth + 1} ctx={ctx} />
          ))}
        </>
      ) : null}
    </div>
  );
}

// The root "Unfiled" bucket: meetings with no folder. Shown only once at least one folder exists
// (before that, the sidebar is just the flat list). Dropping a meeting here un-files it; dropping a
// folder here moves it back to the root.
function UnfiledSection({ meetings, ctx }: { meetings: MeetingRead[]; ctx: SidebarCtx }) {
  const moveMeeting = useMoveMeeting();
  const reparent = useReparentFolder();
  const [isOver, setIsOver] = useState(false);
  const collapsed = ctx.collapsed.has("unfiled");
  const canDrop = ctx.dragging !== null;

  const onDrop = (event: ReactDragEvent) => {
    event.preventDefault();
    setIsOver(false);
    const drag = readDrag(event);
    if (!drag) return;
    if (drag.kind === "meeting") {
      moveMeeting.mutate({ id: drag.id, folderId: null });
    } else if (ctx.foldersById.get(drag.id)?.parent_id != null) {
      // Only reparent to the root if it isn't already there.
      reparent.mutate({ id: drag.id, parentId: null });
    }
  };

  return (
    <div className="folder">
      <div
        className={`folder__header${isOver ? " folder--drop-active" : ""}`}
        onDragOver={(event) => {
          if (!canDrop) return;
          event.preventDefault();
          event.dataTransfer.dropEffect = "move";
        }}
        onDragEnter={() => {
          if (canDrop) setIsOver(true);
        }}
        onDragLeave={() => setIsOver(false)}
        onDrop={onDrop}
      >
        <button
          type="button"
          className="folder__toggle"
          aria-expanded={!collapsed}
          aria-label={collapsed ? "Expand Unfiled" : "Collapse Unfiled"}
          onClick={() => ctx.toggleCollapse("unfiled")}
        >
          <Chevron open={!collapsed} />
        </button>
        <span className="folder__icon" aria-hidden="true">
          <FolderGlyph />
        </span>
        <button type="button" className="folder__name" onClick={() => ctx.toggleCollapse("unfiled")}>
          Unfiled
        </button>
        {meetings.length > 0 ? <span className="folder__count">{meetings.length}</span> : null}
      </div>
      {!collapsed ? (
        <ul className="folder__meetings">
          {meetings.map((meeting) => (
            <MeetingRow key={meeting.id} meeting={meeting} depth={1} ctx={ctx} />
          ))}
        </ul>
      ) : null}
    </div>
  );
}

export function MeetingList({
  meetings,
  isLoading,
  error,
  selectedId,
  onSelect,
  embedded = false,
}: Props) {
  const [title, setTitle] = useState("");
  const [addingFolder, setAddingFolder] = useState(false);
  const [folderName, setFolderName] = useState("");
  // Folder ids the user has collapsed (plus the sentinel "unfiled"); everything is expanded by
  // default. Kept here so a drop can force-expand its target.
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [dragging, setDragging] = useState<Drag>(null);

  const start = useStartMeeting();
  const status = useStatus();
  const folders = useFolders();
  const createFolder = useCreateFolder();

  const folderItems = useMemo(() => folders.data?.items ?? [], [folders.data?.items]);

  const childrenByParent = useMemo(() => {
    const map = new Map<string | null, FolderRead[]>();
    for (const folder of folderItems) {
      const key = folder.parent_id ?? null;
      const list = map.get(key);
      if (list) list.push(folder);
      else map.set(key, [folder]);
    }
    return map;
  }, [folderItems]);

  const meetingsByFolder = useMemo(() => {
    const map = new Map<string | null, MeetingRead[]>();
    for (const meeting of meetings) {
      const key = meeting.folder_id ?? null;
      const list = map.get(key);
      if (list) list.push(meeting);
      else map.set(key, [meeting]);
    }
    return map;
  }, [meetings]);

  const foldersById = useMemo(() => {
    const map = new Map<string, FolderRead>();
    for (const folder of folderItems) map.set(folder.id, folder);
    return map;
  }, [folderItems]);

  const ctx: SidebarCtx = {
    selectedId,
    onSelect,
    childrenByParent,
    meetingsByFolder,
    foldersById,
    collapsed,
    toggleCollapse: (id) =>
      setCollapsed((prev) => {
        const next = new Set(prev);
        if (next.has(id)) next.delete(id);
        else next.add(id);
        return next;
      }),
    expand: (id) =>
      setCollapsed((prev) => {
        if (!prev.has(id)) return prev;
        const next = new Set(prev);
        next.delete(id);
        return next;
      }),
    dragging,
    setDragging,
  };

  const rootFolders = childrenByParent.get(null) ?? [];
  const unfiled = meetingsByFolder.get(null) ?? [];
  const hasFolders = folderItems.length > 0;

  // A meeting is already recording (only one runs at a time). The warm pool is intentionally empty
  // during a meeting — it re-warms once this one stops — so don't read that as "models loading".
  const recording = meetings.some((meeting) => meeting.status === "recording");
  // Gate "Start" on the transcription sidecars having loaded their models — starting before then
  // records ~30 s of audio the live view can't transcribe. Treat an errored status probe as ready so
  // a status-endpoint problem never bricks the button (a real broken engine still fails at start).
  const sidecarsReady = status.isError || (status.data?.sidecars_ready ?? false);
  const canStart = !recording && sidecarsReady && !start.isPending;
  const startLabel = start.isPending
    ? "Starting…"
    : recording
      ? "Recording…"
      : sidecarsReady
        ? "Start"
        : "Preparing models…";

  const onStart = () => {
    if (!canStart) return;
    start.mutate(
      { title: title.trim() || null },
      {
        onSuccess: (meeting) => {
          setTitle("");
          onSelect(meeting.id);
        },
      },
    );
  };

  const submitFolder = () => {
    const trimmed = folderName.trim();
    if (!trimmed) {
      setAddingFolder(false);
      return;
    }
    createFolder.mutate(
      { name: trimmed },
      {
        onSuccess: () => {
          setFolderName("");
          setAddingFolder(false);
        },
      },
    );
  };

  return (
    <aside className={"meetings" + (embedded ? " meetings--embedded" : "")}>
      {!embedded ? (
        <div className="meetings__new">
          <input
            value={title}
            placeholder="Meeting title (optional)"
            aria-label="Meeting title"
            onChange={(event) => setTitle(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") onStart();
            }}
          />
          <button
            type="button"
            onClick={onStart}
            disabled={!canStart}
            title={
              recording || sidecarsReady
                ? undefined
                : "Loading transcription models — ready to record in a moment"
            }
          >
            {startLabel}
          </button>
        </div>
      ) : null}
      <div className="meetings__folder-new">
        {addingFolder ? (
          <form
            className="meetings__edit"
            onSubmit={(event) => {
              event.preventDefault();
              submitFolder();
            }}
          >
            <input
              className="meetings__edit-input"
              value={folderName}
              autoFocus
              placeholder="Folder name"
              aria-label="New folder name"
              disabled={createFolder.isPending}
              onChange={(event) => setFolderName(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Escape") setAddingFolder(false);
              }}
            />
            <button
              type="submit"
              className="meetings__confirm-yes"
              disabled={createFolder.isPending || folderName.trim() === ""}
            >
              Add
            </button>
            <button
              type="button"
              className="meetings__confirm-no"
              disabled={createFolder.isPending}
              onClick={() => setAddingFolder(false)}
            >
              Cancel
            </button>
          </form>
        ) : (
          <button
            type="button"
            className="meetings__new-folder-btn"
            onClick={() => {
              createFolder.reset();
              setAddingFolder(true);
            }}
          >
            ＋ New folder
          </button>
        )}
      </div>
      {!embedded && !recording && !sidecarsReady ? (
        <p className="muted meetings__preparing" role="status">
          Loading transcription models… you can start recording once they're ready.
        </p>
      ) : null}
      {!embedded && start.error ? <p className="error">{errorMessage(start.error)}</p> : null}
      {createFolder.error ? <p className="error">{errorMessage(createFolder.error)}</p> : null}
      {isLoading ? <p className="muted">Loading meetings…</p> : null}
      {error ? <p className="error">{errorMessage(error)}</p> : null}
      <div className="meetings__tree">
        {rootFolders.map((folder) => (
          <FolderNode key={folder.id} folder={folder} depth={0} ctx={ctx} />
        ))}
        {hasFolders ? (
          <UnfiledSection meetings={unfiled} ctx={ctx} />
        ) : (
          <ul className="folder__meetings">
            {unfiled.map((meeting) => (
              <MeetingRow key={meeting.id} meeting={meeting} depth={0} ctx={ctx} />
            ))}
          </ul>
        )}
        {meetings.length === 0 && !isLoading ? <p className="muted">No meetings yet.</p> : null}
      </div>
    </aside>
  );
}

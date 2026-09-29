import { useEffect, useMemo, useState, type DragEvent as ReactDragEvent } from "react";

import { ApiError } from "../api/client";
import {
  MEETING_PAGE_SIZE,
  useCreateFolder,
  useDeleteFolder,
  useDeleteMeeting,
  useFolders,
  useMeetingCounts,
  useMeetings,
  useMoveMeeting,
  useRenameFolder,
  useRenameMeeting,
} from "../api/hooks";
import type { FolderRead, MeetingRead, MeetingSort } from "../api/types";

interface Props {
  onSelect: (id: string) => void;
}

// A meeting id rides on the drag payload; folders (and "Unfiled") are drop targets that file it.
const DND_MIME = "application/x-hearsay-meeting";
const INDENT = 14;
// Sentinel filter value (distinct from any folder UUID) for the "Unfiled" bucket.
const UNFILED = "unfiled";
// Typing pause before the title search goes to the server, so a word is one request, not six.
const SEARCH_DEBOUNCE_MS = 250;
const ELLIPSIS = "…" as const;

function errorMessage(error: unknown): string {
  if (error instanceof ApiError || error instanceof Error) return error.message;
  return String(error);
}

// Two-letter avatar text from the meeting title (first letters of the first two words, else the
// first two characters). Purely cosmetic.
function initials(title: string): string {
  const words = title.split(/[^A-Za-z0-9]+/u).filter(Boolean);
  if (words.length === 0) return "?";
  if (words.length === 1) return words[0].slice(0, 2).toUpperCase();
  return (words[0][0] + words[1][0]).toUpperCase();
}

// A stable hue from the meeting id, so each avatar keeps a consistent muted tint.
function hue(id: string): number {
  let acc = 0;
  for (let i = 0; i < id.length; i += 1) acc = (acc * 31 + id.charCodeAt(i)) % 360;
  return acc;
}

function durationLabel(meeting: MeetingRead): string | null {
  if (!meeting.ended_at) return null;
  const mins = Math.round(
    (new Date(meeting.ended_at).getTime() - new Date(meeting.started_at).getTime()) / 60_000,
  );
  if (mins < 1) return "<1 min";
  if (mins < 60) return `${mins} min`;
  const hours = Math.floor(mins / 60);
  const rest = mins % 60;
  return rest ? `${hours}h ${rest}m` : `${hours}h`;
}

// Which time bucket a meeting falls in, compared by calendar day. `key` orders the groups.
function timeBucket(iso: string, now: Date): { key: number; label: string } {
  const then = new Date(iso);
  const startOfDay = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const days = Math.round((startOfDay(now) - startOfDay(then)) / 86_400_000);
  if (days <= 0) return { key: 0, label: "Today" };
  if (days === 1) return { key: 1, label: "Yesterday" };
  if (days < 7) return { key: 2, label: "Earlier this week" };
  if (days < 31) return { key: 3, label: "Earlier this month" };
  return { key: 4, label: "Older" };
}

// Folders flattened depth-first with their nesting depth, so the sidebar renders the tree as indented
// rows (nested folders are supported; sub-folders sit under their parent).
function flattenFolders(folders: FolderRead[]): { folder: FolderRead; depth: number }[] {
  const byParent = new Map<string | null, FolderRead[]>();
  for (const folder of folders) {
    const key = folder.parent_id ?? null;
    const list = byParent.get(key);
    if (list) list.push(folder);
    else byParent.set(key, [folder]);
  }
  const out: { folder: FolderRead; depth: number }[] = [];
  const walk = (parent: string | null, depth: number) => {
    for (const folder of byParent.get(parent) ?? []) {
      out.push({ folder, depth });
      walk(folder.id, depth + 1);
    }
  };
  walk(null, 0);
  return out;
}

function FolderGlyph() {
  return (
    <svg width="14" height="12" viewBox="0 0 16 14" aria-hidden="true" fill="currentColor">
      <path d="M0 2.4A1.4 1.4 0 011.4 1h3.9c.37 0 .72.15.99.41L7.6 2.6h7A1.4 1.4 0 0116 4v7.6A1.4 1.4 0 0114.6 13H1.4A1.4 1.4 0 010 11.6V2.4z" />
    </svg>
  );
}

type Editing =
  | { mode: "new-root" }
  | { mode: "new-sub"; parentId: string }
  | { mode: "rename"; folderId: string }
  | null;

// The Library: a full two-pane meetings browser (left = All meetings + the folder tree as filters,
// right = one page of the filtered meetings, grouped by recency). The folder bucket, the title
// search, the sort, and the counts all resolve server-side, so nothing here is capped by how many
// meetings a single request returns.
export function Library({ onSelect }: Props) {
  // Active filter: null = "All meetings", UNFILED, else a folder id.
  const [filter, setFilter] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<MeetingSort>("newest");
  const [page, setPage] = useState(1);
  const [editing, setEditing] = useState<Editing>(null);
  const [draft, setDraft] = useState("");
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [dragging, setDragging] = useState(false);
  const [dropTarget, setDropTarget] = useState<string | null>(null); // folder id, or UNFILED
  // Per-meeting inline actions (rename / delete), keyed by meeting id.
  const [renamingMeeting, setRenamingMeeting] = useState<string | null>(null);
  const [meetingDraft, setMeetingDraft] = useState("");
  const [confirmDeleteMeeting, setConfirmDeleteMeeting] = useState<string | null>(null);

  useEffect(() => {
    const timer = setTimeout(() => setQuery(search.trim()), SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [search]);

  // Changing what is listed starts again at the first page.
  useEffect(() => setPage(1), [filter, query, sort]);

  const isFolderFilter = filter !== null && filter !== UNFILED;
  const meetingsQuery = useMeetings({
    page,
    folderId: isFolderFilter ? filter : null,
    unfiled: filter === UNFILED,
    q: query,
    sort,
  });
  const counts = useMeetingCounts();
  const folders = useFolders();
  const createFolder = useCreateFolder();
  const renameFolder = useRenameFolder();
  const deleteFolder = useDeleteFolder();
  const moveMeeting = useMoveMeeting();
  const renameMeeting = useRenameMeeting();
  const deleteMeeting = useDeleteMeeting();

  const folderItems = useMemo(() => folders.data?.items ?? [], [folders.data?.items]);
  const flat = useMemo(() => flattenFolders(folderItems), [folderItems]);

  const countByFolder = useMemo(() => {
    const map = new Map<string, number>();
    for (const row of counts.data?.folders ?? []) map.set(row.folder_id, row.meetings);
    return map;
  }, [counts.data?.folders]);

  const folderName = isFolderFilter
    ? (folderItems.find((f) => f.id === filter)?.name ?? "Folder")
    : null;
  const title =
    filter === null ? "All meetings" : filter === UNFILED ? "Unfiled" : folderName;

  const visible = meetingsQuery.data?.items ?? [];
  const matched = meetingsQuery.data?.total ?? 0;
  const pageCount = Math.max(1, Math.ceil(matched / MEETING_PAGE_SIZE));

  // Deleting the last meeting on the last page (or a shrinking filter) can leave the page past the
  // end; step back rather than showing an empty list with meetings still behind it.
  useEffect(() => {
    if (page > pageCount) setPage(pageCount);
  }, [page, pageCount]);

  const now = new Date();
  // Group the listed meetings into recency buckets, ordered to follow the sort direction.
  const groupMap = new Map<number, { label: string; items: MeetingRead[] }>();
  for (const meeting of visible) {
    const bucket = timeBucket(meeting.started_at, now);
    const group = groupMap.get(bucket.key);
    if (group) group.items.push(meeting);
    else groupMap.set(bucket.key, { label: bucket.label, items: [meeting] });
  }
  const groups = [...groupMap.keys()]
    .sort((a, b) => (sort === "newest" ? a - b : b - a))
    .map((key) => groupMap.get(key) as { label: string; items: MeetingRead[] });

  const startEdit = (next: Editing, value: string) => {
    createFolder.reset();
    renameFolder.reset();
    setConfirmDelete(null);
    setDraft(value);
    setEditing(next);
  };

  const submitEdit = () => {
    const name = draft.trim();
    if (!name || !editing) {
      setEditing(null);
      return;
    }
    if (editing.mode === "rename") {
      renameFolder.mutate({ id: editing.folderId, name }, { onSuccess: () => setEditing(null) });
    } else {
      const parent_id = editing.mode === "new-sub" ? editing.parentId : undefined;
      createFolder.mutate({ name, parent_id }, { onSuccess: () => setEditing(null) });
    }
  };

  const dropOn = (folderId: string | null) => (event: ReactDragEvent) => {
    event.preventDefault();
    setDropTarget(null);
    setDragging(false);
    const id = event.dataTransfer.getData(DND_MIME);
    if (id) moveMeeting.mutate({ id, folderId });
  };
  const allowDrop = (target: string) => (event: ReactDragEvent) => {
    if (!dragging) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
    setDropTarget(target);
  };

  return (
    <section className="library">
      <aside className="library__nav">
        <p className="library__nav-head">Library</p>
        <button
          type="button"
          className={"library__item" + (filter === null ? " is-active" : "")}
          onClick={() => setFilter(null)}
        >
          <span className="library__item-icon" aria-hidden="true">
            <svg width="16" height="16" viewBox="0 0 18 18" fill="none">
              <path
                d="M2.5 4.5h13M2.5 9h13M2.5 13.5h13"
                stroke="currentColor"
                strokeWidth="1.6"
                strokeLinecap="round"
              />
            </svg>
          </span>
          <span className="library__item-label">All meetings</span>
          <span className="library__count">{counts.data?.total ?? 0}</span>
        </button>
        <button
          type="button"
          className={"library__item" + (filter === UNFILED ? " is-active" : "")}
          onClick={() => setFilter(UNFILED)}
          onDragOver={allowDrop(UNFILED)}
          onDragLeave={() => setDropTarget(null)}
          onDrop={dropOn(null)}
          data-drop={dropTarget === UNFILED ? "" : undefined}
          title="Meetings not in any folder"
        >
          <span className="library__item-icon" aria-hidden="true">
            <svg width="16" height="15" viewBox="0 0 16 15" fill="none" stroke="currentColor">
              <path
                d="M1 3.2A1.4 1.4 0 012.4 1.8h3.3c.3 0 .6.12.8.34l1 1.06h6A1.4 1.4 0 0114.9 4.6"
                strokeWidth="1.3"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
              <path
                d="M1 5.5h14v6.3A1.4 1.4 0 0113.6 13.2H2.4A1.4 1.4 0 011 11.8V5.5z"
                strokeWidth="1.3"
                strokeLinejoin="round"
              />
            </svg>
          </span>
          <span className="library__item-label">Unfiled</span>
          <span className="library__count">{counts.data?.unfiled ?? 0}</span>
        </button>

        <p className="library__nav-head">Folders</p>
        {flat.map(({ folder, depth }) => (
          <div key={folder.id}>
            {editing?.mode === "rename" && editing.folderId === folder.id ? (
              <form
                className="library__edit"
                style={{ paddingLeft: `${8 + depth * INDENT}px` }}
                onSubmit={(event) => {
                  event.preventDefault();
                  submitEdit();
                }}
              >
                <input
                  value={draft}
                  autoFocus
                  aria-label={`Rename ${folder.name}`}
                  disabled={renameFolder.isPending}
                  onChange={(event) => setDraft(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Escape") setEditing(null);
                  }}
                />
                <button type="submit" disabled={renameFolder.isPending || !draft.trim()}>
                  Save
                </button>
                <button type="button" onClick={() => setEditing(null)}>
                  Cancel
                </button>
              </form>
            ) : (
              <div
                className={"library__item" + (filter === folder.id ? " is-active" : "")}
                style={{ paddingLeft: `${8 + depth * INDENT}px` }}
                onDragOver={allowDrop(folder.id)}
                onDragLeave={() => setDropTarget(null)}
                onDrop={dropOn(folder.id)}
                data-drop={dropTarget === folder.id ? "" : undefined}
              >
                <button
                  type="button"
                  className="library__item-btn"
                  onClick={() => setFilter(folder.id)}
                >
                  <span className="library__item-icon" aria-hidden="true">
                    <FolderGlyph />
                  </span>
                  <span className="library__item-label">{folder.name}</span>
                </button>
                {confirmDelete === folder.id ? (
                  <span className="library__confirm">
                    <button
                      type="button"
                      aria-label={`Confirm delete ${folder.name}`}
                      disabled={deleteFolder.isPending}
                      onClick={() =>
                        deleteFolder.mutate(folder.id, {
                          onSettled: () => {
                            setConfirmDelete(null);
                            if (filter === folder.id) setFilter(null);
                          },
                        })
                      }
                    >
                      Delete
                    </button>
                    <button type="button" aria-label="Cancel delete" onClick={() => setConfirmDelete(null)}>
                      Cancel
                    </button>
                  </span>
                ) : (
                  <span className="library__trailing">
                    <span className="library__count">{countByFolder.get(folder.id) ?? 0}</span>
                    <span className="library__actions">
                      <button
                        type="button"
                        aria-label={`New sub-folder in ${folder.name}`}
                        title="New sub-folder"
                        onClick={() => startEdit({ mode: "new-sub", parentId: folder.id }, "")}
                      >
                        ＋
                      </button>
                      <button
                        type="button"
                        aria-label={`Rename ${folder.name}`}
                        onClick={() => startEdit({ mode: "rename", folderId: folder.id }, folder.name)}
                      >
                        ✎
                      </button>
                      <button
                        type="button"
                        aria-label={`Delete ${folder.name}`}
                        onClick={() => setConfirmDelete(folder.id)}
                      >
                        ✕
                      </button>
                    </span>
                  </span>
                )}
              </div>
            )}
            {editing?.mode === "new-sub" && editing.parentId === folder.id ? (
              <form
                className="library__edit"
                style={{ paddingLeft: `${8 + (depth + 1) * INDENT}px` }}
                onSubmit={(event) => {
                  event.preventDefault();
                  submitEdit();
                }}
              >
                <input
                  value={draft}
                  autoFocus
                  placeholder="Sub-folder name"
                  aria-label={`New sub-folder in ${folder.name}`}
                  disabled={createFolder.isPending}
                  onChange={(event) => setDraft(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Escape") setEditing(null);
                  }}
                />
                <button type="submit" disabled={createFolder.isPending || !draft.trim()}>
                  Add
                </button>
                <button type="button" onClick={() => setEditing(null)}>
                  Cancel
                </button>
              </form>
            ) : null}
          </div>
        ))}

        {editing?.mode === "new-root" ? (
          <form
            className="library__edit"
            onSubmit={(event) => {
              event.preventDefault();
              submitEdit();
            }}
          >
            <input
              value={draft}
              autoFocus
              placeholder="Folder name"
              aria-label="New folder name"
              disabled={createFolder.isPending}
              onChange={(event) => setDraft(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Escape") setEditing(null);
              }}
            />
            <button type="submit" disabled={createFolder.isPending || !draft.trim()}>
              Add
            </button>
            <button type="button" onClick={() => setEditing(null)}>
              Cancel
            </button>
          </form>
        ) : (
          <button
            type="button"
            className="library__new-folder"
            onClick={() => startEdit({ mode: "new-root" }, "")}
          >
            ＋ New folder
          </button>
        )}
        {createFolder.error ? (
          <p className="library__error">{errorMessage(createFolder.error)}</p>
        ) : null}
      </aside>

      <div className="library__main">
        <header className="library__top">
          <h1 className="library__title">{title}</h1>
          <div className="library__tools">
            <input
              className="library__search"
              type="search"
              placeholder="Search meetings…"
              aria-label="Search meetings by title"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
            />
            <select
              className="library__sort"
              aria-label="Sort meetings"
              value={sort}
              onChange={(event) => setSort(event.target.value as "newest" | "oldest")}
            >
              <option value="newest">Newest</option>
              <option value="oldest">Oldest</option>
            </select>
          </div>
        </header>

        <div className="library__list">
          {meetingsQuery.isLoading ? (
            <p className="muted library__empty">Loading…</p>
          ) : meetingsQuery.error ? (
            <p className="library__error" role="alert">
              {errorMessage(meetingsQuery.error)}
            </p>
          ) : groups.length === 0 ? (
            <p className="muted library__empty">
              {query ? "No meetings match your search." : "No meetings here yet."}
            </p>
          ) : (
            groups.map((group) => (
              <section key={group.label} className="library__group">
                <p className="library__group-label">{group.label}</p>
                <ul className="library__rows">
                  {group.items.map((meeting) => {
                    const parts = [
                      durationLabel(meeting),
                      meeting.folder_id ? folderNameFor(folderItems, meeting.folder_id) : null,
                    ].filter(Boolean);
                    if (renamingMeeting === meeting.id) {
                      const submit = () => {
                        const title = meetingDraft.trim();
                        if (!title || title === meeting.title) {
                          setRenamingMeeting(null);
                          return;
                        }
                        renameMeeting.mutate(
                          { id: meeting.id, title },
                          { onSuccess: () => setRenamingMeeting(null) },
                        );
                      };
                      return (
                        <li key={meeting.id}>
                          <form
                            className="library__row-edit"
                            onSubmit={(event) => {
                              event.preventDefault();
                              submit();
                            }}
                          >
                            <input
                              value={meetingDraft}
                              autoFocus
                              aria-label={`Rename ${meeting.title}`}
                              disabled={renameMeeting.isPending}
                              onChange={(event) => setMeetingDraft(event.target.value)}
                              onKeyDown={(event) => {
                                if (event.key === "Escape") setRenamingMeeting(null);
                              }}
                            />
                            <button type="submit" disabled={renameMeeting.isPending || !meetingDraft.trim()}>
                              {renameMeeting.isPending ? "Saving…" : "Save"}
                            </button>
                            <button type="button" onClick={() => setRenamingMeeting(null)}>
                              Cancel
                            </button>
                          </form>
                        </li>
                      );
                    }
                    return (
                      <li
                        key={meeting.id}
                        draggable
                        onDragStart={(event) => {
                          event.dataTransfer.setData(DND_MIME, meeting.id);
                          event.dataTransfer.effectAllowed = "move";
                          setDragging(true);
                        }}
                        onDragEnd={() => {
                          setDragging(false);
                          setDropTarget(null);
                        }}
                      >
                        <div className="library__row">
                          <button
                            type="button"
                            className="library__row-open"
                            onClick={() => onSelect(meeting.id)}
                          >
                            <span
                              className="library__avatar"
                              style={{
                                background: `hsl(${hue(meeting.id)} 26% 24%)`,
                                color: `hsl(${hue(meeting.id)} 55% 78%)`,
                              }}
                              aria-hidden="true"
                            >
                              {initials(meeting.title)}
                            </span>
                            <span className="library__row-main">
                              <span className="library__row-title">{meeting.title}</span>
                              <span className="library__row-meta">
                                {parts.length > 0 ? parts.join(" · ") : "—"}
                              </span>
                            </span>
                          </button>
                          <span className="library__row-trailing">
                            {meeting.status !== "finalized" ? (
                              <span className={`badge badge--${meeting.status}`}>
                                {meeting.status}
                              </span>
                            ) : null}
                            {confirmDeleteMeeting === meeting.id ? (
                              <span className="library__confirm">
                                <button
                                  type="button"
                                  aria-label={`Confirm delete ${meeting.title}`}
                                  disabled={deleteMeeting.isPending}
                                  onClick={() =>
                                    deleteMeeting.mutate(meeting.id, {
                                      onSettled: () => setConfirmDeleteMeeting(null),
                                    })
                                  }
                                >
                                  {deleteMeeting.isPending ? "Deleting…" : "Delete"}
                                </button>
                                <button
                                  type="button"
                                  aria-label="Cancel delete"
                                  onClick={() => setConfirmDeleteMeeting(null)}
                                >
                                  Cancel
                                </button>
                              </span>
                            ) : (
                              <span className="library__row-actions">
                                <button
                                  type="button"
                                  aria-label={`Rename ${meeting.title}`}
                                  onClick={() => {
                                    renameMeeting.reset();
                                    setConfirmDeleteMeeting(null);
                                    setMeetingDraft(meeting.title);
                                    setRenamingMeeting(meeting.id);
                                  }}
                                >
                                  ✎
                                </button>
                                <button
                                  type="button"
                                  aria-label={`Delete ${meeting.title}`}
                                  onClick={() => setConfirmDeleteMeeting(meeting.id)}
                                >
                                  ✕
                                </button>
                              </span>
                            )}
                          </span>
                        </div>
                      </li>
                    );
                  })}
                </ul>
              </section>
            ))
          )}
        </div>

        {matched > 0 ? (
          <div className="library__footer">
            <p className="muted library__tally">
              {matched === 1 ? "1 meeting" : `${matched} meetings`}
              {pageCount > 1 ? ` · page ${page} of ${pageCount}` : ""}
            </p>
            {pageCount > 1 ? (
              <nav className="library__pager" aria-label="Meeting pages">
                <button
                  type="button"
                  className="library__pager-step"
                  disabled={page <= 1}
                  onClick={() => setPage(page - 1)}
                >
                  ‹ Prev
                </button>
                {pageNumbers(page, pageCount).map((entry, index) =>
                  entry === ELLIPSIS ? (
                    <span key={`gap-${index}`} className="library__pager-gap" aria-hidden="true">
                      {ELLIPSIS}
                    </span>
                  ) : (
                    <button
                      key={entry}
                      type="button"
                      className={"library__pager-page" + (entry === page ? " is-active" : "")}
                      aria-label={`Page ${entry}`}
                      aria-current={entry === page ? "page" : undefined}
                      onClick={() => setPage(entry)}
                    >
                      {entry}
                    </button>
                  ),
                )}
                <button
                  type="button"
                  className="library__pager-step"
                  disabled={page >= pageCount}
                  onClick={() => setPage(page + 1)}
                >
                  Next ›
                </button>
              </nav>
            ) : null}
          </div>
        ) : null}
      </div>
    </section>
  );
}

function folderNameFor(folders: FolderRead[], id: string): string | null {
  return folders.find((f) => f.id === id)?.name ?? null;
}

// Page buttons: always the first and last page, the current one and the two beside it, with the
// runs between them elided.
function pageNumbers(current: number, count: number): (number | typeof ELLIPSIS)[] {
  const wanted = [1, count, current - 1, current, current + 1].filter((n) => n >= 1 && n <= count);
  const pages = [...new Set(wanted)].sort((a, b) => a - b);
  const out: (number | typeof ELLIPSIS)[] = [];
  let previous = 0;
  for (const n of pages) {
    if (previous && n - previous > 1) out.push(ELLIPSIS);
    out.push(n);
    previous = n;
  }
  return out;
}

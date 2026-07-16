import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { api } from "./client";
import { queryKeys } from "./queryKeys";
import type {
  DownloadState,
  FolderCreate,
  FolderRead,
  MeetingCreate,
  MeetingNotesRead,
  MeetingRead,
  ModelCatalog,
  ModelSettings,
  PageFolder,
  PageIdentity,
  PageMeeting,
  PageSegment,
  PageSpeaker,
  PermissionsInfo,
  RecordingSettings,
  SegmentRead,
  SettingsRead,
  SpeakerRead,
  SpeakerSettings,
  StatusInfo,
  StorageSettings,
} from "./types";

export function useMeetings(page = 1, pageSize = 50) {
  return useQuery({
    queryKey: queryKeys.meetings.list(page, pageSize),
    queryFn: () => api.get<PageMeeting>(`/api/meetings?page=${page}&page_size=${pageSize}`),
    refetchInterval: 5_000,
  });
}

// The server hard-caps page_size at 200 (routes/meetings.rs), so any meeting with more than 200
// segments spans several pages. Fetch them all — a single missing page is silent transcript loss in
// the primary view.
const SEGMENT_PAGE_SIZE = 200;

async function fetchAllSegments(meetingId: string): Promise<SegmentRead[]> {
  const items: SegmentRead[] = [];
  for (let page = 1; ; page += 1) {
    const chunk = await api.get<PageSegment>(
      `/api/meetings/${meetingId}/segments?page=${page}&page_size=${SEGMENT_PAGE_SIZE}`,
    );
    items.push(...chunk.items);
    // Stop once we've collected the reported total; the empty-page guard bounds the loop even if
    // `total` is momentarily inconsistent with the pages during a live write.
    if (items.length >= chunk.total || chunk.items.length === 0) break;
  }
  return items;
}

// Live engine readiness, for gating "Start" on the transcription sidecars having loaded their
// models. Polls fast while warming up (to catch the ready transition promptly) and keeps a slow
// keepalive once ready — never stops entirely, so the gate always reflects the current pool state
// (e.g. after a meeting stops and the pool re-warms) without depending on a manual re-trigger.
export function useStatus() {
  return useQuery({
    queryKey: queryKeys.status.all,
    queryFn: () => api.get<StatusInfo>("/api/status"),
    refetchInterval: (query) => (query.state.data?.sidecars_ready ? 10_000 : 1_500),
  });
}

export function useSegments(meetingId: string | null, isLive = false) {
  return useQuery({
    queryKey: queryKeys.meetings.segments(meetingId ?? "none"),
    queryFn: () => fetchAllSegments(meetingId as string),
    enabled: meetingId !== null,
    // While recording, poll as a belt-and-suspenders backfill: the primary recovery is the WS
    // resync/reconnect invalidation (useTranscript), but a modest refetch also catches any final a
    // lag dropped without a surviving signal. Merged non-destructively (seed replace=false while
    // live). Off once finalized -- the DB is then static and the refined transcript is authoritative.
    refetchInterval: isLive ? 15_000 : false,
  });
}

export function useStartMeeting() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: MeetingCreate) => api.post<MeetingRead>("/api/meetings", body),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.meetings.all });
      // Starting takes the warm pair; the replacement is still loading, so re-check readiness (this
      // re-arms polling for the *next* meeting's gate).
      qc.invalidateQueries({ queryKey: queryKeys.status.all });
    },
  });
}

// Stop runs the inline auto-refine (re-diarize + re-transcribe every turn; a cold Parakeet
// sidecar load alone is ~11s), so it can take far longer than the 15s default -- give it the
// same long timeout as the manual refine. Too short and the client aborts before onSuccess can
// invalidate, so the UI never refetches the refined transcript the server did finish writing.
const REFINE_TIMEOUT_MS = 600_000;

export function useStopMeeting() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) =>
      api.post<MeetingRead>(`/api/meetings/${id}/stop`, undefined, {
        timeoutMs: REFINE_TIMEOUT_MS,
      }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.meetings.all });
      // Re-check the warm pool so the gate reflects the replacement pair (usually already loaded
      // during the meeting, so the next meeting can start immediately).
      qc.invalidateQueries({ queryKey: queryKeys.status.all });
    },
  });
}

export function useDeleteMeeting() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.delete<void>(`/api/meetings/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: queryKeys.meetings.all }),
  });
}

// Rename a meeting's title. The server trims + length-checks the title and returns the updated row;
// invalidate the list so every view reflects the new title.
export function useRenameMeeting() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, title }: { id: string; title: string }) =>
      api.patch<MeetingRead>(`/api/meetings/${id}`, { title }),
    onSuccess: () => qc.invalidateQueries({ queryKey: queryKeys.meetings.all }),
  });
}

// The organizational folder tree. Fetched whole (one large page) and assembled into a tree
// client-side; folder mutations invalidate this prefix rather than refetching on a timer.
export function useFolders() {
  return useQuery({
    queryKey: queryKeys.folders.list(1, 200),
    queryFn: () => api.get<PageFolder>("/api/folders?page=1&page_size=200"),
  });
}

export function useCreateFolder() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: FolderCreate) => api.post<FolderRead>("/api/folders", body),
    onSuccess: () => qc.invalidateQueries({ queryKey: queryKeys.folders.all }),
  });
}

export function useRenameFolder() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, name }: { id: string; name: string }) =>
      api.patch<FolderRead>(`/api/folders/${id}`, { name }),
    onSuccess: () => qc.invalidateQueries({ queryKey: queryKeys.folders.all }),
  });
}

// Move a folder to a new parent (`parentId: null` moves it to the root). The server rejects a move
// into the folder itself or a descendant (422); invalidate the tree on success.
export function useReparentFolder() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, parentId }: { id: string; parentId: string | null }) =>
      api.put<FolderRead>(`/api/folders/${id}/parent`, { parent_id: parentId }),
    onSuccess: () => qc.invalidateQueries({ queryKey: queryKeys.folders.all }),
  });
}

// Delete a folder. The server removes its sub-folder subtree and un-files (never deletes) the
// meetings within, so refresh both the folder tree and the meeting list.
export function useDeleteFolder() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.delete<void>(`/api/folders/${id}`),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.folders.all });
      qc.invalidateQueries({ queryKey: queryKeys.meetings.all });
    },
  });
}

// File a meeting under a folder (`folderId: null` un-files it) — the drag-and-drop "move" action.
// The server validates the target folder (422 if missing); invalidate the meeting list so the
// sidebar re-groups.
export function useMoveMeeting() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, folderId }: { id: string; folderId: string | null }) =>
      api.put<MeetingRead>(`/api/meetings/${id}/folder`, { folder_id: folderId }),
    onSuccess: () => qc.invalidateQueries({ queryKey: queryKeys.meetings.all }),
  });
}

export function useSpeakers(meetingId: string | null) {
  return useQuery({
    queryKey: queryKeys.meetings.speakers(meetingId ?? "none"),
    queryFn: () => api.get<PageSpeaker>(`/api/meetings/${meetingId}/speakers`),
    enabled: meetingId !== null,
    refetchInterval: 5_000,
  });
}

export function useIdentities(page = 1, pageSize = 50) {
  return useQuery({
    queryKey: queryKeys.identities.list(page, pageSize),
    queryFn: () => api.get<PageIdentity>(`/api/identities?page=${page}&page_size=${pageSize}`),
  });
}

export function useRenameSpeaker(meetingId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ clusterId, displayName }: { clusterId: string; displayName: string }) =>
      api.put<SpeakerRead>(`/api/meetings/${meetingId}/speakers/${clusterId}`, {
        display_name: displayName,
      }),
    // Relabels segments server-side, so refresh the transcript + speakers + suggestions.
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.meetings.all });
      qc.invalidateQueries({ queryKey: queryKeys.identities.all });
    },
  });
}

// Post-meeting re-diarization. Slow, so it gets a long timeout; on success the transcript +
// speaker labels are rewritten server-side, so refresh both trees.
export function useRediarize(meetingId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: () =>
      api.post<PageSpeaker>(`/api/meetings/${meetingId}/rediarize`, undefined, {
        timeoutMs: REFINE_TIMEOUT_MS,
      }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.meetings.all });
      qc.invalidateQueries({ queryKey: queryKeys.identities.all });
    },
  });
}

// Persisted meeting notes (local-LLM summary + action items). The endpoint 404s when notes have
// not been generated yet, which is the normal empty state — don't retry it, and let the caller
// render the "generate" affordance rather than an error.
export function useMeetingNotes(meetingId: string | null) {
  return useQuery({
    queryKey: queryKeys.meetings.notes(meetingId ?? "none"),
    queryFn: () => api.get<MeetingNotesRead>(`/api/meetings/${meetingId}/notes`),
    enabled: meetingId !== null,
    retry: false,
  });
}

// Generate (or regenerate) a meeting's notes from its finalized transcript. A local-LLM pass (cold
// model load + generation), so it gets the same long timeout as the refine; on success the server
// persisted the row, so seed it straight into the cache.
export function useGenerateNotes(meetingId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: () =>
      api.post<MeetingNotesRead>(`/api/meetings/${meetingId}/notes`, undefined, {
        timeoutMs: REFINE_TIMEOUT_MS,
      }),
    onSuccess: (notes) => {
      qc.setQueryData<MeetingNotesRead>(queryKeys.meetings.notes(meetingId), notes);
    },
  });
}

// The notes-model catalog (curated, ungated GGUF models) annotated with which are installed.
export function useModelCatalog() {
  return useQuery({
    queryKey: queryKeys.models.catalog,
    queryFn: () => api.get<ModelCatalog>("/api/models/catalog"),
  });
}

// The single active model download's progress. Polls quickly while a download is running (to move
// the progress bar) and stops once idle/ready/errored — re-armed when a new download starts.
export function useDownloadStatus() {
  return useQuery({
    queryKey: queryKeys.models.download,
    queryFn: () => api.get<DownloadState>("/api/models/download"),
    refetchInterval: (query) => {
      const status = query.state.data?.status;
      return status === "downloading" || status === "verifying" ? 1_000 : false;
    },
  });
}

// Start downloading a catalog model by id (single-at-a-time; idempotent for an already-installed
// one). Seed the returned snapshot so polling picks up immediately.
export function useStartDownload() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.post<DownloadState>("/api/models/download", { id }),
    onSuccess: (state) => {
      qc.setQueryData<DownloadState>(queryKeys.models.download, state);
    },
  });
}

// Editable settings (the writable overlay over the env defaults).
export function useSettings() {
  return useQuery({
    queryKey: queryKeys.settings.all,
    queryFn: () => api.get<SettingsRead>("/api/settings"),
  });
}

// Live TCC permission status. Each fetch briefly spawns the capture helper, so this never
// auto-refetches: it loads when the panel mounts and only re-runs on an explicit Recheck.
export function usePermissions() {
  return useQuery({
    queryKey: queryKeys.settings.permissions,
    queryFn: () => api.get<PermissionsInfo>("/api/settings/permissions"),
    staleTime: Infinity,
    refetchOnWindowFocus: false,
  });
}

// Update the recording/privacy section; the response is the new section, so patch the cache.
export function useUpdateRecording() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: RecordingSettings) =>
      api.put<RecordingSettings>("/api/settings/recording", body),
    onSuccess: (recording) => {
      qc.setQueryData<SettingsRead>(queryKeys.settings.all, (old) =>
        old ? { ...old, recording } : old,
      );
    },
  });
}

// Update the speaker-diarization section; patch the cache with the returned section.
export function useUpdateSpeakers() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: SpeakerSettings) => api.put<SpeakerSettings>("/api/settings/speakers", body),
    onSuccess: (speakers) => {
      qc.setQueryData<SettingsRead>(queryKeys.settings.all, (old) =>
        old ? { ...old, speakers } : old,
      );
    },
  });
}

// Update the default storage location; server validates the directory (422 on bad path). Patch
// the section, then refetch so storage_info (usage, DB path) reflects the change.
export function useUpdateStorage() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: StorageSettings) => api.put<StorageSettings>("/api/settings/storage", body),
    onSuccess: (storage) => {
      qc.setQueryData<SettingsRead>(queryKeys.settings.all, (old) =>
        old ? { ...old, storage } : old,
      );
    },
  });
}

// Update the `models` section (refine whisper model + the notes toggle/model). The server validates
// any changed model file (absolute, exists, correct magic) and returns the canonicalized section, so
// a 200 means the refine model resolves; a non-empty notes model likewise resolves (empty = unset).
// Callers send the whole section — the server full-replaces it — so never omit a field you mean to
// keep. Patch the cache from the returned section.
export function useUpdateModels() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: ModelSettings) => api.put<ModelSettings>("/api/settings/models", body),
    onSuccess: (models) => {
      // Patch the section for instant input feedback, then refetch for authoritative `models_info`:
      // a notes-only change echoes the (possibly non-existent, bundled) refine path back unchanged,
      // which the server accepts without re-checking, so we can't assume either file resolves.
      qc.setQueryData<SettingsRead>(queryKeys.settings.all, (old) =>
        old ? { ...old, models } : old,
      );
      qc.invalidateQueries({ queryKey: queryKeys.settings.all });
    },
  });
}

// Clear the refine-model override, reverting to the bundled default. Separate from the PUT because
// the default may be a relative/bundled path the PUT's absolute-path validation would reject. Refetch
// settings so models_info.refine_model_exists reflects whether the default resolves on this install.
export function useResetModels() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: () => api.delete<ModelSettings>("/api/settings/models"),
    onSuccess: (models) => {
      qc.setQueryData<SettingsRead>(queryKeys.settings.all, (old) =>
        old ? { ...old, models } : old,
      );
      qc.invalidateQueries({ queryKey: queryKeys.settings.all });
    },
  });
}

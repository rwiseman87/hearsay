import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { api } from "./client";
import { queryKeys } from "./queryKeys";
import type {
  ArchiveState,
  DownloadState,
  FolderCreate,
  FolderRead,
  IdentityRead,
  MeetingCreate,
  MeetingNotesRead,
  MeetingRead,
  ModelCatalog,
  ModelSettings,
  PageFolder,
  PageIdentity,
  PageMeeting,
  PageSearchHit,
  PageSegment,
  PageSpeaker,
  PageVoiceprint,
  PermissionsInfo,
  RecordingSettings,
  SegmentRead,
  SettingsRead,
  SetupState,
  SpeakerRead,
  SpeakerSettings,
  StatusInfo,
  StorageSettings,
  UserNotesRead,
} from "./types";

export function useMeetings(page = 1, pageSize = 50) {
  return useQuery({
    queryKey: queryKeys.meetings.list(page, pageSize),
    queryFn: ({ signal }) =>
      api.get<PageMeeting>(`/api/meetings?page=${page}&page_size=${pageSize}`, signal),
    refetchInterval: 5_000,
  });
}

// The server hard-caps page_size at 200 (routes/meetings.rs), so any meeting with more than 200
// segments spans several pages. Fetch them all — a single missing page is silent transcript loss in
// the primary view.
const SEGMENT_PAGE_SIZE = 200;

async function fetchAllSegments(
  meetingId: string,
  signal?: AbortSignal,
): Promise<SegmentRead[]> {
  const items: SegmentRead[] = [];
  for (let page = 1; ; page += 1) {
    const chunk = await api.get<PageSegment>(
      `/api/meetings/${meetingId}/segments?page=${page}&page_size=${SEGMENT_PAGE_SIZE}`,
      signal,
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
    queryFn: ({ signal }) => api.get<StatusInfo>("/api/status", signal),
    refetchInterval: (query) => (query.state.data?.sidecars_ready ? 10_000 : 1_500),
  });
}

export function useSegments(meetingId: string | null, isLive = false) {
  return useQuery({
    queryKey: queryKeys.meetings.segments(meetingId ?? "none"),
    queryFn: ({ signal }) => fetchAllSegments(meetingId as string, signal),
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

// "Keep recording" from the inactivity prompt: reset the server-side silence clock so the meeting is
// not nudged again or auto-ended while the user is present. Fire-and-forget (204, no cache change);
// 404s harmlessly if the meeting already stopped.
export function useKeepRecording() {
  return useMutation({
    mutationFn: (id: string) => api.post<void>(`/api/meetings/${id}/keep-recording`),
  });
}

// Pause / resume the live meeting's capture (the "Pause" control). Fire-and-forget (204, no cache
// change) — the paused state reaches the UI over the transcript WebSocket (a `capture_state` frame),
// which is authoritative and also snapshots on reconnect. 404s harmlessly if the meeting is not live.
export function usePauseMeeting() {
  return useMutation({
    mutationFn: (id: string) => api.post<void>(`/api/meetings/${id}/pause`),
  });
}

export function useResumeMeeting() {
  return useMutation({
    mutationFn: (id: string) => api.post<void>(`/api/meetings/${id}/resume`),
  });
}

export function useDeleteMeeting() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.delete<void>(`/api/meetings/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: queryKeys.meetings.all }),
  });
}

// Open a meeting's recordings folder (audio, transcript, notes, meeting.json) in the OS file
// manager. Runs in the core (a native process), reached over the same-origin HTTP API — the same
// channel Settings' "Reveal data folder" uses, which works in the packaged app.
export function useRevealMeeting() {
  return useMutation({
    mutationFn: (id: string) => api.post<void>(`/api/meetings/${id}/reveal`),
  });
}

// Edit a transcript segment's text (fix an ASR mishearing). The server marks the segment `edited`,
// re-exports transcript.md, and returns the updated row; invalidate the meeting's segments so the
// transcript re-renders with the edit (and its "edited" badge).
export function useEditSegment(meetingId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ segmentId, text }: { segmentId: string; text: string }) =>
      api.patch<SegmentRead>(`/api/meetings/${meetingId}/segments/${segmentId}`, { text }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.meetings.segments(meetingId) });
      // The transcript changed, so any generated notes may now be stale — refresh them.
      qc.invalidateQueries({ queryKey: queryKeys.meetings.notes(meetingId) });
    },
  });
}

// Reassign one transcript line to a different speaker: pass `{ clusterId }` to move it to an existing
// speaker, or `{ displayName }` to assign it to a person by name (reused if present, else a new
// speaker). The server marks the segment `edited`, re-exports transcript.md, and returns the updated
// row; refresh the transcript + speakers (a new speaker may appear) + identities + notes.
export function useReassignSpeaker(meetingId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({
      segmentId,
      clusterId,
      displayName,
    }: {
      segmentId: string;
      clusterId?: string;
      displayName?: string;
    }) =>
      api.patch<SegmentRead>(`/api/meetings/${meetingId}/segments/${segmentId}/speaker`, {
        cluster_id: clusterId,
        display_name: displayName,
      }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.meetings.segments(meetingId) });
      qc.invalidateQueries({ queryKey: queryKeys.meetings.speakers(meetingId) });
      qc.invalidateQueries({ queryKey: queryKeys.meetings.notes(meetingId) });
      qc.invalidateQueries({ queryKey: queryKeys.identities.all });
    },
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
    queryFn: ({ signal }) => api.get<PageFolder>("/api/folders?page=1&page_size=200", signal),
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

export function useSpeakers(meetingId: string | null, isLive = false) {
  return useQuery({
    queryKey: queryKeys.meetings.speakers(meetingId ?? "none"),
    queryFn: ({ signal }) => api.get<PageSpeaker>(`/api/meetings/${meetingId}/speakers`, signal),
    enabled: meetingId !== null,
    // Poll only while recording: new Them clusters appear server-side with no WS notification. Once
    // finalized the speaker set only changes on rename/refine, both of which already invalidate, so a
    // standing 5s poll for the life of the view is wasted.
    refetchInterval: isLive ? 5_000 : false,
  });
}

export function useIdentities(page = 1, pageSize = 50) {
  return useQuery({
    queryKey: queryKeys.identities.list(page, pageSize),
    queryFn: ({ signal }) =>
      api.get<PageIdentity>(`/api/identities?page=${page}&page_size=${pageSize}`, signal),
  });
}

export function useRenameSpeaker(meetingId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ clusterId, displayName }: { clusterId: string; displayName: string }) =>
      api.put<SpeakerRead>(`/api/meetings/${meetingId}/speakers/${clusterId}`, {
        display_name: displayName,
      }),
    // Relabels this meeting's segments server-side (and the labels feed generated notes), so refresh
    // just the affected meeting's transcript + speakers + notes + the identity suggestions — not the
    // whole `["meetings"]` prefix (every meeting's list/segments/speakers/notes), which the sibling
    // useReassignSpeaker already scopes correctly.
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.meetings.segments(meetingId) });
      qc.invalidateQueries({ queryKey: queryKeys.meetings.speakers(meetingId) });
      qc.invalidateQueries({ queryKey: queryKeys.meetings.notes(meetingId) });
      qc.invalidateQueries({ queryKey: queryKeys.identities.all });
    },
  });
}

// Fold one of a meeting's speakers into another: the source's lines move onto the target and the
// source cluster is deleted. Scoped to this meeting, so — unlike a rename — no identity is created
// or renamed and `identities` stays valid.
export function useMergeSpeakers(meetingId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ clusterId, into }: { clusterId: string; into: string }) =>
      api.post<PageSpeaker>(`/api/meetings/${meetingId}/speakers/${clusterId}/merge`, {
        into,
      }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.meetings.segments(meetingId) });
      qc.invalidateQueries({ queryKey: queryKeys.meetings.speakers(meetingId) });
      qc.invalidateQueries({ queryKey: queryKeys.meetings.notes(meetingId) });
      // A merge discards the source cluster's stored voice sample.
      qc.invalidateQueries({ queryKey: queryKeys.voiceprints.all });
    },
  });
}

// The stored-voiceprint roster: every known person with the per-meeting voice samples recognition
// matches against. People named but never refined appear with no samples.
export function useVoiceprints(page = 1, pageSize = 50) {
  return useQuery({
    queryKey: queryKeys.voiceprints.list(page, pageSize),
    queryFn: ({ signal }) =>
      api.get<PageVoiceprint>(`/api/voiceprints?page=${page}&page_size=${pageSize}`, signal),
  });
}

// Rename a person everywhere they appear. Unlike useRenameSpeaker, this rewrites speaker labels
// across every meeting that person is in, so the narrow per-meeting scoping is wrong here — the
// whole `["meetings"]` tree is stale.
export function useRenameIdentity() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ identityId, displayName }: { identityId: string; displayName: string }) =>
      api.patch<IdentityRead>(`/api/identities/${identityId}`, { display_name: displayName }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.voiceprints.all });
      qc.invalidateQueries({ queryKey: queryKeys.identities.all });
      qc.invalidateQueries({ queryKey: queryKeys.meetings.all });
    },
  });
}

// Forget one stored voice sample. Only `clusters.centroid` is cleared, so names, labels and past
// transcripts are untouched and no meeting query goes stale.
export function useDeleteVoiceprint() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (clusterId: string) => api.delete<void>(`/api/voiceprints/${clusterId}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: queryKeys.voiceprints.all }),
  });
}

// Forget every voice sample stored for a person; their name stays on every past transcript.
export function useForgetVoice() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (identityId: string) =>
      api.delete<void>(`/api/identities/${identityId}/voiceprint`),
    onSuccess: () => qc.invalidateQueries({ queryKey: queryKeys.voiceprints.all }),
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

// Persisted meeting notes (local-LLM Markdown, stored verbatim). The endpoint 404s when notes have
// not been generated yet, which is the normal empty state — don't retry it, and let the caller
// render the "generate" affordance rather than an error.
export function useMeetingNotes(meetingId: string | null) {
  return useQuery({
    queryKey: queryKeys.meetings.notes(meetingId ?? "none"),
    queryFn: ({ signal }) => api.get<MeetingNotesRead>(`/api/meetings/${meetingId}/notes`, signal),
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

// Edit a meeting's notes (the verbatim Markdown content). The server marks them `edited`, re-exports
// notes.md, and returns the updated row (with `stale: false`); seed it into the cache.
export function useEditNotes(meetingId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: { content: string }) =>
      api.patch<MeetingNotesRead>(`/api/meetings/${meetingId}/notes`, body),
    onSuccess: (notes) => {
      qc.setQueryData<MeetingNotesRead>(queryKeys.meetings.notes(meetingId), notes);
    },
  });
}

// A meeting's user-authored "My notes" body. The endpoint 404s when nothing has been typed yet,
// which is the normal empty state — don't retry it, and let the caller start with a blank editor.
export function useUserNotes(meetingId: string | null) {
  return useQuery({
    queryKey: queryKeys.meetings.userNotes(meetingId ?? "none"),
    queryFn: ({ signal }) => api.get<UserNotesRead>(`/api/meetings/${meetingId}/user-notes`, signal),
    enabled: meetingId !== null,
    retry: false,
  });
}

// Autosave the user's "My notes" body. Called from a debounced effect; on success the server echoes
// the stored row, so seed it straight into the cache (no refetch, no flicker).
export function useSaveUserNotes(meetingId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: string) =>
      api.put<UserNotesRead>(`/api/meetings/${meetingId}/user-notes`, { body }),
    onSuccess: (notes) => {
      qc.setQueryData<UserNotesRead>(queryKeys.meetings.userNotes(meetingId), notes);
    },
  });
}

// Full-text transcript search across every meeting. Enabled only for a non-empty query; the caller
// debounces the input so keystrokes don't each hit the endpoint.
export function useSearch(query: string) {
  const q = query.trim();
  return useQuery({
    queryKey: queryKeys.search.query(q),
    queryFn: ({ signal }) =>
      api.get<PageSearchHit>(`/api/search?q=${encodeURIComponent(q)}&page=1&page_size=50`, signal),
    enabled: q.length > 0,
  });
}

// The notes-model catalog (curated, ungated GGUF models) annotated with which are installed.
export function useModelCatalog() {
  return useQuery({
    queryKey: queryKeys.models.catalog,
    queryFn: ({ signal }) => api.get<ModelCatalog>("/api/models/catalog", signal),
  });
}

// First-run model setup: whether models are still missing, and a run's per-step progress. Polls
// while a run is going (to move the progress bar) and while the setup screen is up; once nothing is
// required it stops, since only a run changes that and the app has moved on.
export function useSetup() {
  return useQuery({
    queryKey: queryKeys.setup.all,
    queryFn: ({ signal }) => api.get<SetupState>("/api/setup", signal),
    refetchInterval: (query) => {
      const state = query.state.data;
      if (state?.status === "running") return 1_000;
      return state?.required ? 5_000 : false;
    },
  });
}

// Start (or retry) first-run setup, optionally downloading a notes model in the same pass. Seed the
// returned snapshot so the progress poll picks up immediately.
export function useStartSetup() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (notesModelId: string | null) =>
      api.post<SetupState>("/api/setup", { notes_model_id: notesModelId }),
    onSuccess: (state) => {
      qc.setQueryData<SetupState>(queryKeys.setup.all, state);
    },
  });
}

// The single active model download's progress. Polls quickly while a download is running (to move
// the progress bar) and stops once idle/ready/errored — re-armed when a new download starts.
export function useDownloadStatus() {
  return useQuery({
    queryKey: queryKeys.models.download,
    queryFn: ({ signal }) => api.get<DownloadState>("/api/models/download", signal),
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
    queryFn: ({ signal }) => api.get<SettingsRead>("/api/settings", signal),
  });
}

// Live TCC permission status. Each fetch briefly spawns the capture helper, so this never
// auto-refetches: it loads when the panel mounts and only re-runs on an explicit Recheck.
export function usePermissions() {
  return useQuery({
    queryKey: queryKeys.settings.permissions,
    queryFn: ({ signal }) => api.get<PermissionsInfo>("/api/settings/permissions", signal),
    staleTime: Infinity,
    refetchOnWindowFocus: false,
  });
}

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

// Update the `storage` section (recordings location + the audio-archival policy). The server
// validates the directory (422 on bad path) and full-replaces the section, so callers send every
// field — never omit one you mean to keep. Patch the section from the response, then invalidate so
// `storage_info` (usage, uncompressed bytes) is refetched rather than left stale.
export function useUpdateStorage() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: StorageSettings) => api.put<StorageSettings>("/api/settings/storage", body),
    onSuccess: (storage) => {
      qc.setQueryData<SettingsRead>(queryKeys.settings.all, (old) =>
        old ? { ...old, storage } : old,
      );
      void qc.invalidateQueries({ queryKey: queryKeys.settings.all });
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

// Progress of the audio-archival pass, whether started by the button below or by the periodic
// sweep. Polls while a pass is running and stops once it finishes, mirroring useDownloadStatus.
export function useArchiveStatus() {
  return useQuery({
    queryKey: queryKeys.settings.archive,
    queryFn: ({ signal }) => api.get<ArchiveState>("/api/settings/storage/compress", signal),
    refetchInterval: (query) => (query.state.data?.running ? 1_000 : false),
  });
}

// Run the archival pass now rather than waiting for the periodic sweep. The server answers 202 and
// works in the background (409 while a meeting is recording, or if a pass is already running), so
// seed the cache with the returned snapshot to start the poll immediately, and refresh the settings
// once it finishes so the reclaimed bytes are reflected.
export function useCompressNow() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: () => api.post<ArchiveState>("/api/settings/storage/compress", {}),
    onSuccess: (state) => {
      qc.setQueryData<ArchiveState>(queryKeys.settings.archive, state);
    },
  });
}

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { api } from "./client";
import { queryKeys } from "./queryKeys";
import type {
  MeetingCreate,
  MeetingRead,
  ModelSettings,
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

export function useSegments(meetingId: string | null) {
  return useQuery({
    queryKey: queryKeys.meetings.segments(meetingId ?? "none"),
    queryFn: () => fetchAllSegments(meetingId as string),
    enabled: meetingId !== null,
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

// Update the offline-refine whisper model path; server validates the file (absolute, exists, GGML
// magic) and returns the canonicalized path. Patch the section, and mark the file as resolving
// since the server only returns 200 for a model it verified on disk.
export function useUpdateModels() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: ModelSettings) => api.put<ModelSettings>("/api/settings/models", body),
    onSuccess: (models) => {
      qc.setQueryData<SettingsRead>(queryKeys.settings.all, (old) =>
        old
          ? { ...old, models, models_info: { ...old.models_info, refine_model_exists: true } }
          : old,
      );
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

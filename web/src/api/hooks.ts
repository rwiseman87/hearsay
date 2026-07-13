import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { api } from "./client";
import { queryKeys } from "./queryKeys";
import type {
  MeetingCreate,
  MeetingRead,
  PageIdentity,
  PageMeeting,
  PageSegment,
  PageSpeaker,
  RecordingSettings,
  SettingsRead,
  SpeakerRead,
} from "./types";

export function useMeetings(page = 1, pageSize = 50) {
  return useQuery({
    queryKey: queryKeys.meetings.list(page, pageSize),
    queryFn: () => api.get<PageMeeting>(`/api/meetings?page=${page}&page_size=${pageSize}`),
    refetchInterval: 5_000,
  });
}

export function useSegments(meetingId: string | null, page = 1, pageSize = 200) {
  return useQuery({
    queryKey: queryKeys.meetings.segments(meetingId ?? "none", page, pageSize),
    queryFn: () =>
      api.get<PageSegment>(`/api/meetings/${meetingId}/segments?page=${page}&page_size=${pageSize}`),
    enabled: meetingId !== null,
  });
}

export function useStartMeeting() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: MeetingCreate) => api.post<MeetingRead>("/api/meetings", body),
    onSuccess: () => qc.invalidateQueries({ queryKey: queryKeys.meetings.all }),
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
    onSuccess: () => qc.invalidateQueries({ queryKey: queryKeys.meetings.all }),
  });
}

export function useDeleteMeeting() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.delete<void>(`/api/meetings/${id}`),
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

// Re-point a meeting's storage to a directory its artifacts were moved to. The server validates
// (does not move files); on success the stored root changes, so refresh the meetings tree.
export function useRelocateMeeting(meetingId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (newRoot: string) =>
      api.put<MeetingRead>(`/api/meetings/${meetingId}/storage`, { new_root: newRoot }),
    onSuccess: () => qc.invalidateQueries({ queryKey: queryKeys.meetings.all }),
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

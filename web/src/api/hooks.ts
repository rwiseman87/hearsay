import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { api } from "./client";
import { queryKeys } from "./queryKeys";
import type {
  ASRSelect,
  ASRStatus,
  MeetingCreate,
  MeetingRead,
  PageIdentity,
  PageMeeting,
  PageSegment,
  PageSpeaker,
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

export function useStopMeeting() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.post<MeetingRead>(`/api/meetings/${id}/stop`),
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

export function useAsrStatus() {
  return useQuery({
    queryKey: queryKeys.asr.status,
    queryFn: () => api.get<ASRStatus>("/api/asr/models"),
  });
}

export function useSetAsrModel() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: ASRSelect) => api.put<ASRStatus>("/api/asr/model", body),
    onSuccess: (data) => qc.setQueryData(queryKeys.asr.status, data),
  });
}

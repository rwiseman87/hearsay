// Convenience aliases over the OpenAPI-generated schema (see schema.ts, produced
// by `make codegen`). Import app types from here, not from schema.ts directly.
import type { components } from "./schema";

type Schemas = components["schemas"];

export type MeetingRead = Schemas["MeetingRead"];
export type MeetingCreate = Schemas["MeetingCreate"];
export type FolderRead = Schemas["FolderRead"];
export type FolderCreate = Schemas["FolderCreate"];
export type PageFolder = Schemas["Page_FolderRead"];
export type SegmentRead = Schemas["SegmentRead"];
export type SegmentSpeakerAssign = Schemas["SegmentSpeakerAssign"];
export type PageMeeting = Schemas["Page_MeetingRead"];
export type PageSegment = Schemas["Page_SegmentRead"];
export type SearchHit = Schemas["SearchHit"];
export type PageSearchHit = Schemas["Page_SearchHit"];
export type SpeakerRead = Schemas["SpeakerRead"];
export type SpeakerMerge = Schemas["SpeakerMerge"];
export type PageSpeaker = Schemas["Page_SpeakerRead"];
export type PageIdentity = Schemas["Page_IdentityRead"];
export type IdentityRead = Schemas["IdentityRead"];
export type VoiceprintRead = Schemas["VoiceprintRead"];
export type VoiceprintSampleRead = Schemas["VoiceprintSampleRead"];
export type PageVoiceprint = Schemas["Page_VoiceprintRead"];
export type SettingsRead = Schemas["SettingsRead"];
export type RecordingSettings = Schemas["RecordingSettings"];
export type SpeakerSettings = Schemas["SpeakerSettings"];
export type StorageSettings = Schemas["StorageSettings"];
export type ModelSettings = Schemas["ModelSettings"];
export type MeetingNotesRead = Schemas["MeetingNotesRead"];
export type UserNotesRead = Schemas["UserNotesRead"];
export type UserNotesWrite = Schemas["UserNotesWrite"];
export type ModelCatalog = Schemas["ModelCatalog"];
export type DownloadState = Schemas["DownloadState"];
export type PermissionsInfo = Schemas["PermissionsInfo"];
export type StatusInfo = Schemas["StatusInfo"];

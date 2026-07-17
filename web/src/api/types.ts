// Convenience aliases over the OpenAPI-generated schema (see schema.ts, produced
// by `make codegen`). Import app types from here, not from schema.ts directly.
import type { components } from "./schema";

type Schemas = components["schemas"];

export type MeetingRead = Schemas["MeetingRead"];
export type MeetingCreate = Schemas["MeetingCreate"];
export type MeetingUpdate = Schemas["MeetingUpdate"];
export type MeetingFolderAssign = Schemas["MeetingFolderAssign"];
export type FolderRead = Schemas["FolderRead"];
export type FolderCreate = Schemas["FolderCreate"];
export type FolderUpdate = Schemas["FolderUpdate"];
export type FolderReparent = Schemas["FolderReparent"];
export type PageFolder = Schemas["Page_FolderRead"];
export type SegmentRead = Schemas["SegmentRead"];
export type Stream = Schemas["Stream"];
export type PageMeeting = Schemas["Page_MeetingRead"];
export type PageSegment = Schemas["Page_SegmentRead"];
export type SearchHit = Schemas["SearchHit"];
export type PageSearchHit = Schemas["Page_SearchHit"];
export type SpeakerRead = Schemas["SpeakerRead"];
export type PageSpeaker = Schemas["Page_SpeakerRead"];
export type PageIdentity = Schemas["Page_IdentityRead"];
export type SettingsRead = Schemas["SettingsRead"];
export type RecordingSettings = Schemas["RecordingSettings"];
export type SpeakerSettings = Schemas["SpeakerSettings"];
export type StorageSettings = Schemas["StorageSettings"];
export type StorageInfo = Schemas["StorageInfo"];
export type ModelSettings = Schemas["ModelSettings"];
export type ModelsInfo = Schemas["ModelsInfo"];
export type MeetingNotesRead = Schemas["MeetingNotesRead"];
export type CatalogEntry = Schemas["CatalogEntry"];
export type ModelCatalog = Schemas["ModelCatalog"];
export type DownloadState = Schemas["DownloadState"];
export type DownloadStatus = Schemas["DownloadStatus"];
export type AboutInfo = Schemas["AboutInfo"];
export type PermissionsInfo = Schemas["PermissionsInfo"];
export type StatusInfo = Schemas["StatusInfo"];

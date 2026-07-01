// Convenience aliases over the OpenAPI-generated schema (see schema.ts, produced
// by `make codegen`). Import app types from here, not from schema.ts directly.
import type { components } from "./schema";

type Schemas = components["schemas"];

export type MeetingRead = Schemas["MeetingRead"];
export type MeetingCreate = Schemas["MeetingCreate"];
export type SegmentRead = Schemas["SegmentRead"];
export type Stream = Schemas["Stream"];
export type PageMeeting = Schemas["Page_MeetingRead_"];
export type PageSegment = Schemas["Page_SegmentRead_"];
export type SpeakerRead = Schemas["SpeakerRead"];
export type PageSpeaker = Schemas["Page_SpeakerRead_"];
export type PageIdentity = Schemas["Page_IdentityRead_"];

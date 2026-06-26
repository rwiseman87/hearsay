"""Business logic; API routers delegate here and stay thin."""

from __future__ import annotations

from hearsay.services.meetings import MeetingService, meeting_folder_name, slugify
from hearsay.services.speakers import SpeakerService

__all__ = ["MeetingService", "SpeakerService", "meeting_folder_name", "slugify"]

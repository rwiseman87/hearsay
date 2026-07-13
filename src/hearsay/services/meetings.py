"""Meeting + segment persistence logic.

Routers stay thin and delegate here. Each method is a single logical write and
commits once; multi-step writes (none yet) would wrap an explicit transaction.
"""

from __future__ import annotations

import asyncio
import re
from collections.abc import Sequence
from datetime import UTC, datetime
from pathlib import Path
from uuid import UUID

from sqlalchemy import func, select
from sqlalchemy.ext.asyncio import AsyncSession

from hearsay.enums import AssetKind, MeetingStatus, Stream
from hearsay.models import Meeting, MeetingAsset, Segment

_SLUG_STRIP = re.compile(r"[^a-z0-9]+")


class MeetingRelocationError(RuntimeError):
    """A meeting's storage could not be re-pointed: bad target path or missing artifacts.

    ``missing`` lists the artifact ``rel_path``\\ s that were absent/unreadable at the target,
    so the caller can tell the user exactly which files it could not find.
    """

    def __init__(self, message: str, *, missing: Sequence[str] = ()) -> None:
        super().__init__(message)
        self.missing = list(missing)


# The known per-meeting artifacts, keyed by their filename in the meeting folder. ``audio.wav``
# is present only when audio.record was on for that meeting, so the manifest -- not the current
# setting -- is the source of truth for which files a meeting actually has.
_KNOWN_ASSETS: tuple[tuple[str, AssetKind], ...] = (
    ("transcript.md", AssetKind.TRANSCRIPT),
    ("meeting.json", AssetKind.METADATA),
    ("audio.wav", AssetKind.AUDIO),
)


def _scan_assets(directory: Path) -> dict[str, tuple[AssetKind, int]]:
    """Enumerate the known artifacts present in a meeting folder: ``rel_path -> (kind, size)``."""
    found: dict[str, tuple[AssetKind, int]] = {}
    for name, kind in _KNOWN_ASSETS:
        try:
            size = (directory / name).stat().st_size
        except OSError:  # missing or unreadable -> not part of the manifest
            continue
        found[name] = (kind, size)
    return found


def _missing_artifacts(directory: Path, expected: Sequence[str]) -> list[str]:
    """Which of the ``expected`` relative paths are absent or unreadable under ``directory``."""
    missing: list[str] = []
    for rel_path in expected:
        try:
            with (directory / rel_path).open("rb"):  # verifies existence + read access
                pass
        except OSError:
            missing.append(rel_path)
    return missing


def slugify(title: str) -> str:
    slug = _SLUG_STRIP.sub("-", title.lower()).strip("-")
    return slug or "meeting"


def meeting_folder_name(title: str, when: datetime) -> str:
    """``<YYYY-MM-DD_HHMM>_<slug>`` — the per-meeting on-disk folder name."""
    return f"{when:%Y-%m-%d_%H%M}_{slugify(title)}"


def meeting_dir(meeting: Meeting) -> Path:
    """The meeting's on-disk folder: its stamped ``storage_root`` joined with ``folder``.

    Uses the root captured at creation, so a later change to the output-dir setting never
    repoints an existing meeting away from where its artifacts were written.
    """
    return Path(meeting.storage_root) / meeting.folder


class MeetingService:
    def __init__(self, session: AsyncSession) -> None:
        self._session = session

    async def create(
        self, *, title: str, folder: str, storage_root: str, started_at: datetime
    ) -> Meeting:
        meeting = Meeting(
            title=title, folder=folder, storage_root=storage_root, started_at=started_at
        )
        self._session.add(meeting)
        await self._session.commit()
        await self._session.refresh(meeting)
        return meeting

    async def get(self, meeting_id: UUID) -> Meeting | None:
        return await self._session.get(Meeting, meeting_id)

    async def sync_manifest(self, meeting: Meeting) -> list[MeetingAsset]:
        """Reconcile the meeting's asset manifest with what is actually on disk.

        Scans the meeting folder, upserts a row for each known artifact present (updating its
        size), and drops rows for files that are gone, so the manifest mirrors the meeting's
        storage. Idempotent; safe to call at every finalize/refine.
        """
        found = await asyncio.to_thread(_scan_assets, meeting_dir(meeting))
        rows = await self._session.scalars(
            select(MeetingAsset).where(MeetingAsset.meeting_id == meeting.id)
        )
        existing = {asset.rel_path: asset for asset in rows}
        for rel_path, (kind, size) in found.items():
            asset = existing.get(rel_path)
            if asset is None:
                self._session.add(
                    MeetingAsset(
                        meeting_id=meeting.id, kind=kind, rel_path=rel_path, size_bytes=size
                    )
                )
            else:
                asset.kind = kind
                asset.size_bytes = size
        for rel_path, asset in existing.items():
            if rel_path not in found:
                await self._session.delete(asset)
        await self._session.commit()
        result = await self._session.scalars(
            select(MeetingAsset)
            .where(MeetingAsset.meeting_id == meeting.id)
            .order_by(MeetingAsset.rel_path)
        )
        return list(result)

    async def relocate(self, meeting: Meeting, new_root: str) -> Meeting:
        """Re-point the meeting's storage to ``new_root`` after confirming its artifacts are there.

        Does not move files -- the caller has already moved them; this validates that every tracked
        artifact is readable at ``new_root/<folder>`` and updates the stamped ``storage_root`` (then
        re-syncs the manifest at the new location). Raises :class:`MeetingRelocationError` (with the
        missing files) when the target is not an absolute directory holding the artifacts.
        """
        target_root = Path(new_root).expanduser()
        if not target_root.is_absolute():
            raise MeetingRelocationError("new_root must be an absolute path")
        normalized = str(target_root.resolve())
        candidate = Path(normalized) / meeting.folder

        # What we expect to find: the manifest, or -- for a meeting recorded before manifests
        # existed -- one rebuilt from its current location, or the transcript as a last resort.
        expected = [asset.rel_path for asset in await self._meeting_assets(meeting.id)]
        if not expected:
            expected = [asset.rel_path for asset in await self.sync_manifest(meeting)]
        if not expected:
            expected = ["transcript.md"]

        missing = await asyncio.to_thread(_missing_artifacts, candidate, expected)
        if missing:
            raise MeetingRelocationError(
                f"target {candidate} is missing meeting artifacts", missing=missing
            )

        meeting.storage_root = normalized
        await self._session.commit()
        await self._session.refresh(meeting)
        await self.sync_manifest(meeting)  # re-scan sizes/presence at the new location
        return meeting

    async def _meeting_assets(self, meeting_id: UUID) -> list[MeetingAsset]:
        rows = await self._session.scalars(
            select(MeetingAsset).where(MeetingAsset.meeting_id == meeting_id)
        )
        return list(rows)

    async def list_meetings(self, *, page: int, page_size: int) -> tuple[list[Meeting], int]:
        total = await self._session.scalar(select(func.count()).select_from(Meeting)) or 0
        rows = await self._session.scalars(
            select(Meeting)
            .order_by(Meeting.started_at.desc())
            .offset((page - 1) * page_size)
            .limit(page_size)
        )
        return list(rows), total

    async def list_segments(
        self, meeting_id: UUID, *, page: int, page_size: int
    ) -> tuple[list[Segment], int]:
        where = Segment.meeting_id == meeting_id
        total = (
            await self._session.scalar(select(func.count()).select_from(Segment).where(where)) or 0
        )
        rows = await self._session.scalars(
            select(Segment)
            .where(where)
            .order_by(Segment.start_s)
            .offset((page - 1) * page_size)
            .limit(page_size)
        )
        return list(rows), total

    async def add_segment(
        self,
        meeting_id: UUID,
        *,
        stream: Stream,
        speaker_label: str,
        text: str,
        start_s: float,
        end_s: float,
        cluster_id: UUID | None = None,
    ) -> Segment:
        segment = Segment(
            meeting_id=meeting_id,
            stream=stream,
            speaker_label=speaker_label,
            text=text,
            start_s=start_s,
            end_s=end_s,
            cluster_id=cluster_id,
        )
        self._session.add(segment)
        await self._session.commit()
        await self._session.refresh(segment)
        return segment

    async def finalize(self, meeting_id: UUID) -> Meeting | None:
        meeting = await self.get(meeting_id)
        if meeting is None:
            return None
        meeting.status = MeetingStatus.FINALIZED
        meeting.ended_at = datetime.now(UTC)
        await self._session.commit()
        await self._session.refresh(meeting)
        return meeting

    async def delete(self, meeting_id: UUID) -> bool:
        meeting = await self.get(meeting_id)
        if meeting is None:
            return False
        await self._session.delete(meeting)
        await self._session.commit()
        return True

"""Speakers (diarization clusters) + identities: list and rename. Routers stay thin."""

from __future__ import annotations

from typing import Annotated
from uuid import UUID

from fastapi import APIRouter, Depends, HTTPException, Query, status

from hearsay.api.deps import ContextDep, ManagerDep, SessionDep, require_token
from hearsay.models import Cluster
from hearsay.schemas import IdentityRead, Page, SpeakerRead, SpeakerRename
from hearsay.services import MeetingService, SpeakerService
from hearsay.transcript import RefineError, build_recognition_embedder, rediarize_meeting

router = APIRouter(tags=["speakers"], dependencies=[Depends(require_token)])

PageParam = Annotated[int, Query(ge=1)]
PageSizeParam = Annotated[int, Query(ge=1, le=200)]


def _speaker_read(cluster: Cluster) -> SpeakerRead:
    label = (
        cluster.identity.display_name
        if cluster.identity is not None
        else f"Speaker {cluster.ordinal}"
    )
    return SpeakerRead(
        id=cluster.id,
        ordinal=cluster.ordinal,
        label=label,
        identity_id=cluster.identity_id,
        locked=cluster.locked,
    )


@router.get("/meetings/{meeting_id}/speakers", response_model=Page[SpeakerRead])
async def list_speakers(meeting_id: UUID, session: SessionDep) -> Page[SpeakerRead]:
    clusters = await SpeakerService(session).list_clusters(meeting_id)
    items = [_speaker_read(cluster) for cluster in clusters]
    return Page[SpeakerRead](total=len(items), page=1, page_size=max(len(items), 1), items=items)


@router.put("/meetings/{meeting_id}/speakers/{cluster_id}", response_model=SpeakerRead)
async def rename_speaker(
    meeting_id: UUID, cluster_id: UUID, body: SpeakerRename, manager: ManagerDep
) -> SpeakerRead:
    cluster = await manager.relabel_speaker(meeting_id, cluster_id, body.display_name)
    if cluster is None:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="speaker not found")
    return _speaker_read(cluster)


@router.post("/meetings/{meeting_id}/rediarize", response_model=Page[SpeakerRead])
async def rediarize(meeting_id: UUID, context: ContextDep) -> Page[SpeakerRead]:
    """Re-diarize the recorded Them track (FluidAudio on the ANE) and return the new speakers.

    The heavy diarization runs in the helper, off-loop. 404 if the meeting is
    unknown, 409 if it has no recorded ``them.wav`` (diarization.refine was off)."""
    async with context.database.session() as session:
        if await MeetingService(session).get(meeting_id) is None:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="meeting not found")
    try:
        embedder = build_recognition_embedder(context.settings)
        await rediarize_meeting(
            meeting_id, database=context.database, settings=context.settings, embedder=embedder
        )
    except RefineError as exc:
        raise HTTPException(status_code=status.HTTP_409_CONFLICT, detail=str(exc)) from exc
    async with context.database.session() as session:
        clusters = await SpeakerService(session).list_clusters(meeting_id)
    items = [_speaker_read(cluster) for cluster in clusters]
    return Page[SpeakerRead](total=len(items), page=1, page_size=max(len(items), 1), items=items)


@router.get("/identities", response_model=Page[IdentityRead])
async def list_identities(
    session: SessionDep, page: PageParam = 1, page_size: PageSizeParam = 50
) -> Page[IdentityRead]:
    items, total = await SpeakerService(session).list_identities(page=page, page_size=page_size)
    return Page[IdentityRead](
        total=total,
        page=page,
        page_size=page_size,
        items=[IdentityRead.model_validate(identity) for identity in items],
    )

"""Meetings REST router. Routers stay thin: validate, call a service, return."""

from __future__ import annotations

from typing import Annotated
from uuid import UUID

from fastapi import APIRouter, Depends, HTTPException, Query, status

from hearsay.api.deps import ManagerDep, SessionDep, require_token
from hearsay.schemas import MeetingCreate, MeetingRead, MeetingRelocate, Page, SegmentRead
from hearsay.services import MeetingRelocationError, MeetingService
from hearsay.transcript import SessionBusyError

router = APIRouter(prefix="/meetings", tags=["meetings"], dependencies=[Depends(require_token)])

PageParam = Annotated[int, Query(ge=1)]
PageSizeParam = Annotated[int, Query(ge=1, le=200)]


@router.post("", response_model=MeetingRead, status_code=status.HTTP_201_CREATED)
async def start_meeting(body: MeetingCreate, manager: ManagerDep) -> MeetingRead:
    try:
        meeting = await manager.start_meeting(title=body.title)
    except SessionBusyError as exc:
        raise HTTPException(status_code=status.HTTP_409_CONFLICT, detail=str(exc)) from exc
    return MeetingRead.model_validate(meeting)


@router.get("", response_model=Page[MeetingRead])
async def list_meetings(
    session: SessionDep, page: PageParam = 1, page_size: PageSizeParam = 50
) -> Page[MeetingRead]:
    items, total = await MeetingService(session).list_meetings(page=page, page_size=page_size)
    return Page[MeetingRead](
        total=total,
        page=page,
        page_size=page_size,
        items=[MeetingRead.model_validate(m) for m in items],
    )


@router.get("/{meeting_id}", response_model=MeetingRead)
async def get_meeting(meeting_id: UUID, session: SessionDep) -> MeetingRead:
    meeting = await MeetingService(session).get(meeting_id)
    if meeting is None:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="meeting not found")
    return MeetingRead.model_validate(meeting)


@router.get("/{meeting_id}/segments", response_model=Page[SegmentRead])
async def list_segments(
    meeting_id: UUID, session: SessionDep, page: PageParam = 1, page_size: PageSizeParam = 200
) -> Page[SegmentRead]:
    items, total = await MeetingService(session).list_segments(
        meeting_id, page=page, page_size=page_size
    )
    return Page[SegmentRead](
        total=total,
        page=page,
        page_size=page_size,
        items=[SegmentRead.model_validate(s) for s in items],
    )


@router.post("/{meeting_id}/stop", response_model=MeetingRead)
async def stop_meeting(meeting_id: UUID, manager: ManagerDep) -> MeetingRead:
    meeting = await manager.stop_meeting(meeting_id)
    if meeting is None:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="meeting not found")
    return MeetingRead.model_validate(meeting)


@router.put("/{meeting_id}/storage", response_model=MeetingRead)
async def relocate_meeting(
    meeting_id: UUID, body: MeetingRelocate, manager: ManagerDep
) -> MeetingRead:
    """Re-point a meeting's storage to a directory its artifacts were moved to (validate, don't
    move). 409 while it is recording; 422 if the target does not hold the artifacts."""
    try:
        meeting = await manager.relocate_meeting(meeting_id, body.new_root)
    except SessionBusyError as exc:
        raise HTTPException(status_code=status.HTTP_409_CONFLICT, detail=str(exc)) from exc
    except MeetingRelocationError as exc:
        raise HTTPException(
            status_code=status.HTTP_422_UNPROCESSABLE_CONTENT,
            detail={"message": str(exc), "missing": exc.missing},
        ) from exc
    if meeting is None:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="meeting not found")
    return MeetingRead.model_validate(meeting)


@router.delete("/{meeting_id}", status_code=status.HTTP_204_NO_CONTENT)
async def delete_meeting(meeting_id: UUID, manager: ManagerDep) -> None:
    if not await manager.delete_meeting(meeting_id):
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="meeting not found")

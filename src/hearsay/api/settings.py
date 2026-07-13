"""Settings REST router. Routers stay thin: validate, call a service, return.

Reads/writes the editable-preferences overlay (``SettingsService``). One section per panel;
``GET`` returns the effective values, ``PUT /{section}`` updates one section.
"""

from __future__ import annotations

from fastapi import APIRouter, Depends, HTTPException, status

from hearsay.api.deps import ContextDep, SessionDep, require_token
from hearsay.schemas import RecordingSettings, SettingsRead, SpeakerSettings, StorageSettings
from hearsay.services import SettingsService, SettingsValidationError

router = APIRouter(prefix="/settings", tags=["settings"], dependencies=[Depends(require_token)])


@router.get("", response_model=SettingsRead)
async def read_settings(context: ContextDep, session: SessionDep) -> SettingsRead:
    return await SettingsService(session).read_all(context.settings)


@router.put("/recording", response_model=RecordingSettings)
async def update_recording(body: RecordingSettings, session: SessionDep) -> RecordingSettings:
    return await SettingsService(session).set_recording(body)


@router.put("/speakers", response_model=SpeakerSettings)
async def update_speakers(body: SpeakerSettings, session: SessionDep) -> SpeakerSettings:
    return await SettingsService(session).set_speakers(body)


@router.put("/storage", response_model=StorageSettings)
async def update_storage(body: StorageSettings, session: SessionDep) -> StorageSettings:
    try:
        return await SettingsService(session).set_storage(body)
    except SettingsValidationError as exc:
        raise HTTPException(
            status_code=status.HTTP_422_UNPROCESSABLE_CONTENT, detail=str(exc)
        ) from exc

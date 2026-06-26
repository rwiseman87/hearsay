"""ASR model picker router: list curated/installed models and switch the active one.

Switching updates process settings; it takes effect for the next meeting (a running
meeting keeps the backend it started with).
"""

from __future__ import annotations

from fastapi import APIRouter, Depends

from hearsay.api.context import AppContext
from hearsay.api.deps import ContextDep, require_token
from hearsay.asr import available_models
from hearsay.schemas.asr import ASRSelect, ASRStatus, ModelInfoRead

router = APIRouter(prefix="/asr", tags=["asr"], dependencies=[Depends(require_token)])


def _status(context: AppContext) -> ASRStatus:
    settings = context.settings
    return ASRStatus(
        backend=settings.asr.backend,
        model=settings.asr.model,
        models=[
            ModelInfoRead(name=info.name, label=info.label, installed=info.installed)
            for info in available_models(settings)
        ],
    )


@router.get("/models", response_model=ASRStatus)
async def list_models(context: ContextDep) -> ASRStatus:
    return _status(context)


@router.put("/model", response_model=ASRStatus)
async def set_model(body: ASRSelect, context: ContextDep) -> ASRStatus:
    context.settings.asr.model = body.model
    if body.backend is not None:
        context.settings.asr.backend = body.backend
    return _status(context)

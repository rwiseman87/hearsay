"""Schemas for the ASR model picker (swap models/backends at runtime)."""

from __future__ import annotations

from pydantic import BaseModel

from hearsay.enums import ASRBackendKind


class ModelInfoRead(BaseModel):
    name: str
    label: str
    installed: bool


class ASRStatus(BaseModel):
    backend: ASRBackendKind
    model: str
    models: list[ModelInfoRead]


class ASRSelect(BaseModel):
    backend: ASRBackendKind | None = None
    model: str

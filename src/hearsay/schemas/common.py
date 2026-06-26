"""Shared response schemas."""

from __future__ import annotations

from pydantic import BaseModel


class Page[ItemT](BaseModel):
    """Paginated list envelope used by every list endpoint."""

    total: int
    page: int
    page_size: int
    items: list[ItemT]

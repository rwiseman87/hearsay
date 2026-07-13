"""preferences

Revision ID: 7579f4d6db78
Revises: e11fa8155afd
Create Date: 2026-07-13 12:52:03.016334
"""

from __future__ import annotations

from collections.abc import Sequence

from alembic import op
import sqlalchemy as sa


revision: str = '7579f4d6db78'
down_revision: str | None = 'e11fa8155afd'
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    op.create_table(
        'preferences',
        sa.Column('section', sa.String(length=64), nullable=False),
        sa.Column('value', sa.JSON(), nullable=False),
        sa.Column('id', sa.Uuid(), nullable=False),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('updated_at', sa.DateTime(timezone=True), nullable=False),
        sa.PrimaryKeyConstraint('id'),
        sa.UniqueConstraint('section', name='uq_preferences_section'),
    )


def downgrade() -> None:
    op.drop_table('preferences')

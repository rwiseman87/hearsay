"""meeting_assets manifest

Revision ID: e11fa8155afd
Revises: ec241ea5692b
Create Date: 2026-07-13 11:57:19.258950
"""

from __future__ import annotations

from collections.abc import Sequence

from alembic import op
import sqlalchemy as sa


revision: str = 'e11fa8155afd'
down_revision: str | None = 'ec241ea5692b'
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    op.create_table(
        'meeting_assets',
        sa.Column('meeting_id', sa.Uuid(), nullable=False),
        sa.Column(
            'kind',
            sa.Enum(
                'transcript', 'metadata', 'audio', name='assetkind', native_enum=False, length=32
            ),
            nullable=False,
        ),
        sa.Column('rel_path', sa.String(length=512), nullable=False),
        sa.Column('size_bytes', sa.BigInteger(), nullable=False),
        sa.Column('id', sa.Uuid(), nullable=False),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('updated_at', sa.DateTime(timezone=True), nullable=False),
        sa.ForeignKeyConstraint(['meeting_id'], ['meetings.id'], ondelete='CASCADE'),
        sa.PrimaryKeyConstraint('id'),
        sa.UniqueConstraint('meeting_id', 'rel_path', name='uq_meeting_assets_meeting_rel_path'),
    )


def downgrade() -> None:
    op.drop_table('meeting_assets')

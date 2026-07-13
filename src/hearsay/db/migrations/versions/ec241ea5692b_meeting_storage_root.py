"""meeting storage_root

Revision ID: ec241ea5692b
Revises: a69ee62a795b
Create Date: 2026-07-13 11:43:43.734830
"""

from __future__ import annotations

from collections.abc import Sequence

from alembic import op
import sqlalchemy as sa

from hearsay.config.settings import Settings


revision: str = 'ec241ea5692b'
down_revision: str | None = 'a69ee62a795b'
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    # Add the per-meeting storage root. Existing rows are backfilled with the current output root
    # (where their artifacts still live, since nothing has moved yet); after that the column is
    # NOT NULL. Stamping it per row means a later output-dir change only affects new meetings.
    default_root = str(Settings().output_dir.resolve())
    with op.batch_alter_table('meetings', schema=None) as batch_op:
        batch_op.add_column(sa.Column('storage_root', sa.String(length=1024), nullable=True))
    meetings = sa.table('meetings', sa.column('storage_root', sa.String))
    op.execute(meetings.update().values(storage_root=default_root))
    with op.batch_alter_table('meetings', schema=None) as batch_op:
        batch_op.alter_column(
            'storage_root', existing_type=sa.String(length=1024), nullable=False
        )


def downgrade() -> None:
    with op.batch_alter_table('meetings', schema=None) as batch_op:
        batch_op.drop_column('storage_root')

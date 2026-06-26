"""Speaker-attribution fusion.

Clusters the Them stream's utterance embeddings into stable "Speaker N" identities
(pure online clustering) and, with cross-meeting memory, recognizes returning people.
Me is never diarized -- the channel is the identity. The pure clustering logic lives
in :mod:`hearsay.fusion.clustering`; the pipeline (next increment) wires it to the DB.
"""

from __future__ import annotations

from hearsay.fusion.clustering import Assignment, OnlineSpeakerClusterer, Seed, Speaker

__all__ = ["Assignment", "OnlineSpeakerClusterer", "Seed", "Speaker"]

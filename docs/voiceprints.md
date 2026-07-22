# Voiceprints

A **voiceprint** is Hearsay's cross-meeting speaker identity: a fixed-length speaker-embedding
vector that lets the app recognize a returning person in a later meeting without any manual work.
This document traces a voiceprint end to end — what it is, where it comes from, how it is stored,
and how it is referenced to name speakers.

The pure logic lives in `hearsay-attribution`; production lives in `hearsay-inference` (plus the
Swift `hearsay-diarize` sidecar); storage and matching live in `hearsay-db`. For the wider
transcription flow see [pipeline.md](pipeline.md); for the schema see [architecture.md](architecture.md).

## What a voiceprint is

A voiceprint is a **centroid**: an L2-normalized `f32` vector that summarizes one speaker's voice
across a whole meeting. It is stored as little-endian float32 bytes in the nullable `clusters.centroid`
BLOB column. A centroid becomes a *recognizable* voiceprint only once its cluster is **named and
locked** — that is what promotes a per-meeting diarization cluster into a reusable identity.

Two facts shape everything below:

- **Voiceprints are a post-meeting artifact.** The live sidecars emit only `{speaker, text,
  start_s, end_s}` — no embeddings — so nothing during recording writes a centroid. Centroids are
  produced only by the **offline refine**. The live path creates its clusters with a `None`
  centroid:

  ```rust
  // rust/crates/hearsay-orchestrator/src/pipeline.rs:860
  /// Get-or-create the per-meeting cluster for a Them speaker ordinal (unlocked, no centroid live —
  /// the offline refine re-seeds centroids). ...
  queries::create_cluster(pool, meeting_id, ordinal, false, None).await
  ```

- **Only the remote ("Them") track is diarized.** The microphone is always "Me" and is never
  diarized, so Me segments carry no cluster and no voiceprint.

## Lifecycle

```mermaid
flowchart TB
    subgraph produce["1. Produce (offline refine only)"]
        wav["audio.wav (Them = right channel)"]
        diar["Diarizer: speaker turns + per-speaker mean embedding"]
        norm["build_centroids / l2_normalize<br/>-> RefineOutput.centroids by ordinal"]
        wav --> diar --> norm
    end
    subgraph store["2. Store"]
        rep["replace_them_segments (one transaction)"]
        col[("clusters.centroid<br/>LE float32 BLOB")]
        rep --> col
    end
    subgraph name["3. Promote to a named voiceprint"]
        rename["rename_speaker -> rename_cluster<br/>identity_id set, locked = 1"]
    end
    subgraph use["4. Reference in a later meeting"]
        known["KNOWN_VOICEPRINTS_SQL<br/>locked + centroid + named, other meetings"]
        match["recognize_speakers -> match_identity<br/>cosine >= recognition_threshold"]
        known --> match
    end
    norm --> rep
    col --> rename
    rename --> known
    col --> match
```

## 1. Where voiceprints come from (production)

Both refine paths return the same shape — a `Diarization` of ordinal turns plus each speaker's raw
mean embedding by ordinal — and the refine normalizes those into stored centroids. The seam:

```rust
// rust/crates/hearsay-inference/src/diarizer.rs:20
/// A diarization result: ordinal speaker turns (start-sorted) + each speaker's raw mean voiceprint by
/// ordinal (the refine L2-normalizes these into stored centroids). ...
pub struct Diarization {
    pub turns: Vec<DiarTurn>,
    pub embeddings: HashMap<i64, Vec<f32>>,
}
```

The two producers differ in **where the per-speaker mean is computed** and in the embedding model
they use.

### Path A — macOS: FluidAudio on the Apple Neural Engine (accuracy tier)

The `hearsay-diarize` Swift sidecar runs FluidAudio's `OfflineDiarizerManager` (pyannote
community-1 segmentation + **wespeaker_v2 256-d embeddings** + PLDA clustering). The per-speaker
mean is computed *inside* FluidAudio and exposed as `speakerDatabase`; the sidecar just forwards it:

```swift
// helper/Sources/hearsay-diarize/main.swift:71
let manager = OfflineDiarizerManager()
let result = try await manager.process(url)
let turns = result.segments.map {
    Turn(speaker: $0.speakerId, startS: ..., endS: ...)
}
// The offline pipeline populates a per-speaker mean embedding (the voiceprint);
// emit it so the Rust refine can store + match it across meetings.
let speakers = (result.speakerDatabase ?? [:]).map {
    SpeakerEmbedding(speaker: $0.key, embedding: $0.value)
}
```

The sidecar writes one JSON object to stdout (keys snake-cased): `{ sample_rate, duration_s,
speaker_count, turns: [{speaker, start_s, end_s}], speakers: [{speaker, embedding: [f32]}] }`. The
Rust side maps each speaker label to a 1-based ordinal by first appearance and keys the embedding by
that ordinal — no averaging (FluidAudio already did it), and **`consolidate_speakers` does not run
on this path**:

```rust
// rust/crates/hearsay-inference/src/refine.rs:128
let ordinals = order_speakers(&ordering);
...
let mut embeddings: HashMap<i64, Vec<f32>> = HashMap::new();
for speaker in diarized.speakers {
    if let Some(&ord) = ordinals.get(&speaker.speaker) {
        embeddings.insert(i64::from(ord), speaker.embedding);
    }
}
```

### Path B — Windows / cross-platform: sherpa-onnx (fallback tier)

`SherpaDiarizer` runs pyannote segmentation-3.0 (ONNX), which returns only `(start, end, speaker)`
segments — **no embeddings**. Hearsay therefore computes the centroids itself with a second ONNX
model (**TitaNet-small, 192-d**): it concatenates each speaker's turn audio and embeds it, chunked
at 30 s (to stay under TitaNet's positional limit) and duration-weighted-averaged, direction-only:

```rust
// rust/crates/hearsay-inference/src/sherpa_diarize.rs:159
let weight = chunk.len() as f64;
for (acc, v) in sum.iter_mut().zip(&vector) {
    *acc += f64::from(*v) / norm * weight;   // each chunk L2-normalized, weighted by duration
}
total_weight += weight;
...
Ok(Some(sum.into_iter().map(|v| (v / total_weight) as f32).collect()))
```

Because this pipeline reliably over-splits (a known-2-speaker clip can come back as ~6 clusters),
the sherpa path — and only this path — then runs `consolidate_speakers` over the whole-speaker
centroids to fold the fragments back together and drop non-speech clusters:

```rust
// rust/crates/hearsay-inference/src/sherpa_diarize.rs:253
let merged = consolidate_speakers(&embeddings, &speech_s, self.consolidate);
turns.retain(|turn| !merged.dropped.contains(&turn.speaker));
```

Consolidation is covered in [its own section](#the-consolidation-pass-sherpa-only) below.

### Final normalization (both paths)

Whichever diarizer produced the `Diarization`, the refine L2-normalizes each raw embedding into the
stored centroid and keeps a centroid only for a speaker who actually appears in the refined
segments:

```rust
// rust/crates/hearsay-inference/src/refine.rs:172
let present: HashSet<i64> = segments.iter().map(|s| s.ordinal).collect();
let mut centroids = build_centroids(&diarization.embeddings);  // l2_normalize per ordinal
centroids.retain(|ordinal, _| present.contains(ordinal));
```

Whisper is not involved in embeddings at all — it only re-transcribes the track; the ASR text is
then attributed to whichever diarizer turn it most overlaps.

### Producers at a glance

| | macOS (Path A) | Windows / cross-platform (Path B) |
|---|---|---|
| Diarizer | `hearsay-diarize` Swift sidecar (FluidAudio, ANE) | `SherpaDiarizer` (sherpa-onnx, `sherpa` feature) |
| Segmentation | pyannote community-1 | pyannote segmentation-3.0 |
| Embedding model | wespeaker_v2, **256-d** | TitaNet-small, **192-d** |
| Per-speaker mean computed | inside FluidAudio (`speakerDatabase`) | in Rust (`SherpaDiarizer::embed`, duration-weighted) |
| `consolidate_speakers` | no | yes |
| Backend | `MacRefiner` (`hearsay-backends/src/mac.rs`) | `WindowsRefiner` (`hearsay-backends/src/windows.rs`) |

The two spaces are not interchangeable: a 256-d macOS voiceprint and a 192-d Windows voiceprint
never match, and the length-mismatch guard in [`cosine`](#the-math) makes that a safe non-match
rather than a garbage score.

## 2. Where voiceprints are stored

### The column

A voiceprint lives in the nullable `centroid` BLOB on the `clusters` table — one cluster per
diarized speaker per meeting:

```sql
-- rust/crates/hearsay-db/migrations/0001_baseline.sql:26
CREATE TABLE clusters (
    id          BLOB    NOT NULL PRIMARY KEY,
    meeting_id  BLOB    NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    ordinal     INTEGER NOT NULL,
    identity_id BLOB    REFERENCES identities(id) ON DELETE SET NULL,
    locked      INTEGER NOT NULL,
    centroid    BLOB,
    created_at  TEXT    NOT NULL,
    updated_at  TEXT    NOT NULL,
    UNIQUE (meeting_id, ordinal)
);
```

- `ordinal` — the 1-based "Speaker N" number within the meeting.
- `identity_id` — the bound cross-meeting `identities` row (the person's name), or NULL.
- `locked` — 1 when a manual label pins the binding (see [promotion](#3-promoting-a-centroid-to-a-named-voiceprint)).
- `centroid` — the voiceprint bytes, or NULL when the cluster has no usable embedding.

Deleting a meeting cascades its clusters (and their centroids); there is no separate voiceprint
store — a person's voiceprints are simply their locked, named clusters spread across meetings.

### The byte format

A centroid is serialized as contiguous little-endian float32 — no header, no length prefix; the
length is the byte length / 4:

```rust
// rust/crates/hearsay-attribution/src/voiceprint.rs:8
pub fn centroid_to_bytes(centroid: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(centroid.len() * 4);
    for value in centroid {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

pub fn centroid_from_bytes(data: &[u8]) -> Vec<f32> {
    data.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}
```

### The write

The refine's output is carried as `RefineResult { segments, centroids: HashMap<i64, Vec<f32>> }`,
and `replace_them_segments` persists it in **one transaction**: it drops the meeting's live Them
segments and all its clusters, then creates one cluster per refined ordinal with its centroid
serialized onto the row (Me segments are untouched):

```rust
// rust/crates/hearsay-db/src/queries.rs:997
let centroid = result.centroids.get(&seg.ordinal).map(|c| centroid_to_bytes(c));
sqlx::query(
    "INSERT INTO clusters \
     (id, meeting_id, ordinal, identity_id, locked, centroid, created_at, updated_at) \
     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
)
```

A refine that produced no segments is a no-op — the transcript and its existing clusters are never
wiped.

## 3. Promoting a centroid to a named voiceprint

A stored centroid on an unnamed `Speaker N` cluster is not yet recognizable across meetings. It
becomes a voiceprint when the user renames the speaker, via
`PUT /api/meetings/{id}/speakers/{cluster_id}`:

```rust
// rust/crates/hearsay-core/src/routes/speakers.rs:58
pub(crate) async fn rename_speaker(...) -> ApiResult<Json<SpeakerRead>> {
    let name = body.display_name.trim();
    if name.is_empty() || name.chars().count() > 255 {
        return Err(ApiError::Unprocessable("display_name must be 1..=255 characters".into()));
    }
    let row = queries::rename_cluster(&state.pool, cluster_id, name)
        .await?
        .ok_or(ApiError::NotFound("speaker not found"))?;
    Ok(Json(row.into()))
}
```

`rename_cluster` resolves the name to an identity, then **locks** the binding — all in one
transaction:

```rust
// rust/crates/hearsay-db/src/queries.rs:631
let identity_id = get_or_create_identity(&mut tx, name, now).await?;
sqlx::query("UPDATE clusters SET identity_id = ?, locked = 1, updated_at = ? WHERE id = ?")
    .bind(identity_id).bind(now).bind(cluster_id).execute(&mut *tx).await?;
sqlx::query("UPDATE segments SET speaker_label = ?, updated_at = ? WHERE cluster_id = ?")
    .bind(name)  // relabel that speaker's already-saved segments
```

- The rename does **not** write the centroid — that was stored earlier by the refine. It only sets
  `identity_id` + `locked = 1`, which is what qualifies the existing centroid as a known voiceprint.
- `get_or_create_identity` keys on `identities.display_name` (declared `NOT NULL UNIQUE`), so one
  person is one identity row and renaming two clusters to the same name binds both to it. The
  get-or-create is what keeps the UNIQUE constraint from ever raising.
- `GET /api/identities` lists these people (most-recently-updated first) and powers the rename
  autocomplete, so a name from one meeting is suggested in the next.

## 4. How voiceprints are referenced (cross-meeting recognition)

### The candidate set

The "known voiceprints" a refine matches against are exactly the locked, named clusters that carry a
centroid, from *other* meetings:

```rust
// rust/crates/hearsay-db/src/queries.rs:16
const KNOWN_VOICEPRINTS_SQL: &str = "SELECT i.display_name, c.centroid FROM clusters c \
     JOIN identities i ON i.id = c.identity_id \
     WHERE c.locked = 1 AND c.centroid IS NOT NULL AND c.meeting_id != ?";
```

### The match

During `replace_them_segments`, each new ordinal's centroid is compared against that candidate set;
the best match at or above the threshold wins:

```rust
// rust/crates/hearsay-db/src/queries.rs:838
for (&ordinal, centroid) in centroids {
    if manual.contains_key(&ordinal) {
        continue; // a manual carry-forward name wins over auto-recognition
    }
    if let Some(name) = match_identity(centroid, &known, threshold) {
        recognized.insert(ordinal, name.to_string());
    }
}
```

A recognized speaker is bound to the identity but left **unlocked** (provisional) — a later manual
rename can still override it.

### Precedence

`replace_them_segments` resolves each refined ordinal's name in strict precedence order:

| Rank | Source | Binding | Lock | How |
|---|---|---|---|---|
| 1 | **Manual carry-forward** | identity | `locked = 1` | A prior *locked* name is voted onto the new ordinal its old segments most overlap (`carry_forward_locked_names`), so a re-diarize never drops a manual binding. |
| 2 | **Cross-meeting recognition** | identity | `locked = 0` | An unclaimed ordinal whose voiceprint clears the threshold (`recognize_speakers` -> `match_identity`), bound provisionally. |
| 3 | **Fresh** | none | `locked = 0` | Otherwise a plain `Speaker N`. |

A single wrong recognition can never flip a stable manual binding: locked names carry forward first
and are skipped by recognition.

### The recognition threshold

Recognition uses cosine similarity against a configurable cutoff, the effective
`speakers.recognition_threshold`:

- **Default `0.6`**, set in config from `HEARSAY_RECOGNITION_THRESHOLD` (validated to `0.0..=1.0`;
  out-of-range or non-numeric values log a startup problem and fall back to `0.6`) —
  `rust/crates/hearsay-core/src/config.rs:244`.
- Overridable per install via `PUT /api/settings/speakers`, which rejects anything outside
  `0.0..=1.0` with a 422 — `rust/crates/hearsay-core/src/routes/settings.rs`.
- Resolved fresh on every refine by `effective_speakers` (stored override per field, else the
  config default), so a Settings change applies to the next refine with no restart —
  `rust/crates/hearsay-db/src/queries.rs:1296`.

Both refine entry points — auto-refine at stop and the manual "Refine speakers" button
(`POST /api/meetings/{id}/rediarize`) — read this value and pass it into `replace_them_segments`, so
manual and automatic recognition never drift.

## The math

Matching is plain cosine similarity, computed in `f64`, with two safety rails: a length mismatch or
a zero-norm vector returns `0.0` (a safe non-match, never a garbage score).

```rust
// rust/crates/hearsay-attribution/src/voiceprint.rs:28
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return 0.0;
    }
    ...
}

// rust/crates/hearsay-attribution/src/voiceprint.rs:57
pub fn match_identity<'a>(
    centroid: &[f32],
    known: &'a [(String, Vec<f32>)],
    threshold: f64,
) -> Option<&'a str> {
    // best score with cosine >= threshold; different-length candidates skipped; ties -> first
}
```

The length guard does double duty. It tolerates a model change (a stored centroid of a different
dimension is simply skipped rather than compared), and it keeps macOS voiceprints (256-d) from ever
matching Windows voiceprints (192-d) — the two embedding spaces are incompatible, so a length
mismatch is exactly the right verdict.

## The consolidation pass (sherpa only)

`consolidate_speakers` folds a diarizer's over-split clusters back together on their whole-speaker
centroids and drops clusters that resemble no voice in the room. It is pure ordinal-to-ordinal logic
(no ML, no I/O) and runs only on the sherpa path. Two rules, in order:

1. **Merge** the pair of clusters with the widest margin over the similarity their evidence demands,
   until nothing clears its bar. The bar is not fixed: a centroid averaged over two seconds is a
   noisier estimate than one averaged over two minutes, so the merge threshold is scaled down by
   `attenuation(speech_s)` — duration is used as *evidence for how far to trust a centroid*, never
   as a right to exist. The default threshold is `0.65` with an evidence time constant of `3.0 s`.
2. **Drop** clusters whose best cosine to every other cluster is at or below `0` (orthogonal) — what
   non-speech (a chime, room noise the ASR put words on) looks like. Dropping never empties the
   room, and loses no transcript: with the turns gone, overlap attribution hands their text to
   whoever was really speaking.

```rust
// rust/crates/hearsay-attribution/src/consolidate.rs:213
let required = config.merge_threshold
    * attenuation(groups[i].weight, config.evidence_s)
    * attenuation(groups[j].weight, config.evidence_s);
let margin = cosine(&groups[i].centroid, &groups[j].centroid) - required;
```

Merged centroids are duration-weighted means of the L2-normalized inputs, renormalized — so the
merged voiceprint is still a unit vector suitable for cosine matching.

## Deliberately absent

- **No live voiceprints.** The streaming diarizer works online with limited context; a whole-track
  refine is more accurate, so centroids are only ever a refine artifact. Live clusters carry a NULL
  centroid until the refine re-seeds them.
- **No "a cluster under N seconds cannot be a speaker" rule.** That reads as a tidy denoiser but is
  really a cliff that silently deletes a real participant who spoke briefly. Duration enters
  consolidation as evidence (`attenuation`), never as a hard cutoff.
- **No manual centroid editing.** A user names and locks a cluster; the centroid itself is never
  hand-edited.

## Testing

- **Pure logic** (`hearsay-attribution`) is unit-tested directly: `centroid_to_bytes`/`from_bytes`
  round-trips, `cosine` (identical / orthogonal / zero / length-mismatch), `match_identity`
  (best-above-threshold, ties, skip-different-length), and the full `consolidate_speakers` rule set
  (merge, evidence-scaled bar, non-speech drop, never-empty-the-room).
- **The refine** is tested against a stub diarizer in `hearsay-orchestrator`: turn rebuild, rename
  carry-forward, voiceprint recognition, and the empty-meeting / no-turns guards — no ML deps.
- **The sherpa embedding + consolidation** path has opt-in probes in `hearsay-inference`
  (`sherpa_diarize`, `embed_cap_probe`).
- **The Swift codec / sidecar contract** is validated via `hearsay-helper selftest`. Run everything
  with `make test`.

## File reference

| Concern | Location |
|---|---|
| Serialization, cosine, `match_identity` | `rust/crates/hearsay-attribution/src/voiceprint.rs` |
| Over-split consolidation + `attenuation` | `rust/crates/hearsay-attribution/src/consolidate.rs` |
| Diarizer seam (`Diarization`, `Diarizer`) | `rust/crates/hearsay-inference/src/diarizer.rs` |
| Refine + normalize (`build_centroids`, `l2_normalize`) | `rust/crates/hearsay-inference/src/refine.rs` |
| sherpa producer (`embed`, consolidation call) | `rust/crates/hearsay-inference/src/sherpa_diarize.rs` |
| macOS producer (Swift sidecar) | `helper/Sources/hearsay-diarize/main.swift` |
| Backend assembly | `rust/crates/hearsay-backends/src/{mac,windows}.rs` |
| Storage, `known_voiceprints`, `recognize_speakers`, `replace_them_segments`, `rename_cluster` | `rust/crates/hearsay-db/src/queries.rs` |
| Schema | `rust/crates/hearsay-db/migrations/0001_baseline.sql` |
| Rename / re-diarize routes | `rust/crates/hearsay-core/src/routes/speakers.rs` |
| Threshold config + validation | `rust/crates/hearsay-core/src/config.rs`, `.../routes/settings.rs` |

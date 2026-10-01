# Configuration

Every setting Hearsay reads, and how the runtime overrides interact with it.

All configuration is resolved from the environment at startup into one typed settings struct with
loopback-safe defaults. A malformed override — a boolean typo, an unparseable number, an
out-of-range threshold — is a warning in development and a hard startup error otherwise, so a
misconfigured deployment fails fast instead of silently running on a default.

Some of those defaults are in turn overridable at runtime from the Settings UI, which stores a
preference row that wins over the environment. The two layers are described below.

## Related documents

| Document | Scope |
|---|---|
| [development.md](development.md) | Building and running from source, where you set most of these. |
| [user-guide.md](user-guide.md) | The Settings panels these values back, from the user's side. |
| [architecture.md](architecture.md) | Why particular defaults are what they are. |
| [packaging.md](packaging.md) | The paths the packaged app uses instead of the development ones. |

Where the code lives: `hearsay-core/src/config.rs` defines `Settings` and is the compiler-checked
source this page mirrors; `hearsay-core/src/routes/settings.rs` implements the writable overlay.

## Environment variables

| Setting | Env | Default |
|---|---|---|
| Database URL | `DATABASE_URL` | `sqlite://./outputs/db/hearsay.db` |
| Output dir | `HEARSAY_OUTPUT_DIR` | `./outputs/recordings` |
| Web bundle dir | `HEARSAY_WEB_DIR` | `./web/dist` |
| Bind host / port | `HEARSAY_SERVER_HOST` / `HEARSAY_SERVER_PORT` | `127.0.0.1` / `0` (OS-assigned) |
| Helper path | `HEARSAY_HELPER_PATH` | `helper/.build/arm64-apple-macosx/debug/hearsay-helper` |
| Refine model | `HEARSAY_REFINE_MODEL` | `ggml-large-v3-turbo.bin` inside the models dir |
| Refine timeout (seconds) | `HEARSAY_REFINE_TIMEOUT_SECS` | `1800` |
| Refine prompt carry-over | `HEARSAY_REFINE_CARRY_OVER` | `false` |
| Record meeting audio (`audio.wav`) | `HEARSAY_RECORD` | `true` |
| Auto-refine at stop | `HEARSAY_AUTO_REFINE` | `false` |
| Recognition threshold | `HEARSAY_RECOGNITION_THRESHOLD` | `0.6` |
| Inactivity "still recording?" prompt | `HEARSAY_INACTIVITY_PROMPT` | `true` |
| Inactivity auto-end | `HEARSAY_INACTIVITY_AUTO_END` | `true` |
| Minutes of silence before the prompt | `HEARSAY_INACTIVITY_PROMPT_MINUTES` | `5` |
| Minutes of silence before auto-end | `HEARSAY_INACTIVITY_END_MINUTES` | `10` |
| Notes (local-LLM summary) | `HEARSAY_NOTES` | `false` |
| Compress older meeting audio (lossless FLAC) | `HEARSAY_COMPRESS_AUDIO` | `true` |
| Days before a meeting's audio is compressed | `HEARSAY_COMPRESS_AFTER_DAYS` | `7` |
| Notes model (GGUF) | `HEARSAY_NOTES_MODEL` | unset until one is downloaded |
| Notes prompt template | `HEARSAY_NOTES_PROMPT` | built-in template |
| Notes sidecar path | `HEARSAY_NOTES_PATH` | a `hearsay-notes` sibling of the core executable |
| Models download dir | `HEARSAY_MODELS_DIR` | `outputs/models` |
| Shell handshake file | `HEARSAY_HANDSHAKE_PATH` | unset (headless dev prints the URL instead) |
| Third-party notices file | `HEARSAY_THIRD_PARTY_NOTICES` | `./THIRD-PARTY-NOTICES.md` (the shell points it at the bundled copy) |
| Diarize clustering threshold (macOS sidecar) | `HEARSAY_DIARIZE_CLUSTER_THRESHOLD` | `0.7` |
| Environment | `ENVIRONMENT` | `development` |

The handshake and FluidAudio paths are injected by the desktop shell and are normally unset in
development.

## The writable overlay

`HEARSAY_RECORD`, `HEARSAY_AUTO_REFINE`, `HEARSAY_RECOGNITION_THRESHOLD`, the inactivity settings,
the compression settings, and the notes settings are only the *defaults* for four editable Settings
sections. A stored preference overrides the environment, and a change made in Settings applies
without a restart.

Each section is full-replaced on write, so a request body must carry every field of it — omitting
one is a `422`, never a silent reset.

| Section | Covers | Validation |
|---|---|---|
| `recording` | Audio retention, the inactivity prompt, the silence auto-end | Prompt and auto-end are gated independently; each enabled threshold is 1..=1440 minutes, and when both are on the auto-end must exceed the prompt |
| `speakers` | Cross-meeting recognition threshold | `0.0..=1.0` |
| `storage` | Recordings directory, audio archival | `output_dir` must be absolute, existing, and writable; `compress_after_days` is 1..=365 when compression is on |
| `models` | Notes on/off, notes model, prompt template | The notes model path must exist and be a GGUF; the prompt has a length cap |

`compress_after_days` rejects 0 rather than treating it as "immediately": archiving the moment a
meeting finalizes would race the post-stop refine.

Settings are read at meeting start and stop, so an edit never alters a meeting already in progress.

## Where runtime data lives

Run from source, everything — recordings, the SQLite database, downloaded models — lives under the
repo's gitignored `outputs/` directory. Override any path with the variables above. The packaged app
writes to a standard per-user location instead; see [packaging.md](packaging.md).

Note for development: `make rust-serve` points `DATABASE_URL` at its own database file
(`outputs/db/hearsay-rust.db`), separate from any other local database.

## Why a given default is what it is

Rationale lives with the behavior it governs rather than in this table:

- Audio archival and its sweep cadence — [architecture.md](architecture.md#audio-archival).
- The recognition threshold and the diarize clustering threshold — [voiceprints.md](voiceprints.md).
- Auto-refine defaulting off, and the out-of-process notes sidecar —
  [design-decisions.md](design-decisions.md).

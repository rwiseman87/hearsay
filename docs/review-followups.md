# Code-review follow-ups

A backlog of findings from the 2026-07-27 code review, minus the 14 already fixed on branch
`fix/review-top14`. Each item is `file:line` plus the intended fix. Ordered by how much it hurts:
smells first (worth fixing), then nits (batch when convenient). None are release blockers.

## Already fixed (branch `fix/review-top14`)

For reference — do not re-file these:

- Unbounded live-audio buffer on a long monologue (`hearsay-live` cap).
- FTS-vs-`VACUUM` rowid hazard (warning comment in `queries.rs`).
- Biased `select!` starving `emit_rx` (removed `biased`).
- Search timestamps reading "65:00" (`SearchBox` now uses `formatClock`).
- `make dmg` shipping incomplete models on a partial `cp` failure (strict shell on the loops).
- Windows erase racing the SQLite handle (retry-on-remove).
- `get_or_create_identity` UNIQUE-violation race (`ON CONFLICT ... RETURNING`).
- Speaker-consolidation merge bar using a product instead of the weaker attenuation.
- Tauri splash spinning forever on a nav failure (now surfaces a boot error).
- Live notes editor with no visible focus (`.live__notes-editor:focus`).
- CSS: dead token indirection, hardcoded colors duplicating tokens, z-index ordering
  (settings modal now above popovers), incomplete `prefers-reduced-motion`.

---

## Smells (worth fixing)

- [x] **Duplicated speaker-color logic** — `web/src/components/SpeakerPanel.tsx:10-20` +
  `web/src/components/SpeakerLine.tsx:7-20`. `hash()`, `colorVar()`, and `SPEAKER_COLORS` are
  copy-pasted; edit the `* 31` or the palette in one and a speaker's chip dot desyncs from their
  transcript-line color. Extract to one shared module.
- [x] **Autosave resolves out of order** — `web/src/hooks/useUserNotesEditor.ts:46-52`. No in-flight
  guard, so two overlapping `mutate`s can land newest-first and `onSuccess` sets `lastSaved` back to
  the older value, flipping the badge to "unsaved" and re-firing. Track the latest request; ignore
  stale `onSuccess`.
- [ ] **Wrong test comment** — `web/e2e/paths.ts:18-20`. Says the DB is "Wiped at the start of each
  run"; it is not (`playwright.config.ts` deliberately does not wipe, `meeting.spec.ts` says it
  persists). Fix the comment before someone adds a wipe that races the running core.
- [ ] **Device reroute silently disabled** — `rust/crates/hearsay-capture/src/wasapi_source.rs:408-416`.
  `default_device_changed` returns `false` forever if `get_id()` failed once at build time, so a
  mid-meeting headset switch stops rerouting for the whole session with no log. Log the failure;
  consider retrying `get_id()` rather than permanently disabling.
- [ ] **EOF vs read error conflated** — `rust/crates/hearsay-capture/src/swift_helper.rs:279-281`.
  `fill()` maps a clean EOF and a genuine socket read error to the same `false`, so a transient error
  ends capture looking like a graceful exit. Distinguish and log the error case.
- [ ] **Markdown injection into `transcript.md`** — `rust/crates/hearsay-orchestrator/src/markdown.rs:31-46`.
  Title, speaker labels, and transcript text are interpolated verbatim; a title like `# x` or `a | b`
  produces malformed markup. Cosmetic (local, single-user), but escape or neutralize leading
  `#`/`|` in the title and labels.
- [x] **`AbortSignal` never forwarded** — `web/src/api/hooks.ts`. No `useQuery` passes
  `QueryFunctionContext.signal` into `api.get`, so a rapid meeting switch can't cancel an in-flight
  multi-page segment fetch (the wrapper in `client.ts` already supports it). Forward the signal.
- [x] **O(n^2) match highlighting** — `web/src/components/TranscriptView.tsx:719`.
  `matchIndices.includes(index)` runs per line inside `lines.map`; on a long transcript that's O(n^2)
  on every keystroke while Find is active. Make `matchIndices` a `Set`.
- [x] **Stale play state across meeting switch** — `web/src/components/TranscriptView.tsx` (reset
  effect ~264, `onPause` ~551). The `<audio>` has no `key` and is reused by swapping `src`; the reset
  effect never clears `isPlaying`/`duration`, and per the HTML spec changing `src` while playing does
  not fire `pause`. Result: switch meetings while playing and B shows "Pause" with nothing playing and
  A's `duration` on the seek bar until metadata loads. Reset `isPlaying`/`duration` on meeting change
  (or `key` the `<audio>`).
- [ ] **Silent resampler drops** — `helper/Sources/hearsay-helper/Audio/Resampler.swift:23-43`.
  Both `resample` overloads return `[]` on a conversion error with no log or counter, so audio can be
  dropped with zero visibility. Log/count failures.
- [ ] **Panicked background tasks vanish** — `rust/crates/hearsay-orchestrator/src/orchestrator.rs:365`.
  `background.retain(|h| !h.is_finished())` prunes finished handles, including a task that *panicked*,
  so a background finalize/refine/notes panic is never observed. Log/join panicked handles.
- [ ] **Flaky busy-spin bound in tests** — `rust/crates/hearsay-orchestrator/tests/lifecycle.rs:423-429,458-468`.
  `for _ in 0..10_000 { ... yield_now().await }` is a magic timing bound; under CI contention it can be
  exhausted before the work completes. Replace with a `Notify`/event or a generous `timeout`.
- [ ] **Odd pagination envelope** — `rust/crates/hearsay-core/src/routes/speakers.rs:29`.
  `speaker_page` derives `page_size` from the item count, so the "unpaginated" envelope reports a
  `page_size` that isn't a window (differs from every other list endpoint). Report a stable value.
- [ ] **Live clip can be truncated by the new cap** — `helper/Sources/hearsay-live/main.swift:139-144`.
  The `maxRetainSamples` backstop (just added) means a pathologically long turn whose start was dropped
  yields a truncated live clip. Accepted for now (the refine re-transcribes full audio); optionally
  skip/flag the live emit when `startAbs < audioBase` instead of emitting a truncated prefix.

## Nits / taste (batch when convenient)

- [ ] **Missing indexes** — add (via a new forward-only migration) on `segments.cluster_id`,
  `clusters.identity_id`, `meetings.status`, `meetings.started_at`; each is in a `WHERE`/`ORDER BY`
  but unindexed. Low impact on a single-user local DB, but free.
- [ ] **`make build` is a no-op** — `Makefile:130`. The grouped `build notarize:` target just echoes,
  so `make build` looks like it builds and does nothing (exit 0). Rename or make it build/error.
- [ ] **`> web/openapi.json` truncates before generating** — `Makefile:49,54`. A compile/panic in the
  generator leaves the tracked file clobbered empty. Generate to a temp file, then `mv`.
- [ ] **Hardcoded `aarch64`/`arm64` staging** — `Makefile:288-292`. An Intel build would `cp` from a
  nonexistent `arm64-apple-macosx` path. Derive the arch or guard non-Apple-Silicon.
- [ ] **Undocumented `js-yaml` override** — `web/package.json:48`. `"overrides": { "js-yaml": "4.3.0" }`
  with no comment on why (CVE pin? conflict?). Add a why-comment so it doesn't rot.
- [ ] **Token assumed URL-safe in two places** — `web/src-tauri/src/main.rs:391` +
  `rust/crates/hearsay-core/src/security.rs`. The token is interpolated into the webview URL with no
  percent-encoding and parsed back out with no decoding; safe today only because it's hex. Add a
  comment tying the hex assumption together (or percent-encode/decode) so a future token-generator
  change doesn't silently break auth.
- [ ] **`make help` misses space-grouped targets** — `Makefile:11-12`. The regex can't match
  `build notarize:` / `serve rust-serve:`, so `build`/`serve`/`rust-serve` never appear in help.
- [x] **`blurTimer` has no unmount cleanup** — `web/src/components/SearchBox.tsx:111-114`. The blur
  `setTimeout` is only cleared on the next blur/mousedown; clear it on unmount too.
- [x] **`aria-current` inconsistency + hardcoded logo color** — `web/src/components/NavRail.tsx:50`.
  Fixed: the record + search popover triggers now use `aria-expanded` (the correct disclosure
  semantics) instead of `aria-current`. The logo SVG `fill="#4b37c9"` is left as-is — a brand logo
  legitimately hardcodes its own color.
- [ ] **Blocking registry reads in an `async fn`** — `rust/crates/hearsay-capture/src/win_permissions.rs:21-49`.
  `probe_permissions` does blocking registry `open`/`get_string` on the executor. Fast, but wrap in
  `spawn_blocking`.
- [ ] **`.expect` on invariants in spawned/async paths** —
  `rust/crates/hearsay-orchestrator/src/orchestrator.rs:462,597,755` (ANE semaphore) and
  `rust/crates/hearsay-inference/src/refine.rs:295` (`"stdout piped"`). Safe today, but convert to a
  returned error rather than a panic inside a background task/handler.
- [ ] **Inactivity auto-end marker on the wrong clock** — `rust/crates/hearsay-orchestrator/src/pipeline.rs:874`.
  Uses `Instant::elapsed` since spawn while every persisted segment is on the capture `host_ts`
  timeline with paused spans elided, so the marker's `start_s`/`end_s` drift ahead on a paused meeting.
  Stamp it on the `host_ts` timeline.
- [ ] **`slugify` drops all non-ASCII** — `rust/crates/hearsay-orchestrator/src/orchestrator.rs:874`.
  A meeting titled entirely in CJK/Cyrillic becomes `meeting`, `meeting-2`, ... on disk. Keep Unicode
  alphanumerics (or transliterate) instead of ASCII-only.
- [x] **WS backoff has no jitter/cap** — `web/src/api/ws.ts:118`. Intentionally not applied: jitter
  only helps many clients avoid a synchronized thundering herd, but this is one loopback client and
  the delay is already capped by `MAX_BACKOFF_MS`. Adding jitter also fuzzed a deterministic backoff
  test (`ws.test.ts`) that pins the schedule, which is worth more than the non-benefit here.
- [ ] **Download fast-path adopts without re-hashing** — `rust/crates/hearsay-core/src/models.rs:163`.
  An already-present file matching size + GGUF magic is adopted without re-running the SHA256 the
  network path enforces. Optionally re-hash on adopt.
- [ ] **Test helper swallows JSON parse errors** — `rust/crates/hearsay-core/tests/api.rs:154`.
  `serde_json::from_slice(&bytes).unwrap_or(Value::Null)` turns a parse failure into `Null`, so an
  assertion fails with a misleading `Null != expected` instead of a parse error. Surface the error.
- [x] **Playwright `retries: 0`** — `web/playwright.config.ts:56`. Now `retries: process.env.CI ? 1 : 0`. The one e2e spec depends on
  WebSocket streaming timing + handshake polling; a transient flake fails CI outright. Consider
  `retries: 1` in CI.
- [x] **CSS: `--me`/`--spk-1` duplicate token literals** — `web/src/index.css:9,17`. Left as literals:
  `--me` (the Me speaker color) and `--accent` (the UI accent) are distinct concepts that merely share
  a value today; aliasing them would force an unwanted coupling if either is later retuned.
- [ ] **Optional: universal reduced-motion reset** — the `prefers-reduced-motion` block now lists the
  four current animations explicitly; a universal `*` reset would be more robust against future
  animations, at the cost of one `!important` (the file otherwise has none).

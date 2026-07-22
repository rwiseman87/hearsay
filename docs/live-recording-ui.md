# Live recording screen

The plan and tracking state for a dedicated **recording-in-progress** screen, built from an external
design handoff (directions "1a Signal" and "1d Command"). Today there is no live-specific view:
`web/src/components/TranscriptView.tsx` renders both the recording and finalized states, stacking the
speaker and post-meeting notes panels above a flat transcript list. This adds a focused live screen
and three capabilities the app lacks — user-authored live notes, a real amplitude waveform, and
pause/resume.

This document is the working tracking state: the phase checkboxes below are updated as work lands,
and the **Current state** line names the next step so any session can resume. Design rationale is
recorded inline so it is not re-litigated. Canonical architecture stays in
[architecture.md](architecture.md); the IPC contract in [../shared/protocol/ipc.md](../shared/protocol/ipc.md).

## Current state

Phase 0 done (branch `feat/live-recording-ui`, this doc). **Next:** Phase 1 — the 1a shell
(collapsible nav rail + `LiveRecording` view wired to the live transcript stream, End, and a live
timer; waveform decorative for now).

## Scope

- **In**: the 1a layout, then 1d as progressive layers on the same view; a real (backend-persisted)
  "My notes" feature; a real amplitude waveform; pause/resume.
- **Out**: live AI insights (the AI footer strip / insights rail render as honest placeholders, wired
  later); config badges in the topbar (model/template configuration stays in Settings); the design's
  cyan accent and Manrope/IBM Plex Mono fonts (keep the app's blue accent and system font stack).

## Approved directions (from the handoff)

- **1a "Signal"** — `[collapsible nav rail] [main] [My-notes panel]`. Main = topbar (REC pill +
  elapsed timer, title + meta, waveform, Pause, End) → transcript hero (speaker lines, newest line
  brighter with a blinking caret, bottom fade) → a quiet AI footer strip.
- **1d "Command"** — the same view, denser, with a speaker legend + "currently speaking" indicator, a
  live-insights rail, and a dictionary/command-bar row. Layered onto 1a, not a second screen.

## What is reused, not rebuilt

- `useTranscript(meeting)` (`web/src/hooks/useTranscript.ts`) — merges the DB seed with the WS
  partial/final stream into a time-ordered `lines[]`, plus `connection` / `preparing` /
  `inactivityPrompt` / `micSilent`. The new view consumes this as-is.
- Pin-to-bottom auto-scroll + the inactivity / mic-silent banners — patterns lifted from
  `TranscriptView.tsx`.
- `useStopMeeting` (End), `useRenameSpeaker` (inline rename), `MeetingList.tsx` (inside the expanded
  rail), the `useSidebarWidth` localStorage-resize pattern in `App.tsx`.
- Elapsed timer derives from `MeetingRead.started_at` (no backend field needed).
- The helper already emits a `level` control event (`{stream, rms}`) ~every 250ms
  (`helper/Sources/hearsay-helper/Serve.swift`); it is pinned in the IPC contract
  (`hearsay-ipc` `gen_fixtures` `event_level`) but currently dropped by `drain_control`
  (`rust/crates/hearsay-capture/src/swift_helper.rs`). The waveform forwards it; no capture change.
- WS frame codegen pattern: `TranscriptEvent` / `StatusEvent` are utoipa-modeled and registered in
  the OpenAPI components, so they land in `web/src/api/schema.ts`. `LevelEvent` follows the same path.
- `routes/notes.rs` + its query layer is the template for the new user-notes GET/PUT endpoints.

## Phases

Each phase is independently shippable; land them as separate commits/PRs onto `feat/live-recording-ui`.

### Phase 0 — Setup
- [x] Branch `feat/live-recording-ui` off `main`
- [x] This tracking doc

### Phase 1 — Shell + 1a "Signal" (frontend only, real data)
- [ ] `App.tsx` routes to a new `LiveRecording` view when `status === "recording"`; `TranscriptView`
      keeps `finalized` / `refining`
- [ ] `NavRail.tsx` — collapsed 62px icon strip; toggle expands to reveal `MeetingList` at the
      resizable width; collapsed + width persisted in localStorage
- [ ] `LiveRecording.tsx` topbar — REC pill, `useElapsed(started_at)` timer, title + meta, decorative
      waveform, Pause (disabled placeholder), End (`useStopMeeting`)
- [ ] `SpeakerLine.tsx` — avatar/dot + name + `YOU` tag + mono timestamp + body; newest line brighter
      + blinking caret; bottom fade; auto-scroll via the lifted pin-to-bottom logic
- [ ] Keep the inactivity + mic-silent banners
- [ ] AI footer strip — static placeholder ("Highlights appear after the meeting")
- [ ] `index.css` `.live-*` classes reusing existing tokens; `--spk-1..4` speaker-color set

### Phase 2 — "My notes" backend + panel
- [ ] `hearsay-db/migrations/0009_live_notes.sql` — one row per meeting (`meeting_id` PK/FK,
      `body TEXT`, `updated_at`); model + `get` / `upsert` in `models.rs` + `queries.rs`
- [ ] `GET` / `PUT /api/meetings/{id}/live-notes` (new handler, distinct from the LLM `notes.rs`
      route); utoipa `LiveNotesRead` / `LiveNotesWrite` registered; `make codegen`
- [ ] Export `my-notes.md` into the meeting folder on save + at finalize (orchestrator export seam)
- [ ] `MyNotesPanel.tsx` — textarea + debounced autosave (`useSaveMyNotes`), "autosaved" indicator,
      Bookmark / Mention chips (placeholders)

### Phase 3 — Real waveform (amplitude over the WS)
- [ ] Surface the helper `level` event out of `drain_control` to the orchestrator (extend the
      `AudioSource` / control-event seam — verify the exact channel; today it dead-ends at the log)
- [ ] Orchestrator emits `LevelEvent { kind: "level", stream, rms }` on the meeting's existing
      `broadcast::Sender<String>`
- [ ] Register `LevelEvent` in the OpenAPI components; `make codegen`; add to the `ws.ts` `WsMessage`
      union; track latest `rms` per stream; `Waveform.tsx` renders live bars, frozen on pause

### Phase 4 — Pause / Resume (highest risk — design the timeline first)
- [ ] Decide timeline semantics against `recorder.rs` (recommended: pause both capture-feeding and the
      recorded timeline so `audio.wav` + segment `start_s` stay contiguous; the timer holds by tracking
      accumulated paused time)
- [ ] `LiveEngine::pause_meeting` / `resume_meeting` (`hearsay-engine`), default `Err(Unavailable)`;
      `Orchestrator` impl via a `paused` flag on `ActiveSession` gating the per-stream feed loop
      (`pipeline.rs`) and the recorder
- [ ] `MeetingStatus::Paused` (`hearsay-db/src/models.rs` + migration if the status CHECK enumerates
      values)
- [ ] `POST /meetings/{id}/pause` + `/resume`; `usePauseMeeting` / `useResumeMeeting`; wire the topbar
      toggle (REC dot stops pulsing, waveform freezes, timer holds)

### Phase 5 — 1d "Command" layers (frontend)
- [ ] Speaker legend + "currently speaking" — distinct `speaker_label`s from `lines`, deterministic
      colors, pulse the speaker of the latest partial; inline rename via `useRenameSpeaker`
- [ ] Live-insights rail — placeholder ("listening for action items…"), no fabricated data
- [ ] Dictionary chips / command bar / ⌘K / slash commands — deferred visual placeholders (no backend)

## Verification

Drive the real app per phase: `make rust-serve` with `SYNTHETIC=1` + the vite dev server; open the
`open: …?token=` URL; screenshot the live screen (playwright headless + `--virtual-time-budget`) and
read the PNG before claiming a visual change works. A synthetic meeting streams transcript lines so 1a
can be exercised end to end.

- **Phase 2**: notes autosave persists across reload; `my-notes.md` is written to the meeting folder.
- **Phase 3**: bars move with real input and freeze on pause.
- **Phase 4**: pause freezes transcript/timer/waveform; `audio.wav` + segment times stay contiguous on
  resume; the badge shows `paused`; the auto-refine/finalize path still works after a pause.
- **Gates**: `make codegen-check` (no TS drift), `cargo test`, `make ci` green. Swift codec parity is
  unaffected — no IPC frame changes (`level` is already in the contract).
</content>

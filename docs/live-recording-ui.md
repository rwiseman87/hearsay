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

Phases 0-4 shipped (1a shell + real My-notes + waveform + pause/resume). Post-feedback pass:
- The nav-rail logo is the real Hearsay mark (`/hearsay-icon-small.svg`), not a text "H".
- **The 1d "Command" layer was descoped** at the user's request — the Command toggle, command bar,
  speaker-legend rail, and dictionary placeholder were removed (they were mostly non-functional
  chrome). `SpeakerRail.tsx` deleted; `SpeakerLine` color/initials helpers reverted to internal.
- **Post-meeting now surfaces the recording's outputs**: the finalized detail view shows a "My notes"
  panel (the user_notes taken during recording, autosaved + editable via the shared
  `useUserNotesEditor` hook) above the AI "Notes" (summary + action items) section, so the notes the
  live view captures no longer vanish after the meeting. The live AI footer copy is now accurate
  ("A summary and action items can be generated after the meeting ends"; auto-generation is off by
  default). Verified via Playwright (real icon + no command chrome live; My-notes body shown finalized).

Follow-ups (genuinely new backends this scope excluded): live AI insights, auto-generate notes at
stop by default, custom-vocabulary dictionary, command palette / slash commands.

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
- [x] `App.tsx` routes to a new `LiveRecording` view when `status === "recording"`; `TranscriptView`
      keeps `finalized` / `refining`
- [x] `NavRail.tsx` — 62px icon strip (logo + meetings toggle + settings); toggle reveals/collapses
      `MeetingList` at the resizable width; open state + width persisted in localStorage
- [x] `LiveRecording.tsx` topbar — REC pill, `useElapsed(started_at)` timer, title + meta, decorative
      waveform, Pause (disabled placeholder), End (`useStopMeeting`)
- [x] `SpeakerLine.tsx` — avatar + name + `YOU` tag + timestamp + body; newest line brighter +
      blinking caret; bottom fade; auto-scroll via the lifted pin-to-bottom logic
- [x] Keep the inactivity + mic-silent banners
- [x] AI footer strip — static placeholder ("Highlights appear after the meeting")
- [x] `index.css` `.live-*` classes reusing existing tokens; `--spk-1..4` speaker-color set
- Note: the right-hand My-notes panel is a Phase-1 placeholder ("No notes yet." + chips); Phase 2
      wires its real backend. Settings moved from the header button into the nav rail.

### Phase 2 — "My notes" backend + panel
- [x] `hearsay-db/migrations/0009_user_notes.sql` — `user_notes` (`meeting_id` PK/FK, `body TEXT`,
      timestamps); `UserNotes` model + `get_user_notes` / `upsert_user_notes` (+ round-trip/cascade test)
- [x] `GET` / `PUT /api/meetings/{id}/user-notes` (new `user_notes.rs`, distinct from the LLM
      `notes.rs`; PUT is allowed while recording); utoipa `UserNotesRead` / `UserNotesWrite`; `make codegen`
- [x] Export `my-notes.md` via a targeted `LiveEngine::export_user_notes` seam (writes only
      `my-notes.md`, safe mid-recording); called from the PUT handler on each save
- [x] `MyNotesPanel.tsx` — textarea + debounced autosave (`useSaveUserNotes`) + flush-on-blur,
      "autosaved" indicator, Bookmark / Mention chips (placeholders)
- Note: the route/table/types are named `user-notes` / `user_notes` / `UserNotes` (clearer than the
      "live-notes" placeholder name in the original plan).

### Phase 3 — Real waveform (amplitude over the WS)
- [x] Compute RMS in the orchestrator pipeline (`LevelMeter` in `stream_loop`, from the PCM it
      already feeds) instead of forwarding the helper's dropped `level` event — no capture/helper
      change. `+` unit test for the RMS + throttle.
- [x] Broadcast a throttled (~10 Hz) `LevelEvent { kind: "level", stream, rms }` on the meeting's
      existing `broadcast::Sender<String>`
- [x] `LevelEvent` schema registered in the OpenAPI components; `make codegen`; added to a `WsFrame`
      union in `ws.ts`; routed off the transcript reducer into a `LevelStore`
- [x] `Waveform` subscribes via `useSyncExternalStore` and sets each bar's `scaleY` from the RMS, so
      ~10 Hz updates re-render only the waveform
- Note: chose the pipeline-RMS path over the helper-`level` forwarding in the original plan (the
      pipeline already has the PCM; avoids touching the Swift helper and the capture control seam).

### Phase 4 — Pause / Resume
- [x] Timeline semantics = freeze both (no gap): `demux` drops chunks while paused and subtracts the
      paused span from `t0_s`, so `audio.wav` + segment `start_s` stay contiguous. `+` demux unit test.
- [x] `LiveEngine::pause_meeting` / `resume_meeting` / `paused` (default no-op/`None`); `Orchestrator`
      impl sets an `Arc<AtomicBool>` on the active `Pipeline`, which `demux` + the watchdog read. The
      watchdog holds the silence clock while paused (a pause is never mistaken for inactivity).
- [x] No new `MeetingStatus` — pause is a transient live-session state (avoids the enum ripple across
      badges/reconcile/markdown); the meeting stays `recording`. Signalled via a `capture_state` WS
      frame + a WS connect-snapshot (`engine.paused`), so a reopened window reflects a mid-pause.
- [x] `POST /meetings/{id}/pause` + `/resume`; `usePauseMeeting` / `useResumeMeeting`; topbar toggle —
      dot stops pulsing + "PAUSED", waveform flattens, timer freezes (paused-span offset keeps it
      contiguous on resume). Verified via a Playwright WS mock.

### Phase 5 — 1d "Command" layers (frontend) — cut

Built then removed at the user's request (mostly non-functional chrome). The speaker legend, command
bar, dictionary, and the Command toggle are gone; 1a is the only live layout.

### Post-feedback pass

- [x] Nav-rail logo uses the real Hearsay mark (`/hearsay-icon-small.svg`).
- [x] Removed the 1d Command layer (toggle + command bar + `SpeakerRail` + dictionary).
- [x] Post-meeting detail view shows a "My notes" panel (`UserNotesSection`) — the user_notes from the
      meeting, autosaved/editable via the shared `useUserNotesEditor` hook — above the AI Notes section.
- [x] Live AI footer copy corrected to match reality (notes are generated on demand, not automatic).

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

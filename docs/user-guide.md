# User guide

How to record a meeting, get the speaker labels right, and find things afterwards.

Hearsay records two audio streams separately — your microphone ("Me") and whatever the other people
are coming out of your speakers as ("Them") — transcribes both as they happen, works out who said
what on the far end, and keeps everything on your machine. Nothing is uploaded.

## Related documents

| Document | Scope |
|---|---|
| [packaging.md](packaging.md) | Installing, where your data lives, and uninstalling. |
| [configuration.md](configuration.md) | Every setting, including the ones with no UI. |
| [design-decisions.md](design-decisions.md) | Why it behaves the way it does. |

## First run

The installer does not carry the speech models, so the first launch offers to download them: about
1.3 GB, once. Nothing starts until you click **Download models**, which is the point — on a metered
or offline connection you can quit and come back. An interrupted download resumes where it stopped
rather than starting over.

The download also offers a model for meeting notes. That one is optional and can be added later from
Settings > Models. Once the required models are in place the screen does not come back; from then on
Hearsay works with no network at all.

macOS asks for **Microphone** and **System Audio Recording** the first time you start a recording.
Both are required: without the microphone there is no "Me", and without system audio there is no
"Them". The prompts block that first start, so click Allow and it continues.

Settings > Permissions shows the current status at any time.

## Recording a meeting

### Starting

Click the record button in the left rail. Give the recording a name — or leave it blank and it takes
a timestamped one — optionally pick a folder to file it into, and start.

If the transcription models are still loading (a cold start takes a few seconds), the control says
so rather than accepting a start that would stall.

Only one meeting records at a time.

### While it runs

The live view shows a **REC** indicator with elapsed time, an input waveform, and the transcript
building up as people speak. Lines appear dimmed while someone is still talking and firm up when the
sentence completes. Your own speech is labelled **Me**; the far end is labelled **Speaker 1**,
**Speaker 2**, and so on until you name them.

- **Pause** freezes capture. Nothing is recorded or transcribed while paused, and the paused stretch
  is cut out of the timeline entirely, so there is no silent gap when you resume.
- **End** stops the meeting and finalizes it.

Two banners can appear:

- *"Your microphone is not being heard — it is sending silence."* The mic is muted, the wrong input
  device is selected, or the driver has died. Anything transcribed as "Me" until it clears is
  unreliable. This only applies to the microphone; the Them stream is legitimately silent whenever
  nothing is playing.
- *"Still recording? No speech detected for about N minutes."* Choose **Keep recording** to reset
  the clock, or **End now**. Left alone, the meeting ends itself once the silence passes the second
  threshold. Both thresholds are configurable, and each can be turned off independently, in
  Settings > Recording & Privacy.

You can type into **My notes** while the meeting is still running.

### After it ends

The transcript is rewritten in time order, which interleaves your speech and theirs correctly —
during recording, lines arrive as they finish, so a long remote turn can land after something you
said later.

## Finding meetings

The **home** view lists recent meetings. **Meetings** in the left rail opens the full Library, where
you can sort newest or oldest, filter by title, and organize meetings into folders and sub-folders.
Meetings not filed anywhere appear under **Unfiled**. The count beside each folder is its whole
contents, and the title filter searches every meeting — both run over the full library, not the page
you are looking at. Long libraries are paged 50 at a time, with the page controls along the bottom.

Deleting a folder never deletes meetings — they move back to the root.

**Search** in the left rail searches what was *said* — the full text of every transcript, across all
meetings. It matches spoken words only, not meeting titles or notes. Clicking a result opens that
meeting and jumps to the moment it was said.

To find a meeting by name instead, use the title filter inside the Library.

## Reading a meeting

Click any meeting to open its transcript.

- **Play it back.** The recording plays with the transcript highlighted in sync. Click any line to
  jump to that moment.
- **Find in transcript** searches within the open meeting, with next/previous match.
- **Edit a line.** Hover a line and use the edit control to correct the text.
- **Show files** opens the meeting's folder — the audio, `transcript.md`, and notes — in Finder.

## Getting speakers right

The live speaker labels are a first guess made with limited context. There are four ways to fix
them, from cheapest to most thorough.

### Rename a speaker

In the Speakers panel, type a name over **Speaker 1**. This binds that voice to a person and
relabels all of their lines. The name sticks: it survives a refine, and it is suggested the next
time you name someone.

### Reassign a single line

If one line landed on the wrong person, use **Reassign speaker** on that line and pick an existing
speaker or type a new name. This changes only that line, which is what you want for an isolated
mistake rather than a systematically wrong label.

### Merge two speakers

If the same person was split into "Speaker 2" and "Speaker 4", merge one into the other from the
Speakers panel. Their lines combine under one name. The freed number is not reused, so you may end
up with "Speaker 1, Speaker 3" — renumbering would rename people whose lines already say something
else.

### Refine speakers

**Refine speakers** re-runs diarization over the whole recording at once. Because it can see the
entire meeting rather than a rolling window, it separates and merges speakers far more accurately
than the live pass, and it recognizes people you have named in previous meetings.

Names you set by hand are carried across. Per-line reassignments and hand-edited text are **not** —
a refine rebuilds the transcript from the audio. If you have made line-level edits, Hearsay warns
first: *"Discard N edits?"*.

Refining is worth doing whenever the live labels are messy.

### Recognition across meetings

Once you name someone in a meeting that has been refined, Hearsay saves a sample of their voice.
Next time they speak in a refined meeting, they are named automatically.

Manage this in **Settings > Voices**: every person with a stored voice, their samples, and which
meeting each came from. You can rename a person everywhere at once, remove a single sample, or
**Forget voice** to drop all of them. Forgetting only stops future matching — names already written
into past transcripts stay exactly as they are. Someone whose last sample is removed disappears from
the list, since there is nothing left to match on.

A person can be listed as saved but "not in use": a sample only feeds recognition once you have
confirmed the name by hand.

## Notes

Two separate things, deliberately.

**Notes** are generated by a language model running on your machine. Click **Generate notes** on a
finished meeting. The output is whatever your prompt template asks for, stored and shown exactly as
the model wrote it. You can edit the result, and regenerating warns before replacing your edits.
This is off until you download a model in Settings > Models.

**My notes** are yours — free-form text you type during or after the meeting. Nothing overwrites
them.

## Settings

| Panel | What it controls |
|---|---|
| Recording & Privacy | Whether meeting audio is kept at all, auto-refine after each meeting, the inactivity reminder and the automatic stop (each independently switchable, with their own thresholds) |
| Speakers | The recognition threshold — how similar a voice must be before someone is named automatically. Higher is stricter |
| Voices | Stored voiceprints: who is known, their samples, rename everywhere, forget a sample or a person |
| Models | The notes model, including downloading one, plus the notes prompt template |
| Storage | Where recordings live, whether older audio is compressed, after how many days, and a **Compress now** button |
| Permissions | Live microphone and system-audio permission status |
| About | Version, where the database lives, and third-party licenses |
| Data & Uninstall | Reveal your data folder, or erase everything |

Settings take effect from the next meeting — changing something never alters a recording already in
progress.

**Keeping meeting audio** is what makes playback and **Refine speakers** possible, because both read
the recording. Turning it off means neither works.

**Compressing audio** re-encodes older recordings losslessly, at roughly a third of the size. It is
bit-identical, so playback and refining are unaffected. Compression is refused while a meeting is
recording.

## Your data

Everything stays on your machine: recordings, transcripts, notes, the database, and the models. No
telemetry.

Each meeting is a folder containing the audio, `transcript.md`, and any notes — plain files you can
read, back up, or move without Hearsay. Deleting a meeting removes both its folder and its rows, and
cannot be undone.

For exact paths and the uninstall procedure, see [packaging.md](packaging.md).

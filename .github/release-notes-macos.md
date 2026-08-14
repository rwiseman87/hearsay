
## Install

1. Open the DMG and drag Hearsay to your Applications folder.
2. This build is ad-hoc signed and not notarized, so clear the quarantine flag once:

   ```
   xattr -dr com.apple.quarantine /Applications/Hearsay.app
   ```

3. On first launch Hearsay downloads about 2.6 GB of speech models behind a setup screen. The
   installer ships none, and recording stays disabled until the download finishes.

Transcription, diarization and notes all run on-device; audio never leaves the machine.

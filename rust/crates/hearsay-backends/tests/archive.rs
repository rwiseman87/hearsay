//! Background archival: a finalized meeting's audio is losslessly compressed once it is old enough.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use chrono::Duration as ChronoDuration;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::types::chrono::Utc;
use sqlx::SqlitePool;

use hearsay_backends::archive::{sweep_once, Sweeper};
use hearsay_db::models::{Meeting, MeetingStatus};
use hearsay_db::{connect_options, queries, MIGRATOR};

const FRESH_DAYS: i64 = 1;
const OLD_DAYS: i64 = 30;
/// Enough frames to span several encoder blocks.
const FRAMES: usize = 20_000;

async fn memory_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options("sqlite::memory:").unwrap())
        .await
        .unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    pool
}

fn never_busy() -> Box<dyn Fn() -> bool + Send + Sync> {
    Box::new(|| false)
}

/// The stereo 16 kHz recording the recorder writes, as interleaved samples.
fn write_recording(dir: &Path, frames: usize) -> Vec<i16> {
    std::fs::create_dir_all(dir).unwrap();
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut samples = Vec::with_capacity(frames * 2);
    let mut writer = hound::WavWriter::create(dir.join("audio.wav"), spec).unwrap();
    for i in 0..frames as i32 {
        let me = ((i * 3) % 5_001 - 2_500) as i16;
        let them = ((i * 7) % 12_001 - 6_000) as i16;
        writer.write_sample(me).unwrap();
        writer.write_sample(them).unwrap();
        samples.push(me);
        samples.push(them);
    }
    writer.finalize().unwrap();
    samples
}

/// A finalized meeting whose folder lives under `root`, aged `days` into the past.
async fn aged_meeting(pool: &SqlitePool, root: &Path, name: &str, days: i64) -> Meeting {
    let when = Utc::now() - ChronoDuration::days(days);
    let dir = root.join(name);
    let meeting = queries::create_meeting(pool, name, name, &dir.to_string_lossy(), when)
        .await
        .unwrap();
    queries::finalize_meeting(pool, meeting.id, when, MeetingStatus::Finalized)
        .await
        .unwrap();
    meeting
}

#[tokio::test]
async fn archives_an_aged_meeting_losslessly() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let meeting = aged_meeting(&pool, tmp.path(), "old", OLD_DAYS).await;
    let dir = tmp.path().join("old");
    let samples = write_recording(&dir, FRAMES);
    let wav_bytes = std::fs::metadata(dir.join("audio.wav")).unwrap().len();

    let stats = sweep_once(&pool, tmp.path(), true, 7, &Sweeper::new(), &never_busy()).await;

    assert_eq!(stats.compressed, 1);
    assert_eq!(stats.failed, 0);
    assert!(stats.reclaimed_bytes > 0);
    assert!(!dir.join("audio.wav").exists(), "wav should be gone");
    assert!(dir.join("audio.flac").is_file(), "flac should exist");

    // The archived audio is the same audio: the refine reads the Them channel, so check it against
    // the source rather than trusting the sweep's own verify.
    let them = hearsay_audio::read_flac_channel_16k(&dir.join("audio.flac"), 1).unwrap();
    let expected: Vec<f32> = samples.chunks(2).map(|f| f[1] as f32 / 32768.0).collect();
    assert_eq!(them, expected);

    let flac_bytes = std::fs::metadata(dir.join("audio.flac")).unwrap().len();
    assert!(flac_bytes < wav_bytes / 2, "expected a real saving");
    // And the row is untouched — archival is a filesystem concern.
    let row = queries::get_meeting(&pool, meeting.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, MeetingStatus::Finalized);
}

#[tokio::test]
async fn leaves_meetings_that_are_not_yet_old_enough() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    aged_meeting(&pool, tmp.path(), "fresh", FRESH_DAYS).await;
    let dir = tmp.path().join("fresh");
    write_recording(&dir, FRAMES);

    let stats = sweep_once(&pool, tmp.path(), true, 7, &Sweeper::new(), &never_busy()).await;

    assert_eq!(stats.compressed, 0);
    assert!(dir.join("audio.wav").is_file());
    assert!(!dir.join("audio.flac").exists());
}

#[tokio::test]
async fn never_touches_a_meeting_that_is_not_finalized() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    // Old, but still recording: a hard-exit-stranded row must not have its audio pulled out from
    // under a pipeline that may still be writing it.
    let when = Utc::now() - ChronoDuration::days(OLD_DAYS);
    let dir = tmp.path().join("live");
    queries::create_meeting(&pool, "live", "live", &dir.to_string_lossy(), when)
        .await
        .unwrap();
    write_recording(&dir, FRAMES);

    let stats = sweep_once(&pool, tmp.path(), true, 7, &Sweeper::new(), &never_busy()).await;

    assert_eq!(stats.compressed, 0);
    assert!(dir.join("audio.wav").is_file());
}

#[tokio::test]
async fn does_nothing_when_disabled() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    aged_meeting(&pool, tmp.path(), "old", OLD_DAYS).await;
    let dir = tmp.path().join("old");
    write_recording(&dir, FRAMES);

    let stats = sweep_once(&pool, tmp.path(), false, 7, &Sweeper::new(), &never_busy()).await;

    assert_eq!(stats, Default::default());
    assert!(dir.join("audio.wav").is_file());
}

#[tokio::test]
async fn yields_entirely_while_a_meeting_is_recording() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    aged_meeting(&pool, tmp.path(), "old", OLD_DAYS).await;
    let dir = tmp.path().join("old");
    write_recording(&dir, FRAMES);

    let stats = sweep_once(
        &pool,
        tmp.path(),
        true,
        7,
        &Sweeper::new(),
        &Box::new(|| true) as &(dyn Fn() -> bool + Send + Sync),
    )
    .await;

    assert_eq!(stats.compressed, 0);
    // Reported so the ticker comes back in minutes instead of waiting out the full interval — the
    // app is usually opened in order to record, so the post-launch sweep routinely lands here.
    assert!(stats.deferred, "a busy skip must be reported as deferred");
    assert!(dir.join("audio.wav").is_file());
}

#[tokio::test]
async fn stops_the_pass_when_a_meeting_starts_mid_sweep() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    for name in ["old-a", "old-b", "old-c"] {
        aged_meeting(&pool, tmp.path(), name, OLD_DAYS).await;
        write_recording(&tmp.path().join(name), FRAMES);
    }

    // `sweep_once` checks once up front, then once per meeting. Report idle for the up-front check
    // and the first meeting, busy thereafter: exactly one meeting is archived and the rest wait for
    // the next pass rather than competing with the live pipeline.
    let checks = AtomicUsize::new(0);
    let busy = move || checks.fetch_add(1, Ordering::SeqCst) >= 2;

    let stats = sweep_once(
        &pool,
        tmp.path(),
        true,
        7,
        &Sweeper::new(),
        &busy as &(dyn Fn() -> bool + Send + Sync),
    )
    .await;

    assert_eq!(stats.compressed, 1);
    assert!(
        stats.deferred,
        "an interrupted pass must be reported as deferred"
    );
}

#[tokio::test]
async fn skips_meetings_with_no_recording_and_folders_that_vanished() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    // record=false, so no audio was ever written.
    aged_meeting(&pool, tmp.path(), "silent", OLD_DAYS).await;
    std::fs::create_dir_all(tmp.path().join("silent")).unwrap();
    // Folder deleted out from under the row.
    aged_meeting(&pool, tmp.path(), "gone", OLD_DAYS).await;

    let stats = sweep_once(&pool, tmp.path(), true, 7, &Sweeper::new(), &never_busy()).await;

    assert_eq!(stats, Default::default());
}

#[tokio::test]
async fn an_already_archived_meeting_is_left_alone() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    aged_meeting(&pool, tmp.path(), "old", OLD_DAYS).await;
    let dir = tmp.path().join("old");
    write_recording(&dir, FRAMES);

    sweep_once(&pool, tmp.path(), true, 7, &Sweeper::new(), &never_busy()).await;
    let first = std::fs::read(dir.join("audio.flac")).unwrap();
    let stats = sweep_once(&pool, tmp.path(), true, 7, &Sweeper::new(), &never_busy()).await;

    assert_eq!(stats.compressed, 0, "a second pass should be a no-op");
    assert_eq!(std::fs::read(dir.join("audio.flac")).unwrap(), first);
}

#[tokio::test]
async fn a_meeting_that_fails_is_not_retried_in_the_same_process() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    aged_meeting(&pool, tmp.path(), "broken", OLD_DAYS).await;
    let dir = tmp.path().join("broken");
    std::fs::create_dir_all(&dir).unwrap();
    // Not a readable wav at all: encoding refuses it every time, so without the failure memo the
    // sweep would re-attempt it on every tick forever.
    std::fs::write(dir.join("audio.wav"), b"not a wav").unwrap();

    let sweeper = Sweeper::new();
    let first = sweep_once(&pool, tmp.path(), true, 7, &sweeper, &never_busy()).await;
    assert_eq!(first.failed, 1);
    // The audio it could not read is still exactly where it was.
    assert_eq!(std::fs::read(dir.join("audio.wav")).unwrap(), b"not a wav");

    let second = sweep_once(&pool, tmp.path(), true, 7, &sweeper, &never_busy()).await;
    assert_eq!(second.failed, 0, "should not be retried");
    assert_eq!(second.compressed, 0);
}

#[tokio::test]
async fn a_meeting_finalized_without_an_end_time_still_ages_in() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    // The startup reconcile can finalize a hard-exit row without ever stamping `ended_at`; the age
    // must fall back to `started_at` or this meeting is archived never.
    let when = Utc::now() - ChronoDuration::days(OLD_DAYS);
    let dir = tmp.path().join("no-end");
    let meeting = queries::create_meeting(&pool, "no-end", "no-end", &dir.to_string_lossy(), when)
        .await
        .unwrap();
    queries::set_meeting_finalized(&pool, meeting.id)
        .await
        .unwrap();
    write_recording(&dir, FRAMES);

    let stats = sweep_once(&pool, tmp.path(), true, 7, &Sweeper::new(), &never_busy()).await;

    assert_eq!(stats.compressed, 1);
    assert!(!stats.deferred);
    assert!(dir.join("audio.flac").is_file());
}

/// An I/O failure is transient — a manual re-diarize can hold the recording open, and a read-only
/// moment resolves itself. Unlike a corrupt recording it must NOT be memoized, or the meeting stays
/// uncompressed until the process restarts.
#[cfg(unix)]
#[tokio::test]
async fn a_transient_io_failure_is_retried_on_the_next_pass() {
    use std::os::unix::fs::PermissionsExt;

    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    aged_meeting(&pool, tmp.path(), "locked", OLD_DAYS).await;
    let dir = tmp.path().join("locked");
    write_recording(&dir, FRAMES);

    // Read-only directory: the encoder cannot create its temporary file.
    let original = std::fs::metadata(&dir).unwrap().permissions();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();

    let sweeper = Sweeper::new();
    let first = sweep_once(&pool, tmp.path(), true, 7, &sweeper, &never_busy()).await;
    assert_eq!(first.failed, 1);
    assert_eq!(first.compressed, 0);

    // Once the condition clears, the very next pass picks it up again.
    std::fs::set_permissions(&dir, original).unwrap();
    let second = sweep_once(&pool, tmp.path(), true, 7, &sweeper, &never_busy()).await;
    assert_eq!(
        second.compressed, 1,
        "should be retried after the I/O error"
    );
    assert!(dir.join("audio.flac").is_file());
}

/// The scenario that made this feature look broken in the real app: Hearsay is opened in order to
/// record, so a meeting is often already running when the post-launch sweep fires. Deferring is
/// correct; deferring for a whole hour is not, because a user who records and then quits would never
/// archive anything. A deferred pass must report itself so the ticker can come back in minutes.
#[tokio::test]
async fn a_meeting_running_at_the_first_tick_only_defers_the_pass() {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    aged_meeting(&pool, tmp.path(), "old", OLD_DAYS).await;
    let dir = tmp.path().join("old");
    write_recording(&dir, FRAMES);

    // Recording when the sweep fires.
    let recording = AtomicBool::new(true);
    let busy = move || recording.load(Ordering::SeqCst);
    let first = sweep_once(
        &pool,
        tmp.path(),
        true,
        7,
        &Sweeper::new(),
        &busy as &(dyn Fn() -> bool + Send + Sync),
    )
    .await;
    assert_eq!(first.compressed, 0);
    assert!(first.deferred, "must signal that work remains");
    assert!(dir.join("audio.wav").is_file());

    // The meeting ends; the very next pass does the work.
    let second = sweep_once(&pool, tmp.path(), true, 7, &Sweeper::new(), &never_busy()).await;
    assert_eq!(second.compressed, 1);
    assert!(!second.deferred);
    assert!(dir.join("audio.flac").is_file());
}

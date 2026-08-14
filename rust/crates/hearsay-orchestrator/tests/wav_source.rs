//! `WavFileSource`: read a stereo 16 kHz WAV back as stream-tagged, timed chunks (Me = left,
//! Them = right), holding the channel open until stop.

use hearsay_orchestrator::{AudioSource, Stream, WavFileSource};

#[tokio::test]
async fn wav_file_source_streams_stereo_frames() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("audio.wav");

    // 2400 samples/channel at 16 kHz: L = +0.5 (16384), R = -0.5 (-16384).
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    {
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..2400 {
            writer.write_sample(16384i16).unwrap(); // left / Me
            writer.write_sample(-16384i16).unwrap(); // right / Them
        }
        writer.finalize().unwrap();
    }

    let mut source = WavFileSource::new(path);
    let mut rx = source.start().await.unwrap();

    // 2400 samples / 1600-per-frame -> 2 frames, each emitting Me then Them.
    let mut chunks = Vec::new();
    for _ in 0..4 {
        chunks.push(rx.recv().await.unwrap());
    }

    // Frame 0: full 1600-sample chunks at host_ts 0.
    assert_eq!(chunks[0].stream, Stream::Me);
    assert_eq!(chunks[0].chunk.host_ts, 0);
    assert_eq!(chunks[0].chunk.samples.len(), 1600);
    assert!((chunks[0].chunk.samples[0] - 0.5).abs() < 1e-4);
    assert_eq!(chunks[1].stream, Stream::Them);
    assert!((chunks[1].chunk.samples[0] + 0.5).abs() < 1e-4);

    // Frame 1: the remaining 800 samples at host_ts = 1600/16000 s = 100 ms.
    assert_eq!(chunks[2].stream, Stream::Me);
    assert_eq!(chunks[2].chunk.host_ts, 100_000_000);
    assert_eq!(chunks[2].chunk.samples.len(), 800);
    assert_eq!(chunks[3].stream, Stream::Them);

    // Holds open like a live source until stop; then the channel closes.
    source.stop().await;
    assert!(rx.recv().await.is_none());
}

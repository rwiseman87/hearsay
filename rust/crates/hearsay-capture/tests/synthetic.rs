//! End-to-end capture over the real Swift `hearsay-helper` in `--synthetic` mode (generated audio,
//! no TCC prompts). Ignored by default (needs `make swift-build`); run with:
//!   cargo test --manifest-path rust/Cargo.toml -p hearsay-capture -- --ignored --nocapture

use std::path::PathBuf;
use std::time::Duration;

use hearsay_capture::SwiftHelperSource;
use hearsay_orchestrator::{AudioSource, Stream};

fn helper_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../helper/.build/arm64-apple-macosx/debug/hearsay-helper")
}

#[tokio::test]
#[ignore = "needs the built Swift hearsay-helper (make swift-build)"]
async fn synthetic_capture_yields_both_streams() {
    let mut source = SwiftHelperSource::new(helper_path()).synthetic(true);
    let mut rx = source
        .start()
        .await
        .expect("helper starts + start_capture ok");

    let (mut me, mut them, mut samples) = (0usize, 0usize, 0usize);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while (me == 0 || them == 0) && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(chunk)) => {
                samples += chunk.chunk.samples.len();
                match chunk.stream {
                    Stream::Me => me += 1,
                    Stream::Them => them += 1,
                }
            }
            _ => break,
        }
    }
    source.stop().await;

    eprintln!("captured: {me} Me frames, {them} Them frames, {samples} samples");
    assert!(me > 0, "expected Me chunks from synthetic capture");
    assert!(them > 0, "expected Them chunks from synthetic capture");
    assert!(samples > 0);
}

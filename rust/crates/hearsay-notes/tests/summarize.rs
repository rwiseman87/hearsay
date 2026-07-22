//! Opt-in end-to-end smoke test for the notes sidecar against a real GGUF instruct model: drive the
//! built `hearsay-notes` binary exactly as the core does — JSON request on stdin, JSON notes on
//! stdout. Ignored by default (needs a multi-GB model); run:
//!   HEARSAY_NOTES_MODEL=/abs/path/to/model.gguf \
//!   cargo test --manifest-path rust/Cargo.toml -p hearsay-notes --features metal \
//!     summarize -- --ignored --nocapture

use std::io::Write;
use std::process::{Command, Stdio};

use hearsay_notes_prompt::DEFAULT_NOTES_PROMPT;
use serde_json::{json, Value};

const TRANSCRIPT: &str = "\
Me: Thanks everyone for joining. We need to lock the Q3 launch date and decide who owns the migration.
Alex: I can own the database migration. I'll have a rollback plan ready by Friday.
Me: Great. Let's target August 12 for the launch, assuming the migration passes staging.
Sam: I'll write the customer announcement and send a draft to Alex for review by Wednesday.
Me: Perfect. And let's make sure marketing signs off before we ship.
";

#[test]
#[ignore = "needs a GGUF instruct model at $HEARSAY_NOTES_MODEL"]
fn summarizes_a_short_meeting_via_the_sidecar() {
    let model = std::env::var("HEARSAY_NOTES_MODEL")
        .expect("set HEARSAY_NOTES_MODEL to an absolute .gguf path");
    let request = json!({
        "model": model,
        "template": DEFAULT_NOTES_PROMPT,
        "transcript": TRANSCRIPT,
    })
    .to_string();

    let mut child = Command::new(env!("CARGO_BIN_EXE_hearsay-notes"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn hearsay-notes");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.as_bytes())
        .expect("write request");
    let output = child.wait_with_output().expect("wait hearsay-notes");
    assert!(output.status.success(), "sidecar exited non-zero");

    let notes: Value = serde_json::from_slice(&output.stdout).expect("parse notes JSON");
    let summary = notes["summary"].as_str().unwrap_or("");
    let action_items = notes["action_items"]
        .as_array()
        .expect("action_items array");
    println!("SUMMARY:\n{summary}\n");
    println!("ACTION ITEMS:");
    for item in action_items {
        println!("  - {}", item.as_str().unwrap_or(""));
    }

    assert!(!summary.trim().is_empty(), "expected a non-empty summary");
    // The transcript names concrete owners + dates, so a working instruct model should extract at
    // least one action item.
    assert!(
        !action_items.is_empty(),
        "expected at least one action item"
    );
}

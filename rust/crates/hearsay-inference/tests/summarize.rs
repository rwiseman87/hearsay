//! Opt-in smoke test for the local-LLM notes step against a real GGUF instruct model.
//! Ignored by default (needs a multi-GB model — download it via Settings > Models or the CLI); run:
//!   HEARSAY_NOTES_MODEL=/abs/path/to/model.gguf \
//!   cargo test --manifest-path rust/Cargo.toml -p hearsay-inference --features notes,metal \
//!     summarize -- --ignored --nocapture

#![cfg(feature = "notes")]

use std::path::PathBuf;

use hearsay_inference::summarize;

const TRANSCRIPT: &str = "\
Me: Thanks everyone for joining. We need to lock the Q3 launch date and decide who owns the migration.
Alex: I can own the database migration. I'll have a rollback plan ready by Friday.
Me: Great. Let's target August 12 for the launch, assuming the migration passes staging.
Sam: I'll write the customer announcement and send a draft to Alex for review by Wednesday.
Me: Perfect. And let's make sure marketing signs off before we ship.
";

#[test]
#[ignore = "needs a GGUF instruct model at $HEARSAY_NOTES_MODEL"]
fn summarizes_a_short_meeting() {
    let model = PathBuf::from(
        std::env::var("HEARSAY_NOTES_MODEL")
            .expect("set HEARSAY_NOTES_MODEL to an absolute .gguf path"),
    );

    let notes = summarize(&model, TRANSCRIPT).expect("summarize failed");

    println!("SUMMARY:\n{}\n", notes.summary);
    println!("ACTION ITEMS:");
    for item in &notes.action_items {
        println!("  - {item}");
    }

    assert!(
        !notes.summary.trim().is_empty(),
        "expected a non-empty summary"
    );
    // The transcript names concrete owners + dates, so a working instruct model should extract at
    // least one action item.
    assert!(
        !notes.action_items.is_empty(),
        "expected at least one action item"
    );
}

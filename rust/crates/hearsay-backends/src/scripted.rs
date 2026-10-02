//! The dev-only scripted engine behind the `scripted` feature; release builds leave it out.

use std::sync::Arc;
use std::time::Duration;

use hearsay_db::models::Stream;
use hearsay_engine::LiveEngine;
use hearsay_orchestrator::testing::{
    chunk, seg, ProgressiveBackend, ProgressivePlan, ScriptedSummarizer,
};
use hearsay_orchestrator::{Orchestrator, SegmentKind};

use crate::EngineConfig;

/// Assemble a deterministic, model-free [`LiveEngine`] that runs the real orchestrator pipeline +
/// persistence over a canned meeting — emitting its transcript progressively over the live WebSocket
/// during recording (via [`ProgressiveBackend`]) — instead of touching any capture device, ANE, or
/// GPU. Selected by the core's dev-only `HEARSAY_SCRIPTED` flag so the browser end-to-end test can
/// drive the real core *binary* with exact, assertable output. It reuses the same
/// `hearsay-orchestrator::testing` fakes the in-process Rust full-stack test does, so both paths
/// produce identical output. A canned summarizer backs the "Generate notes" step; no refiner is wired
/// (like the full-stack test), so stop just finalizes and keeps the live-emitted segments.
pub fn build_scripted_engine(config: EngineConfig) -> Arc<dyn LiveEngine> {
    let backend = Arc::new(ProgressiveBackend::new(scripted_meeting_plan()));
    let (summarizer, _) = ScriptedSummarizer::new(
        "Scripted summary for the end-to-end test.\n\n- Ship the browser E2E.",
    );
    let defaults = config.defaults();
    Orchestrator::new(config.pool, config.output_dir, backend)
        .with_defaults(defaults)
        .with_summarizer(summarizer)
        .into_arc()
}

/// The canned conversation `HEARSAY_SCRIPTED` replays: two capture chunks to anchor the shared clock
/// (Me at t0, Them +0.5 s), then a short Me/Them exchange whose partials + finals emit ~0.4-1.2 s into
/// the meeting so the browser E2E can watch the transcript grow, then assert the two finalized turns.
fn scripted_meeting_plan() -> ProgressivePlan {
    ProgressivePlan {
        chunks: vec![
            chunk(Stream::Me, 1_000_000_000, &[0.05, 0.05]),
            chunk(Stream::Them, 1_500_000_000, &[0.05, 0.05, 0.05]),
        ],
        me: vec![
            (
                Duration::from_millis(400),
                seg(SegmentKind::Partial, "hello", 0.0, 0.5, None),
            ),
            (
                Duration::from_millis(400),
                seg(SegmentKind::Final, "hello there", 0.0, 1.0, None),
            ),
        ],
        them: vec![
            (
                Duration::from_millis(600),
                seg(SegmentKind::Partial, "hi", 0.0, 0.5, None),
            ),
            (
                Duration::from_millis(600),
                seg(
                    SegmentKind::Final,
                    "hi everyone, thanks for joining",
                    1.0,
                    3.0,
                    Some(0),
                ),
            ),
        ],
    }
}

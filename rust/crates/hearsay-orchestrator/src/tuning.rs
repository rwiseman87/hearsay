//! Test-visible knobs for the live pipeline's echo handling, plus counters the eval reads back.
//! `Default` is the production behavior; nothing here is a user-facing setting.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use crate::aec::AecConfig;
use crate::echo_dedup::EchoDedupConfig;
use crate::lock::MutexExt;

/// One Me final the echo dedup dropped.
#[derive(Clone, Debug)]
pub struct EchoDrop {
    pub start_s: f64,
    pub end_s: f64,
    pub text: String,
}

/// Counters a pipeline run fills in.
#[derive(Default)]
pub struct LiveStats {
    /// Capture chunks dropped on a full transcriber hand-off queue, both streams.
    pub dropped_chunks: AtomicU64,
    /// Replacement sidecars started after a Me sidecar died mid-meeting.
    pub me_respawns: AtomicU64,
    /// Replacement sidecars started after a Them sidecar died mid-meeting.
    pub them_respawns: AtomicU64,
    echo_drops: Mutex<Vec<EchoDrop>>,
}

impl LiveStats {
    pub(crate) fn record_echo_drop(&self, start_s: f64, end_s: f64, text: &str) {
        self.echo_drops.lock_recover().push(EchoDrop {
            start_s,
            end_s,
            text: text.to_string(),
        });
    }

    /// The Me finals the echo dedup dropped, in drop order.
    pub fn echo_drops(&self) -> Vec<EchoDrop> {
        self.echo_drops.lock_recover().clone()
    }
}

/// How the pipeline cancels and dedups echo. `None` disables the stage.
#[derive(Clone)]
pub struct LiveTuning {
    pub aec: Option<AecConfig>,
    pub echo_dedup: Option<EchoDedupConfig>,
    pub stats: Arc<LiveStats>,
}

impl Default for LiveTuning {
    fn default() -> Self {
        LiveTuning {
            aec: Some(AecConfig::default()),
            echo_dedup: Some(EchoDedupConfig::default()),
            stats: Arc::new(LiveStats::default()),
        }
    }
}

//! Text-level echo dedup: a backstop for acoustic echo cancellation.
//!
//! The acoustic canceller ([`crate::aec`]) cannot fully cancel cheap laptop speakers, so residual
//! Them audio still reaches `hearsay-me` and is transcribed as the local user. This module drops a
//! Me *final* whose text echoes recently transcribed Them speech — the transcript-level complement
//! to AEC's signal-level work. It touches only live Me finals, never the archive or the offline
//! refine. See `docs/echo-cancellation.md`.
//!
//! ## Streaming order
//!
//! Them finals and Them partials are recorded as candidates. A Them final lands only when the
//! diarizer closes the turn, which can be long after speech starts, while a Me echo finalizes after
//! a short silence; the open turn's latest partial text covers that window. The partial slot holds
//! one entry (a new partial replaces the old) and a final for the same turn supersedes it.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::tuning::LiveStats;

/// Tuning for [`EchoDedup`]. Defaults are deliberately conservative: short utterances are never
/// dropped (so backchannels like "yeah" / "right" survive) and only a near-complete, contiguous
/// text match inside a concurrent time window counts as an echo.
#[derive(Clone, Copy, Debug)]
pub struct EchoDedupConfig {
    /// A Me final shorter than this many tokens is never treated as an echo.
    pub min_tokens: usize,
    /// Fraction of the Me final's tokens that must form one contiguous run in the concurrent Them
    /// text.
    pub similarity: f64,
    /// Slack (seconds) on each side when deciding whether a Them entry is concurrent with the Me
    /// final — covers playout + acoustic + ASR-endpoint skew between the two streams.
    pub window_s: f64,
    /// How long (seconds) a Them entry stays a candidate reference after it ends.
    pub retain_s: f64,
}

impl Default for EchoDedupConfig {
    fn default() -> Self {
        Self {
            min_tokens: 4,
            similarity: 0.8,
            window_s: 1.5,
            retain_s: 10.0,
        }
    }
}

struct ThemEntry {
    start_s: f64,
    end_s: f64,
    tokens: Vec<String>,
}

/// A rolling window of recent Them finals plus the open turn's partial, matched against incoming Me
/// finals.
pub(crate) struct EchoDedup {
    cfg: EchoDedupConfig,
    recent: VecDeque<ThemEntry>,
    partial: Option<ThemEntry>,
    /// The latest segment end time seen on either stream, i.e. "now" in meeting time. Drives pruning.
    latest_s: f64,
    stats: Option<Arc<LiveStats>>,
}

impl EchoDedup {
    pub fn new(cfg: EchoDedupConfig) -> Self {
        Self {
            cfg,
            recent: VecDeque::new(),
            partial: None,
            latest_s: 0.0,
            stats: None,
        }
    }

    /// A dedup that never drops (no Me final reaches `usize::MAX` tokens).
    pub fn off() -> Self {
        Self::new(EchoDedupConfig {
            min_tokens: usize::MAX,
            ..EchoDedupConfig::default()
        })
    }

    /// Record every dropped Me final into `stats`.
    pub fn with_stats(mut self, stats: Arc<LiveStats>) -> Self {
        self.stats = Some(stats);
        self
    }

    /// Record a finalized Them segment as a candidate echo source, superseding the open partial.
    pub fn record_them(&mut self, start_s: f64, end_s: f64, text: &str) {
        if self.partial.as_ref().is_some_and(|p| p.start_s < end_s) {
            self.partial = None;
        }
        let tokens = normalize(text);
        if tokens.is_empty() {
            return;
        }
        self.latest_s = self.latest_s.max(end_s);
        self.recent.push_back(ThemEntry {
            start_s,
            end_s,
            tokens,
        });
        self.prune();
    }

    /// Record the open Them turn's partial text, replacing the previous partial.
    pub fn record_them_partial(&mut self, start_s: f64, end_s: f64, text: &str) {
        let tokens = normalize(text);
        if tokens.is_empty() {
            return;
        }
        self.latest_s = self.latest_s.max(end_s);
        self.partial = Some(ThemEntry {
            start_s,
            end_s,
            tokens,
        });
        self.prune();
    }

    /// True if a Me final is an echo of concurrent Them speech and should be dropped.
    pub fn is_echo(&mut self, start_s: f64, end_s: f64, text: &str) -> bool {
        self.latest_s = self.latest_s.max(end_s);
        self.prune();

        let me = normalize(text);
        if me.len() < self.cfg.min_tokens {
            return false;
        }

        // Pool the tokens of every concurrent Them entry, in time order (finals, then the open
        // partial), into one reference sequence: Them may have been endpointed into several finals
        // across the span the Me echo covers as a single final.
        let mut pool: Vec<&str> = Vec::new();
        for e in self.recent.iter().chain(self.partial.iter()) {
            if e.start_s <= end_s + self.cfg.window_s && e.end_s >= start_s - self.cfg.window_s {
                pool.extend(e.tokens.iter().map(String::as_str));
            }
        }
        if pool.is_empty() {
            return false;
        }

        let me_refs: Vec<&str> = me.iter().map(String::as_str).collect();
        let covered = longest_common_run(&me_refs, &pool) as f64 / me.len() as f64;
        let echo = covered >= self.cfg.similarity;
        if echo {
            if let Some(stats) = &self.stats {
                stats.record_echo_drop(start_s, end_s, text);
            }
        }
        echo
    }

    fn prune(&mut self) {
        let cutoff = self.latest_s - self.cfg.retain_s;
        while self.recent.front().is_some_and(|e| e.end_s < cutoff) {
            self.recent.pop_front();
        }
        if self.partial.as_ref().is_some_and(|p| p.end_s < cutoff) {
            self.partial = None;
        }
    }
}

/// Lowercase word tokens: split on any non-alphanumeric run, drop empties. Both streams tokenize
/// the same way, so case and punctuation differences never block a match.
fn normalize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Length of the longest common contiguous run of two token sequences (rolling one-row DP).
fn longest_common_run(a: &[&str], b: &[&str]) -> usize {
    let mut best = 0;
    let mut prev = vec![0usize; b.len() + 1];
    for x in a {
        let mut cur = vec![0usize; b.len() + 1];
        for (j, y) in b.iter().enumerate() {
            if x == y {
                cur[j + 1] = prev[j] + 1;
                best = best.max(cur[j + 1]);
            }
        }
        prev = cur;
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dedup() -> EchoDedup {
        EchoDedup::new(EchoDedupConfig::default())
    }

    #[test]
    fn exact_echo_is_dropped() {
        let mut d = dedup();
        d.record_them(1.0, 3.0, "so what about the third quarter numbers");
        assert!(d.is_echo(1.2, 3.3, "So what about the third quarter numbers?"));
    }

    #[test]
    fn echo_with_a_dropped_word_is_dropped() {
        let mut d = dedup();
        d.record_them(1.0, 3.0, "so what about the third quarter numbers");
        // The mic ASR missed "so" — still a near-complete subsequence of the Them reference.
        assert!(d.is_echo(1.2, 3.3, "what about the third quarter numbers"));
    }

    #[test]
    fn short_backchannel_is_kept() {
        let mut d = dedup();
        d.record_them(1.0, 3.0, "yeah that works for me");
        // Below min_tokens: a genuine "yeah, right" agreement must never be deleted as an echo.
        assert!(!d.is_echo(3.1, 3.4, "yeah right"));
    }

    #[test]
    fn distinct_me_speech_is_kept() {
        let mut d = dedup();
        d.record_them(1.0, 3.0, "so what about the third quarter numbers");
        assert!(!d.is_echo(1.2, 3.3, "i think we should push the launch to may"));
    }

    #[test]
    fn me_original_containing_a_short_them_phrase_is_kept() {
        let mut d = dedup();
        d.record_them(1.0, 1.6, "the budget");
        // Only 2 of 10 Me tokens are covered — this is real Me speech, not an echo.
        assert!(!d.is_echo(
            1.2,
            3.0,
            "i think the budget looks fine for next quarter honestly"
        ));
    }

    #[test]
    fn echo_split_across_two_them_finals_is_dropped() {
        let mut d = dedup();
        d.record_them(1.0, 2.0, "so what about");
        d.record_them(2.0, 3.0, "the third quarter numbers");
        assert!(d.is_echo(1.2, 3.3, "so what about the third quarter numbers"));
    }

    #[test]
    fn them_outside_the_window_is_not_matched() {
        let mut d = dedup();
        d.record_them(1.0, 3.0, "so what about the third quarter numbers");
        // Me starts well after Them ended (beyond window_s): not concurrent, so not an echo.
        assert!(!d.is_echo(6.0, 8.0, "so what about the third quarter numbers"));
    }

    #[test]
    fn old_them_is_pruned_beyond_retain() {
        let mut d = dedup();
        d.record_them(1.0, 3.0, "so what about the third quarter numbers");
        // A later Them final advances "now" past retain_s, evicting the first entry.
        d.record_them(20.0, 22.0, "unrelated later remark from the far end");
        assert!(!d.is_echo(1.2, 3.3, "so what about the third quarter numbers"));
    }

    #[test]
    fn double_talk_keeps_real_me_words() {
        let mut d = dedup();
        d.record_them(1.0, 3.0, "we should ship it on friday");
        // Me talks over Them: a few shared words, but most of Me is its own — coverage stays low.
        assert!(!d.is_echo(
            1.2,
            3.3,
            "no i really do not think we should wait that long"
        ));
    }

    #[test]
    fn echo_finalizing_before_the_them_final_matches_the_partial() {
        let mut d = dedup();
        d.record_them_partial(
            1.0,
            4.0,
            "so what about the third quarter numbers and the forecast",
        );
        assert!(d.is_echo(1.4, 3.6, "what about the third quarter numbers"));
    }

    #[test]
    fn scattered_common_words_are_not_an_echo() {
        let mut d = dedup();
        d.record_them(
            1.0,
            9.0,
            "yeah so i was saying that i do not think this is the case for the rest of them",
        );
        // Every Me token appears in order in the Them text, but no run of them is contiguous.
        assert!(!d.is_echo(3.0, 4.0, "yeah i think that is the case"));
    }

    #[test]
    fn double_talk_over_a_partial_is_kept() {
        let mut d = dedup();
        d.record_them_partial(1.0, 3.0, "we should ship it on friday");
        assert!(!d.is_echo(
            1.2,
            3.3,
            "no i really do not think we should wait that long"
        ));
    }

    #[test]
    fn new_partial_replaces_the_previous_one() {
        let mut d = dedup();
        d.record_them_partial(1.0, 2.0, "so what about the");
        d.record_them_partial(1.0, 3.0, "so what about the third quarter numbers");
        // Pooling both would make this repeated text match; the replaced partial must not count.
        assert!(!d.is_echo(
            1.2,
            3.3,
            "so what about the so what about the third quarter numbers"
        ));
    }

    #[test]
    fn final_supersedes_the_partial_without_double_counting() {
        let mut d = dedup();
        d.record_them_partial(1.0, 3.0, "the third quarter numbers");
        d.record_them(1.0, 3.2, "the third quarter numbers");
        assert!(d.partial.is_none());
        // Doubled text would be matched in full if the partial were still pooled alongside the final.
        assert!(!d.is_echo(
            1.2,
            3.3,
            "the third quarter numbers the third quarter numbers"
        ));
        assert!(d.is_echo(1.2, 3.3, "the third quarter numbers"));
    }

    #[test]
    fn a_partial_for_the_next_turn_survives_the_previous_final() {
        let mut d = dedup();
        d.record_them_partial(5.0, 6.0, "and then the budget review");
        d.record_them(1.0, 3.0, "so what about the third quarter numbers");
        assert!(d.partial.is_some());
    }

    #[test]
    fn partial_expires_beyond_retain() {
        let mut d = dedup();
        d.record_them_partial(1.0, 3.0, "so what about the third quarter numbers");
        d.record_them(20.0, 22.0, "unrelated later remark from the far end");
        assert!(d.partial.is_none());
        assert!(!d.is_echo(1.2, 3.3, "so what about the third quarter numbers"));
    }

    #[test]
    fn partial_outside_the_window_is_not_matched() {
        let mut d = dedup();
        d.record_them_partial(1.0, 3.0, "so what about the third quarter numbers");
        assert!(!d.is_echo(6.0, 8.0, "so what about the third quarter numbers"));
    }
}

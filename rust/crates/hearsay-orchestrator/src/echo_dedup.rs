//! Text-level echo dedup: a backstop for acoustic echo cancellation.
//!
//! On speakers the remote party ("Them") leaks into the microphone ("Me"). The acoustic canceller
//! ([`crate::aec`]) removes most of that leakage, but a linear filter cannot fully cancel cheap
//! laptop speakers (nonlinear distortion, a time-varying echo path), so residual Them audio can
//! still reach `hearsay-me` and be transcribed a second time, attributed to the local user. This
//! module drops a Me *final* whose text is an echo of a recently transcribed Them final.
//!
//! Where AEC works on the signal, this works on the transcript: a pure, dependency-free complement
//! that removes whatever residual still lands as text. It touches only the live Me finals that
//! would otherwise be persisted and broadcast — never the archive or the offline refine.
//!
//! ## Streaming order
//!
//! Only finalized Them segments are recorded as candidates. The physics favors this: Them is tapped
//! *pre-speaker*, so its ASR runs earlier and on cleaner audio than the Me echo, which is delayed by
//! the playout + acoustic round trip. By the time a Me echo finalizes, the matching Them final is
//! almost always already recorded. A Them final that arrives *after* its Me echo is not caught — an
//! accepted limitation of a streaming backstop.

use std::collections::VecDeque;

/// Tuning for [`EchoDedup`]. Defaults are deliberately conservative: short utterances are never
/// dropped (so backchannels like "yeah" / "right" survive) and only a near-complete, in-order text
/// match inside a concurrent time window counts as an echo.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EchoDedupConfig {
    /// A Me final shorter than this many tokens is never treated as an echo.
    pub min_tokens: usize,
    /// Fraction of the Me final's tokens that must appear, in order, in the concurrent Them text.
    pub similarity: f64,
    /// Slack (seconds) on each side when deciding whether a Them final is concurrent with the Me
    /// final — covers playout + acoustic + ASR-endpoint skew between the two streams.
    pub window_s: f64,
    /// How long (seconds) a Them final stays a candidate reference after it ends.
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

/// A rolling window of recent Them finals, matched against incoming Me finals.
pub(crate) struct EchoDedup {
    cfg: EchoDedupConfig,
    recent: VecDeque<ThemEntry>,
    /// The latest segment end time seen on either stream, i.e. "now" in meeting time. Drives pruning.
    latest_s: f64,
}

impl EchoDedup {
    pub fn new(cfg: EchoDedupConfig) -> Self {
        Self {
            cfg,
            recent: VecDeque::new(),
            latest_s: 0.0,
        }
    }

    /// Record a finalized Them segment as a candidate echo source.
    pub fn record_them(&mut self, start_s: f64, end_s: f64, text: &str) {
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

    /// True if a Me final is an echo of concurrent Them speech and should be dropped.
    pub fn is_echo(&mut self, start_s: f64, end_s: f64, text: &str) -> bool {
        self.latest_s = self.latest_s.max(end_s);
        self.prune();

        let me = normalize(text);
        if me.len() < self.cfg.min_tokens {
            return false;
        }

        // Pool the tokens of every concurrent Them final, in time order, into one reference
        // sequence: Them may have been endpointed into several finals across the span the Me echo
        // covers as a single final.
        let mut pool: Vec<&str> = Vec::new();
        for e in &self.recent {
            if e.start_s <= end_s + self.cfg.window_s && e.end_s >= start_s - self.cfg.window_s {
                pool.extend(e.tokens.iter().map(String::as_str));
            }
        }
        if pool.is_empty() {
            return false;
        }

        let me_refs: Vec<&str> = me.iter().map(String::as_str).collect();
        let covered = lcs_len(&me_refs, &pool) as f64 / me.len() as f64;
        covered >= self.cfg.similarity
    }

    fn prune(&mut self) {
        let cutoff = self.latest_s - self.cfg.retain_s;
        while self.recent.front().is_some_and(|e| e.end_s < cutoff) {
            self.recent.pop_front();
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

/// Length of the longest common subsequence of two token sequences (rolling one-row DP).
fn lcs_len(a: &[&str], b: &[&str]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let mut dp = vec![0usize; b.len() + 1];
    for x in a {
        let mut prev_diag = 0; // dp[i-1][j-1] before this cell is overwritten
        for (j, y) in b.iter().enumerate() {
            let tmp = dp[j + 1];
            dp[j + 1] = if x == y {
                prev_diag + 1
            } else {
                dp[j + 1].max(dp[j])
            };
            prev_diag = tmp;
        }
    }
    dp[b.len()]
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
}

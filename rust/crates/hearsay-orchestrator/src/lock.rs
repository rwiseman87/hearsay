//! Poison-tolerant `std::sync::Mutex` locking.
//!
//! The orchestrator's mutexes guard small, wholesale-replaced state (the active session, the
//! background-handle list, a last-activity `Instant`) and are never held across an `.await`. If a
//! thread panics while holding one, `.lock().unwrap()` would poison it and turn every later meeting
//! operation into a panic — a wedged process instead of a degraded one. Recovering the guard keeps
//! the app running; the guarded values are replaced whole, so a poisoned read is at worst stale, not
//! torn.

use std::sync::{Mutex, MutexGuard};

pub(crate) trait MutexExt<T> {
    /// Lock, recovering the guard if a panicking holder poisoned the mutex.
    fn lock_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn lock_recover(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

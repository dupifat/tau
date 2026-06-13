//! Counting semaphore with owned permits.
//!
//! Permits are owned so they can be moved across thread boundaries —
//! the dispatcher loop acquires a permit *before* spawning the worker
//! that holds it, which bounds the in-flight thread count rather than
//! just the concurrent-execution count.

use std::sync::{Arc, Condvar, Mutex};

pub(crate) struct Semaphore {
    state: Mutex<usize>,
    cond: Condvar,
}

/// Owned permit; releases on drop.
pub(crate) struct OwnedPermit(Arc<Semaphore>);

impl Semaphore {
    pub(crate) fn new(permits: usize) -> Self {
        Self {
            state: Mutex::new(permits),
            cond: Condvar::new(),
        }
    }

    /// Try to take a permit without blocking the caller.
    pub(crate) fn try_acquire(self: &Arc<Self>) -> Option<OwnedPermit> {
        let mut count = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if *count == 0 {
            return None;
        }
        *count -= 1;
        Some(OwnedPermit(Arc::clone(self)))
    }
}

impl Drop for OwnedPermit {
    fn drop(&mut self) {
        let mut count = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        *count += 1;
        self.0.cond.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ensures callers can reject excess work without blocking the protocol
    /// reader when all worker permits are already held.
    #[test]
    fn try_acquire_returns_none_when_saturated() {
        let sem = Arc::new(Semaphore::new(1));
        let _permit = sem.try_acquire().expect("initial permit");

        assert!(sem.try_acquire().is_none());
    }
}

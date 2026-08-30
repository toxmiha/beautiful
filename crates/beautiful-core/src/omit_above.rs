//! Transform / text underlay: omit layer indices without mutating `Layer::visible`.
//!
//! Dense composite parallelizes rows with rayon. A thread-local omit list is
//! invisible to worker threads and would bake the omitted layer back in (ghost).
//! [`WorkerTlsGuard`] copies the snapshot onto each rayon worker for the job.

use std::cell::RefCell;

thread_local! {
    static OMIT: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// RAII: while alive, [`is_omitted`] returns true for listed layer indices
/// on the installing thread. Pair with [`WorkerTlsGuard`] inside rayon jobs.
pub struct OmitAboveGuard;

impl OmitAboveGuard {
    pub fn install(indices: impl IntoIterator<Item = usize>) -> Self {
        OMIT.with(|cell| {
            let mut v = cell.borrow_mut();
            v.clear();
            v.extend(indices);
            v.sort_unstable();
            v.dedup();
        });
        Self
    }
}

impl Drop for OmitAboveGuard {
    fn drop(&mut self) {
        OMIT.with(|cell| cell.borrow_mut().clear());
    }
}

/// Copy omit indices onto a rayon worker; clears TLS when the worker drops this.
pub struct WorkerTlsGuard;

impl WorkerTlsGuard {
    pub fn install(indices: &[usize]) -> Self {
        OMIT.with(|cell| {
            let mut v = cell.borrow_mut();
            v.clear();
            v.extend_from_slice(indices);
        });
        Self
    }
}

impl Drop for WorkerTlsGuard {
    fn drop(&mut self) {
        OMIT.with(|cell| cell.borrow_mut().clear());
    }
}

/// Snapshot for rayon `for_each_init` (call on the thread that holds [`OmitAboveGuard`]).
pub fn snapshot() -> Vec<usize> {
    OMIT.with(|cell| cell.borrow().clone())
}

#[inline]
pub fn is_omitted(li: usize) -> bool {
    OMIT.with(|cell| {
        let v = cell.borrow();
        !v.is_empty() && v.binary_search(&li).is_ok()
    })
}

//! Background job primitives (cancel + progress).
//!
//! # If painting regresses after a memory/perf change
//! 1. Reproduce with one tool / one canvas size.
//! 2. Classify: stroke_stack / composite / engine / tip / canvas_gpu / I/O.
//! 3. Fix in that module — or revert the phase; do not leave dual dense||tiled forever.
//! 4. Soft brush dots → check spacing (no hardness-based widen); run
//!    `soft_low_hardness_stroke_stays_continuous`.
//! 5. Blank screen → projection/gpu_dirty vs dab bounds.
//! 6. RAM spike → ensure full-doc Vec did not return in CompositeCache/StrokeStack.
//! 7. `cargo_check` + unit tests → release `dist/beautiful.exe` → smoke paint.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug)]
pub struct CancelToken {
    cancelled: AtomicBool,
}

impl CancelToken {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cancelled: AtomicBool::new(false),
        })
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Default)]
pub struct JobProgress {
    pub done: AtomicU64,
    pub total: AtomicU64,
}

impl JobProgress {
    pub fn fraction(&self) -> f32 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        let done = self.done.load(Ordering::Relaxed).min(total);
        done as f32 / total as f32
    }

    pub fn set(&self, done: u64, total: u64) {
        self.total.store(total, Ordering::Relaxed);
        self.done.store(done, Ordering::Relaxed);
    }
}

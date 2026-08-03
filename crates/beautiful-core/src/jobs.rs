//! Background job primitives (cancel token).
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

use std::sync::atomic::{AtomicBool, Ordering};
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

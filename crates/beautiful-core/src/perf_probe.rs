//! Lightweight phase probes for the app microprofiler.
//!
//! Core records elapsed micros into thread-locals; the app drains them into
//! named spans. Zero cost when the app does not call [`take_*`] (values just
//! accumulate until drained / ignored next stroke).

use std::cell::Cell;
use std::time::Instant;

thread_local! {
    static BRUSH_US: Cell<u64> = const { Cell::new(0) };
    static BLEND_US: Cell<u64> = const { Cell::new(0) };
    static COMPOSE_US: Cell<u64> = const { Cell::new(0) };
}

#[inline]
pub fn add_brush_us(us: u64) {
    BRUSH_US.with(|c| c.set(c.get().saturating_add(us)));
}

#[inline]
pub fn add_blend_us(us: u64) {
    BLEND_US.with(|c| c.set(c.get().saturating_add(us)));
}

#[inline]
pub fn add_compose_us(us: u64) {
    COMPOSE_US.with(|c| c.set(c.get().saturating_add(us)));
}

pub fn take_brush_us() -> u64 {
    BRUSH_US.with(|c| c.replace(0))
}

pub fn take_blend_us() -> u64 {
    BLEND_US.with(|c| c.replace(0))
}

pub fn take_compose_us() -> u64 {
    COMPOSE_US.with(|c| c.replace(0))
}

/// RAII timer that adds elapsed micros to a sink on drop.
pub struct Probe {
    t0: Instant,
    sink: fn(u64),
}

impl Probe {
    pub fn brush() -> Self {
        Self {
            t0: Instant::now(),
            sink: add_brush_us,
        }
    }

    pub fn blend() -> Self {
        Self {
            t0: Instant::now(),
            sink: add_blend_us,
        }
    }

    pub fn compose() -> Self {
        Self {
            t0: Instant::now(),
            sink: add_compose_us,
        }
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        (self.sink)(self.t0.elapsed().as_micros() as u64);
    }
}

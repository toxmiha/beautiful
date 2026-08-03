//! Transform underlay: omit layer indices without mutating `Layer::visible`.
//!
//! Used by Free Transform underlay sync so Soft Light / above layers can be
//! excluded from the dense plate without eye-toggle side effects.

use std::cell::RefCell;

thread_local! {
    static OMIT: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// RAII: while alive, [`is_omitted`] returns true for listed layer indices.
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

#[inline]
pub fn is_omitted(li: usize) -> bool {
    OMIT.with(|cell| {
        let v = cell.borrow();
        !v.is_empty() && v.binary_search(&li).is_ok()
    })
}

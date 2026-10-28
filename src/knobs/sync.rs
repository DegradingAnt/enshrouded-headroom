//! Atomics for shared knob fields. Normal builds use the exact standard-library types.
//!
//! Loom atomics cannot be const-constructed. Model builds therefore keep const standard-backed
//! construction for untouched runtime globals and provide modeled construction for local knobs.

pub(super) use std::sync::atomic::Ordering;

#[cfg(not(all(test, loom)))]
pub(super) use std::sync::atomic::{AtomicU32, AtomicU64};

/// A const runtime-global atomic or a local Loom atomic, only in the model-test build.
#[cfg(all(test, loom))]
pub(super) enum AtomicU32 {
    /// Const construction must not enter a Loom model before one exists.
    Static(std::sync::atomic::AtomicU32),
    /// A local model knob routes every operation through Loom.
    Modeled(::loom::sync::atomic::AtomicU32),
}

#[cfg(all(test, loom))]
impl AtomicU32 {
    /// Preserve const standard-backed global construction in the model-test build.
    pub(super) const fn new(value: u32) -> Self {
        Self::Static(std::sync::atomic::AtomicU32::new(value))
    }

    /// Construct an instrumented local field inside loom::model.
    pub(super) fn modeled(value: u32) -> Self {
        Self::Modeled(::loom::sync::atomic::AtomicU32::new(value))
    }

    /// Forward the unchanged load ordering to the selected atomic.
    pub(super) fn load(&self, order: Ordering) -> u32 {
        match self {
            Self::Static(value) => value.load(order),
            Self::Modeled(value) => value.load(order),
        }
    }

    /// Forward the unchanged store ordering to the selected atomic.
    pub(super) fn store(&self, value: u32, order: Ordering) {
        match self {
            Self::Static(atomic) => atomic.store(value, order),
            Self::Modeled(atomic) => atomic.store(value, order),
        }
    }

    /// Forward the unchanged swap ordering to the selected atomic.
    pub(super) fn swap(&self, value: u32, order: Ordering) -> u32 {
        match self {
            Self::Static(atomic) => atomic.swap(value, order),
            Self::Modeled(atomic) => atomic.swap(value, order),
        }
    }

    /// Forward the unchanged counter ordering to the selected atomic.
    pub(super) fn fetch_add(&self, value: u32, order: Ordering) -> u32 {
        match self {
            Self::Static(atomic) => atomic.fetch_add(value, order),
            Self::Modeled(atomic) => atomic.fetch_add(value, order),
        }
    }
}

/// A const runtime-global timestamp or a local Loom timestamp, only in model-test builds.
#[cfg(all(test, loom))]
pub(super) enum AtomicU64 {
    /// Existing globals remain standard-backed and const-constructible.
    Static(std::sync::atomic::AtomicU64),
    /// Local model timestamps participate in Loom scheduling and memory visibility.
    Modeled(::loom::sync::atomic::AtomicU64),
}

#[cfg(all(test, loom))]
impl AtomicU64 {
    /// Preserve const standard-backed construction for runtime globals.
    pub(super) const fn new(value: u64) -> Self {
        Self::Static(std::sync::atomic::AtomicU64::new(value))
    }

    /// Instrument a local timestamp inside a model.
    pub(super) fn modeled(value: u64) -> Self {
        Self::Modeled(::loom::sync::atomic::AtomicU64::new(value))
    }

    /// Forward the unchanged timestamp load ordering.
    pub(super) fn load(&self, order: Ordering) -> u64 {
        match self {
            Self::Static(value) => value.load(order),
            Self::Modeled(value) => value.load(order),
        }
    }

    /// Forward the unchanged timestamp store ordering.
    pub(super) fn store(&self, value: u64, order: Ordering) {
        match self {
            Self::Static(atomic) => atomic.store(value, order),
            Self::Modeled(atomic) => atomic.store(value, order),
        }
    }
}

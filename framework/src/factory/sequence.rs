//! Monotonic counter for seeding unique-per-call factory fields.
//!
//! ```ignore
//! use suprnova::factory::Sequence;
//!
//! static ORDER_IDS: Sequence = Sequence::new();
//!
//! struct OrderFactory;
//! impl Factory for OrderFactory {
//!     type Model = Order;
//!     fn definition() -> Order {
//!         Order {
//!             id: ORDER_IDS.next(),
//!             total: Faker.fake(),
//!         }
//!     }
//! }
//! ```
//!
//! `next()` starts at 1 on a fresh Sequence and increments by 1 each
//! call. `reset()` returns the counter to 0 so the next `next()`
//! returns 1 again — useful for between-test isolation when a test
//! suite shares a `static Sequence`.
//!
//! Backed by `AtomicI64` with `SeqCst` ordering. Calls from concurrent
//! threads return distinct values; the strong ordering is overkill for
//! "give me a unique id" but keeps reasoning trivial. If a Sequence
//! ever shows up in a hot path, downgrade to `Relaxed` after benching.

use std::sync::atomic::{AtomicI64, Ordering};

pub struct Sequence {
    counter: AtomicI64,
}

impl Sequence {
    pub const fn new() -> Self {
        Self {
            counter: AtomicI64::new(0),
        }
    }

    /// Atomically return the next value. First call after `new()` or
    /// `reset()` returns 1.
    pub fn next(&self) -> i64 {
        self.counter.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Reset the counter to 0. The next `next()` returns 1.
    pub fn reset(&self) {
        self.counter.store(0, Ordering::SeqCst);
    }
}

impl Default for Sequence {
    fn default() -> Self {
        Self::new()
    }
}

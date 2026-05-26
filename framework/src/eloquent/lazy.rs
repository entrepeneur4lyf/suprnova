//! Phase 10C T8 — `LazyCollection<M>` stream wrapper.
//!
//! Stream-based row-by-row iteration over a `Builder<M>` query result.
//! Returned by [`Builder::lazy`], [`Builder::lazy_by_id`], and
//! [`Builder::cursor`] (Laravel alias for `lazy`).
//!
//! Internally batches via PK-cursor pagination (`id > last_id`) so the
//! stream stays memory-bounded by the batch size — never the full
//! result set. The stream yields one `M` at a time; callers drive it
//! with `.next().await` in a `while let Some(item) = ...` loop.
//!
//! ## Why a newtype instead of `impl Stream`?
//!
//! Returning `LazyCollection<M>` instead of `impl Stream<Item = ...>`
//! keeps the return shape stable across compiler versions and makes
//! the type nameable for storage in user structs. The wrapper exposes
//! `.next().await` directly so user code doesn't need a `use
//! futures::StreamExt;` import to drive iteration — same ergonomic
//! shape as `tokio::sync::mpsc::Receiver::recv`.
//!
//! ## Backpressure
//!
//! Each `.next().await` triggers the next row delivery; the underlying
//! batched fetch only runs when the in-batch buffer drains. A slow
//! consumer doesn't accumulate rows in memory — the stream waits at
//! the await point until the consumer is ready.
//!
//! ## Example
//!
//! ```ignore
//! use suprnova::Model;
//!
//! let mut stream = User::query().lazy();
//! while let Some(row) = stream.next().await {
//!     let user = row?;
//!     println!("{}", user.email);
//! }
//! ```

use crate::error::FrameworkError;
use futures::Stream;
use std::pin::Pin;

/// A bounded-memory stream of model rows, returned by
/// [`Builder::lazy`][crate::eloquent::Builder::lazy],
/// [`Builder::lazy_by_id`][crate::eloquent::Builder::lazy_by_id], and
/// [`Builder::cursor`][crate::eloquent::Builder::cursor].
///
/// Drive iteration via `.next().await` — yields `Some(Ok(row))` per
/// row, `Some(Err(_))` on database failure, and `None` when the result
/// set is exhausted.
///
/// The wrapper is `Send` (the inner stream's bound) so it can cross
/// `tokio::spawn` boundaries; not `Sync` because the underlying
/// generator borrow is single-consumer by construction.
pub struct LazyCollection<M> {
    inner: Pin<Box<dyn Stream<Item = Result<M, FrameworkError>> + Send>>,
}

impl<M> LazyCollection<M> {
    /// Wrap a `Stream<Item = Result<M, FrameworkError>>` into a
    /// `LazyCollection<M>`. The builder constructs these directly via
    /// `async_stream::try_stream!`; user code rarely calls this — the
    /// public entry points are `Builder::lazy` / `lazy_by_id` /
    /// `cursor`.
    pub fn boxed(stream: impl Stream<Item = Result<M, FrameworkError>> + Send + 'static) -> Self {
        Self {
            inner: Box::pin(stream),
        }
    }

    /// Yield the next row. Returns `None` when the stream is
    /// exhausted; `Some(Err(_))` propagates the underlying database
    /// failure once and then transitions to exhausted.
    pub async fn next(&mut self) -> Option<Result<M, FrameworkError>> {
        use futures::StreamExt;
        self.inner.next().await
    }
}

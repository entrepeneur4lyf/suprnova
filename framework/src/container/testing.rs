//! Test utilities for the Application Container.
//!
//! Two flavours of test isolation:
//!
//! - **Thread-local — [`TestContainer::fake`]**: ergonomic for sync tests and
//!   `#[tokio::test]` running on the default `current_thread` flavour. The
//!   override is bound to the thread that called `fake()`. Tasks spawned with
//!   `tokio::spawn` (which can run on different worker threads) do NOT see
//!   the override.
//!
//! - **Task-local — [`TestContainer::scope`]**: async-safe across multi-thread
//!   runtimes (`#[tokio::test(flavor = "multi_thread")]`). The override is
//!   bound to the future passed to `scope`, so it persists across awaits even
//!   if the runtime migrates the future between worker threads. Bind your
//!   fakes inside the scoped future via `TestContainer::bind` / `singleton`
//!   / `factory` — those calls route through the task-local first.
//!
//! Lookup order in [`crate::App`]: task-local, then thread-local, then global.
//! Mutation helpers ([`TestContainer::bind`], etc.) write to whichever scope
//! is currently active (task-local takes precedence over thread-local).
//!
//! # Example — thread-local
//!
//! ```rust,ignore
//! use suprnova::testing::{TestContainer, TestContainerGuard};
//!
//! #[tokio::test]
//! async fn test_with_fake_service() {
//!     // Set up test container - automatically cleared when guard is dropped
//!     let _guard = TestContainer::fake();
//!
//!     // Register fake implementations
//!     TestContainer::bind::<dyn HttpClient>(Arc::new(FakeHttpClient::new()));
//!
//!     // App::make() will now return the fake
//!     let client: Arc<dyn HttpClient> = App::make::<dyn HttpClient>().unwrap();
//! }
//! ```
//!
//! # Example — task-local (multi-thread runtime)
//!
//! ```rust,ignore
//! use suprnova::testing::TestContainer;
//!
//! #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
//! async fn test_async_safe() {
//!     TestContainer::scope(async {
//!         TestContainer::bind::<dyn HttpClient>(Arc::new(FakeHttpClient::new()));
//!         // App::make() inside this future sees the fake even after
//!         // awaits that hop between worker threads.
//!         do_async_work().await;
//!     })
//!     .await;
//! }
//! ```

use super::{Container, TASK_CONTAINER, TEST_CONTAINER};
use std::any::Any;
use std::future::Future;
use std::sync::{Arc, RwLock};

/// Test utilities for the container
///
/// Provides methods to set up isolated test containers with fake implementations.
/// Test containers use thread-local storage, so tests run in parallel won't interfere.
pub struct TestContainer;

impl TestContainer {
    /// Set up a test container with overrides
    ///
    /// Returns a guard that clears the test container when dropped.
    /// This ensures test isolation - each test gets a fresh container.
    ///
    /// # Example
    /// ```rust,ignore
    /// #[tokio::test]
    /// async fn my_test() {
    ///     let _guard = TestContainer::fake();
    ///     // Register fakes...
    /// } // Container automatically cleared here
    /// ```
    pub fn fake() -> TestContainerGuard {
        TEST_CONTAINER.with(|c| {
            *c.borrow_mut() = Some(Container::new());
        });
        TestContainerGuard
    }

    /// Run a future with a task-local test container override.
    ///
    /// Async-safe alternative to [`TestContainer::fake`]. Use this for
    /// `#[tokio::test(flavor = "multi_thread")]` or any test where the
    /// future may migrate between worker threads — the task-local
    /// override persists for the entire future regardless of which
    /// thread the runtime picks.
    ///
    /// Bind your fakes inside the scoped future via the usual
    /// `TestContainer::bind` / `singleton` / `factory` helpers — those
    /// route through the task-local first, so a call inside `scope`
    /// writes to the task-local container.
    ///
    /// # Caveat — `tokio::spawn`
    ///
    /// tokio task-locals are NOT inherited by `tokio::spawn`'d
    /// sub-tasks. If your test spawns sub-tasks that need to read the
    /// fakes, wrap each sub-task with another `TestContainer::scope`
    /// (and re-register the fakes inside it). The framework test suite
    /// currently has zero `TestContainer::*` + `tokio::spawn` overlaps
    /// where the spawn body reads from `App`, so this remains a
    /// documented caveat rather than an automatic propagation.
    ///
    /// # Example
    /// ```rust,ignore
    /// #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    /// async fn test_async_safe() {
    ///     TestContainer::scope(async {
    ///         TestContainer::bind::<dyn HttpClient>(Arc::new(FakeHttpClient::new()));
    ///         do_async_work().await;  // sees the fake across worker hops
    ///     })
    ///     .await;
    /// }
    /// ```
    pub async fn scope<Fut: Future>(future: Fut) -> Fut::Output {
        let container = Arc::new(RwLock::new(Container::new()));
        TASK_CONTAINER.scope(container, future).await
    }

    /// Register a fake singleton for testing.
    ///
    /// Writes to the active scope: task-local takes precedence over
    /// thread-local, so calls inside a [`TestContainer::scope`] block
    /// land on the task-local container; calls under a
    /// [`TestContainer::fake`] guard land on the thread-local container.
    /// Outside either scope this is a no-op.
    ///
    /// # Example
    /// ```rust,ignore
    /// TestContainer::singleton(FakeDatabase::new());
    /// ```
    pub fn singleton<T: Any + Send + Sync + 'static>(instance: T) {
        // Task-local first.
        let task_done = TASK_CONTAINER.try_with(|c| c.clone()).ok();
        if let Some(container) = task_done
            && let Ok(mut c) = container.write()
        {
            c.singleton(instance);
            return;
        }
        // Fall back to thread-local.
        TEST_CONTAINER.with(|c| {
            if let Some(ref mut container) = *c.borrow_mut() {
                container.singleton(instance);
            }
        });
    }

    /// Register a fake factory for testing.
    ///
    /// Writes to the active scope — see [`TestContainer::singleton`] for
    /// the precedence rules.
    ///
    /// # Example
    /// ```rust,ignore
    /// TestContainer::factory(|| FakeLogger::new());
    /// ```
    pub fn factory<T, F>(factory: F)
    where
        T: Any + Send + Sync + 'static,
        F: Fn() -> T + Send + Sync + 'static,
    {
        let task_done = TASK_CONTAINER.try_with(|c| c.clone()).ok();
        if let Some(container) = task_done
            && let Ok(mut c) = container.write()
        {
            c.factory(factory);
            return;
        }
        TEST_CONTAINER.with(|c| {
            if let Some(ref mut container) = *c.borrow_mut() {
                container.factory(factory);
            }
        });
    }

    /// Bind a fake trait implementation for testing.
    ///
    /// Writes to the active scope — see [`TestContainer::singleton`] for
    /// the precedence rules.
    ///
    /// # Example
    /// ```rust,ignore
    /// TestContainer::bind::<dyn HttpClient>(Arc::new(FakeHttpClient::new()));
    /// ```
    pub fn bind<T: ?Sized + Send + Sync + 'static>(instance: Arc<T>) {
        let task_done = TASK_CONTAINER.try_with(|c| c.clone()).ok();
        if let Some(container) = task_done
            && let Ok(mut c) = container.write()
        {
            c.bind(instance);
            return;
        }
        TEST_CONTAINER.with(|c| {
            if let Some(ref mut container) = *c.borrow_mut() {
                container.bind(instance);
            }
        });
    }

    /// Bind a fake trait factory for testing.
    ///
    /// Writes to the active scope — see [`TestContainer::singleton`] for
    /// the precedence rules.
    ///
    /// # Example
    /// ```rust,ignore
    /// TestContainer::bind_factory::<dyn HttpClient>(|| Arc::new(FakeHttpClient::new()));
    /// ```
    pub fn bind_factory<T: ?Sized + Send + Sync + 'static, F>(factory: F)
    where
        F: Fn() -> Arc<T> + Send + Sync + 'static,
    {
        let task_done = TASK_CONTAINER.try_with(|c| c.clone()).ok();
        if let Some(container) = task_done
            && let Ok(mut c) = container.write()
        {
            c.bind_factory(factory);
            return;
        }
        TEST_CONTAINER.with(|c| {
            if let Some(ref mut container) = *c.borrow_mut() {
                container.bind_factory(factory);
            }
        });
    }
}

/// Guard that clears the test container when dropped
///
/// This ensures test isolation by automatically cleaning up the thread-local
/// test container when the guard goes out of scope.
pub struct TestContainerGuard;

impl Drop for TestContainerGuard {
    fn drop(&mut self) {
        TEST_CONTAINER.with(|c| {
            *c.borrow_mut() = None;
        });
        // Phase 10C T12 — the named-connection registry is process-
        // global (OnceLock<RwLock<HashMap>>), so it survives the
        // thread-local container reset. Wipe it on guard drop so the
        // next test in the same process starts with no `__read_replica__`
        // or other named connection registered.
        //
        // Why ConnectionRegistry is safe to clear here but the eloquent
        // listener / scope registries are NOT (see AF4):
        // `ConnectionRegistry` is keyed by string name; each test
        // chooses a unique name and reaches for it explicitly via
        // `DB::named`. Stale entries don't bleed semantics into
        // unrelated parallel tests.
        //
        // The eloquent `EventDispatcher`, `clear_cancellable_listeners`,
        // and `ScopeRegistry` are keyed by `TypeId::<M>()` — the
        // current discipline is that each test uses a unique model
        // struct so the registrations don't collide. Wiping those
        // registries from `Drop` would break parallel test execution
        // (test A's drop clearing test B's still-needed listeners),
        // so AF4 ships sync `clear()` on each as opt-in (documented in
        // the framework tests' README) and does NOT call them here.
        crate::database::ConnectionRegistry::clear();
    }
}

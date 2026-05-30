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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use tokio::task::JoinHandle;

/// Live `TestContainerGuard` count.
///
/// Each `TestContainer::fake()` bumps it on creation; each guard drop
/// decrements. The process-global named-`ConnectionRegistry` is only
/// wiped when the count transitions back to zero, so a guard dropping
/// in one parallel test cannot erase a connection name still being used
/// by another concurrent test. Required because `ConnectionRegistry`
/// itself is `OnceLock<RwLock<HashMap>>` — purely process-global, no
/// per-test scoping — while the thread-local `TEST_CONTAINER` it sits
/// next to is per-test.
static FAKE_GUARDS: AtomicUsize = AtomicUsize::new(0);

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
        FAKE_GUARDS.fetch_add(1, Ordering::SeqCst);
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
    /// # Spawning sub-tasks
    ///
    /// tokio task-locals are NOT inherited by bare `tokio::spawn`'d
    /// sub-tasks. Use [`TestContainer::spawn`] (which captures the
    /// current task-local container and re-installs it inside the
    /// spawned future) any time a test spawns a sub-task that needs
    /// to read the fakes via `App::make` / `App::resolve`.
    ///
    /// # Example
    /// ```rust,ignore
    /// #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    /// async fn test_async_safe() {
    ///     TestContainer::scope(async {
    ///         TestContainer::bind::<dyn HttpClient>(Arc::new(FakeHttpClient::new()));
    ///         do_async_work().await;  // sees the fake across worker hops
    ///         TestContainer::spawn(async {
    ///             // also sees the fake — task-local was captured and re-installed
    ///             let client = App::make::<dyn HttpClient>().unwrap();
    ///         })
    ///         .await
    ///         .unwrap();
    ///     })
    ///     .await;
    /// }
    /// ```
    pub async fn scope<Fut: Future>(future: Fut) -> Fut::Output {
        let container = Arc::new(RwLock::new(Container::new()));
        TASK_CONTAINER.scope(container, future).await
    }

    /// Spawn an async task that inherits the current task-local test
    /// container.
    ///
    /// `tokio::spawn` does not inherit task-locals — a future spawned
    /// from inside a [`TestContainer::scope`] block would otherwise
    /// see only the global container. This helper captures the
    /// current scope's `Arc<RwLock<Container>>` and re-installs it
    /// inside the spawned future, so test fakes registered via
    /// `TestContainer::bind` / `singleton` / `factory` remain visible
    /// to the sub-task. The same `Arc` is shared, so bindings added
    /// in the sub-task become visible to the parent (and any other
    /// concurrent sub-tasks) on commit; this matches the semantics
    /// of `TestContainer::*` inside the parent scope.
    ///
    /// Outside a `scope` block this falls through to `tokio::spawn`
    /// unchanged.
    ///
    /// # Example
    /// ```rust,ignore
    /// TestContainer::scope(async {
    ///     TestContainer::bind::<dyn HttpClient>(Arc::new(FakeHttpClient::new()));
    ///     let h = TestContainer::spawn(async {
    ///         // resolves the fake — task-local propagated
    ///         App::make::<dyn HttpClient>().unwrap()
    ///     });
    ///     let _client = h.await.unwrap();
    /// })
    /// .await;
    /// ```
    pub fn spawn<Fut>(future: Fut) -> JoinHandle<Fut::Output>
    where
        Fut: Future + Send + 'static,
        Fut::Output: Send + 'static,
    {
        match TASK_CONTAINER.try_with(|c| c.clone()) {
            Ok(container) => {
                tokio::spawn(async move { TASK_CONTAINER.scope(container, future).await })
            }
            Err(_) => tokio::spawn(future),
        }
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
        // The thread-local container reset above isolates this test's
        // service bindings. The named-connection registry is a
        // separate, process-global `OnceLock<RwLock<HashMap>>` and so
        // survives that reset; we only wipe it when the *last* live
        // `TestContainerGuard` drops.
        //
        // Why the refcount matters: `ConnectionRegistry` is keyed by
        // string name. The reserved name `__read_replica__` is the
        // canonical one tests use to exercise read-write split routing,
        // so two parallel tests that both register `__read_replica__`
        // would step on each other if any one of their guard drops
        // wiped the shared registry. Tests that touch the named
        // registry still gate themselves with `#[serial_test::serial]`,
        // but the refcount makes the guard safe to use from any test
        // (including ones that have no connections registered) without
        // accidentally clearing another test's entries.
        //
        // The eloquent `EventDispatcher`, `clear_cancellable_listeners`,
        // and `ScopeRegistry` are keyed by `TypeId::<M>()` — the
        // current discipline is that each test uses a unique model
        // struct so the registrations don't collide. Wiping those
        // registries from `Drop` would break parallel test execution
        // (test A's drop clearing test B's still-needed listeners),
        // so each ships a sync `clear()` as opt-in (documented in
        // the framework tests' README) and we do NOT call them here.
        if FAKE_GUARDS.fetch_sub(1, Ordering::SeqCst) == 1 {
            crate::database::ConnectionRegistry::clear();
        }
    }
}

#[cfg(test)]
mod refcount_tests {
    //! Regression: a guard dropping in one parallel test must not wipe
    //! a `ConnectionRegistry` entry another concurrent test still
    //! depends on. The refcount in `FAKE_GUARDS` makes the clear
    //! conditional on being the *last* live guard.
    //!
    //! These tests are `#[serial]` because they mutate the registry's
    //! global state and assert on guard-count transitions; the harness
    //! must not have other tests holding `TestContainer::fake` guards
    //! at the same time. Other test modules in this crate already
    //! follow the same `#[serial]` discipline for registry mutation.
    use super::*;
    use crate::database::ConnectionRegistry;
    use crate::database::testing::TestDatabase;
    use serial_test::serial;

    #[tokio::test]
    #[serial]
    async fn guard_drop_with_others_alive_preserves_named_connections() {
        // Start from a known-empty registry so we're asserting on the
        // refcount behaviour rather than residual entries.
        ConnectionRegistry::clear();

        // Two concurrent fakes: simulate two parallel test bodies.
        let outer = TestContainer::fake();
        let count_after_outer = FAKE_GUARDS.load(Ordering::SeqCst);
        assert!(
            count_after_outer >= 1,
            "fake() must bump the live-guard counter"
        );

        // Outer test registers a named connection it still needs.
        let db = TestDatabase::sqlite_memory().await.unwrap();
        ConnectionRegistry::register_existing("refcount_outer_test", db.db().clone())
            .await
            .unwrap();
        assert!(ConnectionRegistry::has("refcount_outer_test").await);

        // Inner test starts and immediately ends. Before the fix this
        // would wipe `refcount_outer_test` because the inner guard's
        // drop called `ConnectionRegistry::clear()` unconditionally.
        {
            let _inner = TestContainer::fake();
        }

        assert!(
            ConnectionRegistry::has("refcount_outer_test").await,
            "an inner guard drop must NOT clear named connections still owned by outer guards",
        );

        // Outer drop is the last guard — registry gets cleared as part
        // of teardown.
        drop(outer);

        // `db` itself still holds a `TestContainerGuard` via
        // `TestDatabase`, so even after dropping `outer` the count is
        // not yet zero. Drop the test database to release it.
        drop(db);

        assert!(
            !ConnectionRegistry::has("refcount_outer_test").await,
            "the last guard drop must clear named connections for next-test isolation",
        );
    }
}

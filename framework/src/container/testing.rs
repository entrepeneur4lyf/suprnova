//! Test utilities for the Application Container
//!
//! Provides mechanisms for test isolation via thread-local container overrides.
//!
//! # Example
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

use super::{Container, TEST_CONTAINER};
use std::any::Any;
use std::sync::Arc;

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

    /// Register a fake singleton for testing
    ///
    /// # Example
    /// ```rust,ignore
    /// TestContainer::singleton(FakeDatabase::new());
    /// ```
    pub fn singleton<T: Any + Send + Sync + 'static>(instance: T) {
        TEST_CONTAINER.with(|c| {
            if let Some(ref mut container) = *c.borrow_mut() {
                container.singleton(instance);
            }
        });
    }

    /// Register a fake factory for testing
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
        TEST_CONTAINER.with(|c| {
            if let Some(ref mut container) = *c.borrow_mut() {
                container.factory(factory);
            }
        });
    }

    /// Bind a fake trait implementation for testing
    ///
    /// # Example
    /// ```rust,ignore
    /// TestContainer::bind::<dyn HttpClient>(Arc::new(FakeHttpClient::new()));
    /// ```
    pub fn bind<T: ?Sized + Send + Sync + 'static>(instance: Arc<T>) {
        TEST_CONTAINER.with(|c| {
            if let Some(ref mut container) = *c.borrow_mut() {
                container.bind(instance);
            }
        });
    }

    /// Bind a fake trait factory for testing
    ///
    /// # Example
    /// ```rust,ignore
    /// TestContainer::bind_factory::<dyn HttpClient>(|| Arc::new(FakeHttpClient::new()));
    /// ```
    pub fn bind_factory<T: ?Sized + Send + Sync + 'static, F>(factory: F)
    where
        F: Fn() -> Arc<T> + Send + Sync + 'static,
    {
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
    }
}

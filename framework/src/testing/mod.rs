//! Testing utilities for suprnova framework
//!
//! Provides Jest-like testing helpers including:
//! - `expect!` macro for fluent assertions with clear expected/received output
//! - `describe!` and `test!` macros for test organization
//! - `TestDatabase` for isolated database tests
//! - `TestContainer` for dependency injection in tests
//!
//! # Example
//!
//! ```rust,ignore
//! use suprnova::{describe, test, expect};
//! use suprnova::testing::TestDatabase;
//!
//! describe!("UserService", {
//!     test!("creates a user", async fn(db: TestDatabase) {
//!         let service = UserService::new();
//!         let user = service.create("test@example.com").await.unwrap();
//!
//!         expect!(user.email).to_equal("test@example.com".to_string());
//!     });
//! });
//! ```

mod expect;

pub use crate::container::testing::{TestContainer, TestContainerGuard};
pub use crate::database::testing::TestDatabase;
pub use expect::{set_current_test_name, Expect};

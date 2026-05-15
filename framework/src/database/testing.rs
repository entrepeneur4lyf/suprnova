//! Testing utilities for database operations
//!
//! Provides `TestDatabase` for setting up isolated test environments with
//! in-memory SQLite databases and automatic migration support.
//!
//! # Example
//!
//! ```rust,ignore
//! use suprnova::test_database;
//!
//! #[tokio::test]
//! async fn test_create_user() {
//!     let db = test_database!();
//!
//!     // Your test code here - actions using DB::connection()
//!     // will automatically use this test database
//! }
//! ```

use sea_orm::DatabaseConnection;
use sea_orm_migration::MigratorTrait;

use super::config::DatabaseConfig;
use super::connection::DbConnection;
use crate::container::testing::{TestContainer, TestContainerGuard};
use crate::error::FrameworkError;

/// Test database wrapper that provides isolated database environments
///
/// Each `TestDatabase` creates a fresh in-memory SQLite database with
/// migrations applied. The database is automatically registered in the
/// test container, so any code using `DB::connection()` or `#[inject] db: Database`
/// will receive this test database.
///
/// When the `TestDatabase` is dropped, the test container is cleared,
/// ensuring complete isolation between tests.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::testing::TestDatabase;
/// use crate::migrations::Migrator;
///
/// #[tokio::test]
/// async fn test_user_creation() {
///     let db = TestDatabase::fresh::<Migrator>().await.unwrap();
///
///     // Actions using DB::connection() automatically get this test database
///     let action = CreateUserAction::new();
///     let user = action.execute("test@example.com").await.unwrap();
///
///     // Query directly using db.conn()
///     let found = users::Entity::find_by_id(user.id)
///         .one(db.conn())
///         .await
///         .unwrap();
///     assert!(found.is_some());
/// }
/// ```
pub struct TestDatabase {
    conn: DbConnection,
    _guard: TestContainerGuard,
}

impl TestDatabase {
    /// Create a fresh test database with migrations applied
    ///
    /// This creates an in-memory SQLite database, runs all migrations,
    /// and registers the connection in the test container.
    ///
    /// # Type Parameters
    ///
    /// * `M` - The migrator type implementing `MigratorTrait`. Typically
    ///   this is `crate::migrations::Migrator` from your application.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Database connection fails
    /// - Migration execution fails
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use suprnova::testing::TestDatabase;
    /// use crate::migrations::Migrator;
    ///
    /// #[tokio::test]
    /// async fn test_example() {
    ///     let db = TestDatabase::fresh::<Migrator>().await.unwrap();
    ///     // ...
    /// }
    /// ```
    pub async fn fresh<M: MigratorTrait>() -> Result<Self, FrameworkError> {
        // 1. Create test container guard for isolation
        let guard = TestContainer::fake();

        // 2. Create in-memory SQLite database
        let config = DatabaseConfig::builder()
            .url("sqlite::memory:")
            .max_connections(1)
            .min_connections(1)
            .logging(false)
            .build();

        let conn = DbConnection::connect(&config).await?;

        // 3. Run migrations
        M::up(conn.inner(), None)
            .await
            .map_err(|e| FrameworkError::database(format!("Migration failed: {}", e)))?;

        // 4. Register in TestContainer - this is the key integration!
        // Any code calling DB::connection() or App::resolve::<DbConnection>()
        // will now get this test database
        TestContainer::singleton(conn.clone());

        Ok(Self { conn, _guard: guard })
    }

    /// Get a reference to the underlying database connection
    ///
    /// Use this when you need to execute queries directly in your tests.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let db = test_database!();
    /// let users = users::Entity::find().all(db.conn()).await?;
    /// ```
    pub fn conn(&self) -> &DatabaseConnection {
        self.conn.inner()
    }

    /// Get the `DbConnection` wrapper
    ///
    /// Use this when you need the full `DbConnection` type.
    pub fn db(&self) -> &DbConnection {
        &self.conn
    }
}

/// Create a test database with default migrator
///
/// This macro creates a `TestDatabase` using `crate::migrations::Migrator` as the
/// default migrator. This follows the suprnova convention where migrations are defined
/// in `src/migrations/mod.rs`.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::test_database;
///
/// #[tokio::test]
/// async fn test_user_creation() {
///     let db = test_database!();
///
///     let action = CreateUserAction::new();
///     let user = action.execute("test@example.com").await.unwrap();
///     assert!(user.id > 0);
/// }
/// ```
///
/// # With Custom Migrator
///
/// ```rust,ignore
/// let db = test_database!(my_crate::CustomMigrator);
/// ```
#[macro_export]
macro_rules! test_database {
    () => {
        $crate::testing::TestDatabase::fresh::<$crate::migrations::Migrator>()
            .await
            .expect("Failed to set up test database")
    };
    ($migrator:ty) => {
        $crate::testing::TestDatabase::fresh::<$migrator>()
            .await
            .expect("Failed to set up test database")
    };
}

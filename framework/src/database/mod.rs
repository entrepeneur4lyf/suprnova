//! Database module for suprnova framework
//!
//! Provides a SeaORM-based ORM with Laravel-like API.
//!
//! # Quick Start
//!
//! ```rust,no_run
//! # use suprnova::{Config, DB, DatabaseConfig};
//! # struct User;
//! # impl User {
//! #     fn find() -> Query { Query }
//! # }
//! # struct Query;
//! # impl Query {
//! #     async fn all<C>(self, _conn: &C) -> Result<Vec<String>, suprnova::FrameworkError> { Ok(vec![]) }
//! # }
//! # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
//! // 1. Register database config (in config/mod.rs)
//! Config::register(DatabaseConfig::from_env());
//!
//! // 2. Initialize connection (in bootstrap.rs)
//! DB::init().await.expect("Failed to connect to database");
//!
//! // 3. Use in controllers
//! let conn = DB::connection()?;
//! let users = User::find().all(conn.inner()).await?;
//! # Ok(()) }
//! ```
//!
//! # Configuration
//!
//! Set these environment variables:
//!
//! ```env
//! DATABASE_URL=postgres://user:pass@localhost:5432/mydb
//! # or for MySQL:
//! DATABASE_URL=mysql://user:pass@localhost:3306/mydb
//! # or for SQLite:
//! DATABASE_URL=sqlite://./database.db
//!
//! # Optional:
//! DB_MAX_CONNECTIONS=10
//! DB_MIN_CONNECTIONS=1
//! DB_CONNECT_TIMEOUT=30
//! DB_LOGGING=false
//! ```

pub mod config;
pub mod connection;
pub mod connection_registry;
pub mod db_facade;
pub mod dynamic_row;
pub mod events;
pub mod identifier;
pub mod model;
pub mod query_builder;
pub mod route_binding;
pub mod testing;
pub mod transaction;

pub use config::{DatabaseConfig, DatabaseConfigBuilder, DatabaseType, UrlSource};
pub use connection::DbConnection;
pub use connection_registry::{
    ConnectionRegistry, PRIMARY_CONNECTION_NAME, READ_REPLICA_CONNECTION_NAME,
};
pub use db_facade::DbTableBuilder;
pub use dynamic_row::DynamicRow;
pub use events::{
    ConnectionEstablished, DatabaseBusy, QueryExecuted, QueryListener, ReadWriteType,
    TransactionBeginning, TransactionCommitted, TransactionRolledBack,
};
pub use identifier::{validate_identifier, validate_sql_operator};
pub use model::{EntityExt, EntityExtMut};
pub use query_builder::QueryBuilder;
pub use route_binding::{AutoRouteBinding, RouteBinding, RouteParam};
pub use testing::TestDatabase;
pub use transaction::{Transaction, TxHandle};

/// Injectable database connection type
///
/// This is an alias for `DbConnection` that can be used with dependency injection.
/// Use with the `#[inject]` attribute in actions and services for cleaner database access.
///
/// # Example
///
/// ```rust,no_run
/// # use suprnova::{injectable, Database};
/// # struct User;
/// # impl User { fn find() -> Query { Query } }
/// # struct Query;
/// # impl Query {
/// #     async fn all<C>(self, _conn: &C) -> Result<Vec<String>, suprnova::FrameworkError> { Ok(vec![]) }
/// # }
/// #[injectable]
/// pub struct CreateUserAction {
///     #[inject]
///     db: Database,
/// }
///
/// impl CreateUserAction {
///     pub async fn execute(&self) -> Result<(), Box<dyn std::error::Error>> {
///         let users = User::find().all(self.db.conn()).await?;
///         Ok(())
///     }
/// }
/// ```
pub type Database = DbConnection;

use crate::error::FrameworkError;
use crate::{App, Config};

/// Database facade - main entry point for database operations
///
/// Provides static methods for initializing and accessing the database connection.
/// The connection is stored in the application container as a singleton.
///
/// # Example
///
/// ```rust,no_run
/// # use suprnova::{DB, DatabaseConfig, Config};
/// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
/// // Initialize (usually in bootstrap.rs)
/// Config::register(DatabaseConfig::from_env());
/// DB::init().await?;
///
/// // Use anywhere in your app
/// let conn = DB::connection()?;
/// # Ok(()) }
/// ```
pub struct DB;

impl DB {
    /// Initialize the database connection
    ///
    /// Reads configuration from `DatabaseConfig` (must be registered via Config system)
    /// and establishes a connection pool. The connection is stored in the App container.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `DatabaseConfig` is not registered
    /// - Connection to the database fails
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use suprnova::DB;
    /// // In bootstrap.rs
    /// pub async fn register() {
    ///     DB::init().await.expect("Failed to connect to database");
    /// }
    /// ```
    pub async fn init() -> Result<(), FrameworkError> {
        let config = Config::get::<DatabaseConfig>().ok_or_else(|| {
            FrameworkError::internal(
                "DatabaseConfig not registered. Call Config::register(DatabaseConfig::from_env()) first.",
            )
        })?;

        // Refuse the silent SQLite fallback in production-like
        // environments. `validate_for_environment` is a no-op in
        // Local/Development/Testing/Custom envs so the documented dev
        // posture ("zero-setup SQLite") is preserved.
        config.validate_for_environment(&Config::environment())?;

        let connection = DbConnection::connect(&config).await?;
        App::singleton(connection);
        Ok(())
    }

    /// Initialize with a custom config
    ///
    /// Useful for testing or when you need to connect to a different database.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use suprnova::{DB, DatabaseConfig};
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// let config = DatabaseConfig::builder()
    ///     .url("sqlite::memory:")
    ///     .build();
    /// DB::init_with(config).await?;
    /// # Ok(()) }
    /// ```
    pub async fn init_with(config: DatabaseConfig) -> Result<(), FrameworkError> {
        // Same production guard as `init`: refuse the silent SQLite
        // fallback in production-like environments. Passing
        // `DatabaseConfig::from_env()` here with no `DATABASE_URL` set
        // would otherwise boot against the dev fallback in production.
        // No-op for Local/Development/Testing/Custom envs and for any
        // config carrying an explicit URL (`sqlite::memory:` tests and
        // builder URLs pass unchanged).
        config.validate_for_environment(&Config::environment())?;

        let connection = DbConnection::connect(&config).await?;
        App::singleton(connection);
        Ok(())
    }

    /// Get the database connection
    ///
    /// Returns the connection from the App container. The connection is wrapped
    /// in a `DbConnection` which provides convenient access to the underlying
    /// SeaORM `DatabaseConnection`.
    ///
    /// # Errors
    ///
    /// Returns an error if `DB::init()` was not called.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use suprnova::DB;
    /// # struct User;
    /// # impl User { fn find() -> Query { Query } }
    /// # struct Query;
    /// # impl Query {
    /// #     async fn all<C>(self, _conn: &C) -> Result<Vec<String>, suprnova::FrameworkError> { Ok(vec![]) }
    /// # }
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// let conn = DB::connection()?;
    ///
    /// // Use with SeaORM queries
    /// let users = User::find()
    ///     .all(conn.inner())
    ///     .await?;
    /// # Ok(()) }
    /// ```
    pub fn connection() -> Result<DbConnection, FrameworkError> {
        App::resolve::<DbConnection>()
    }

    /// Check if the database connection is initialized
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use suprnova::DB;
    /// # fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// if DB::is_connected() {
    ///     let conn = DB::connection()?;
    ///     // ...
    /// }
    /// # Ok(()) }
    /// ```
    pub fn is_connected() -> bool {
        App::has::<DbConnection>()
    }

    /// Get the database connection for use with SeaORM
    ///
    /// This is a convenience alias for `DB::connection()`. The returned
    /// `DbConnection` implements `Deref<Target=DatabaseConnection>`, so you
    /// can use it directly with SeaORM methods.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use suprnova::database::DB;
    /// use sea_orm::{Set, ActiveModelTrait};
    ///
    /// let new_todo = todos::ActiveModel {
    ///     title: Set("My Todo".to_string()),
    ///     ..Default::default()
    /// };
    ///
    /// // Use &* to dereference to &DatabaseConnection
    /// let inserted = new_todo.insert(&*DB::get()?).await?;
    ///
    /// // Or use .inner() method
    /// let inserted = new_todo.insert(DB::get()?.inner()).await?;
    /// ```
    pub fn get() -> Result<DbConnection, FrameworkError> {
        Self::connection()
    }

    /// Phase 10C T12 — register an auxiliary database connection under
    /// `name`. The primary pool is registered through [`Self::init`] /
    /// [`Self::init_with`]; this method is for read replicas, sharded
    /// shards, and per-model "warehouse" pools.
    ///
    /// Per-query routing: chain [`Builder::on(name)`] or
    /// [`Model::on(name)`]. Per-model default: tag the model with
    /// `#[model(connection = "name")]`.
    ///
    /// `__primary__` is reserved — registering under that name fails.
    /// `__read_replica__` is the well-known read-replica name; when
    /// registered, every read-shape terminal method on
    /// [`Builder<M>`](crate::eloquent::Builder) auto-routes through it.
    /// Writes (`create` / `save` / `delete`) ignore the replica.
    ///
    /// [`Builder::on(name)`]: crate::eloquent::Builder::on
    /// [`Model::on(name)`]: crate::eloquent::Model
    pub async fn register_named(name: &str, config: DatabaseConfig) -> Result<(), FrameworkError> {
        ConnectionRegistry::register(name, config).await
    }

    /// Look up the connection registered under `name`. Errors when no
    /// connection is registered — no automatic fallback to the primary
    /// (would mask misconfiguration).
    pub async fn named(name: &str) -> Result<DbConnection, FrameworkError> {
        ConnectionRegistry::get(name).await
    }
}

// Re-export sea_orm types that users commonly need
pub use sea_orm;

#[cfg(test)]
mod init_with_tests {
    use super::*;
    use serial_test::serial;

    /// Restores `APP_ENV` / `DATABASE_URL` after the test and starts
    /// from a clean slate. `init_with` reads the environment through
    /// `Config::environment()` (process-wide), so these tests must run
    /// serially and undo their mutations.
    struct EnvGuard {
        keys: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let saved = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for k in keys {
                // SAFETY: tests are gated with #[serial] so no other
                // test concurrently reads or mutates these env vars.
                unsafe {
                    std::env::remove_var(k);
                }
            }
            Self { keys: saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.keys {
                // SAFETY: same as above — serial test, single-threaded
                // env mutation.
                unsafe {
                    match v {
                        Some(value) => std::env::set_var(k, value),
                        None => std::env::remove_var(k),
                    }
                }
            }
        }
    }

    #[tokio::test]
    #[serial]
    async fn init_with_refuses_default_sqlite_in_production() {
        let _guard = EnvGuard::new(&["APP_ENV", "DATABASE_URL"]);
        // SAFETY: serial test.
        unsafe {
            std::env::set_var("APP_ENV", "production");
        }

        // DATABASE_URL unset → from_env records UrlSource::Default.
        let config = DatabaseConfig::from_env();
        assert_eq!(config.url_source, UrlSource::Default);

        let err = DB::init_with(config)
            .await
            .expect_err("init_with must refuse the dev SQLite fallback in production");
        assert!(
            format!("{err}").contains("DATABASE_URL is required"),
            "unexpected error: {err}",
        );
    }

    #[tokio::test]
    #[serial]
    async fn init_with_accepts_explicit_url_in_production() {
        let _guard = EnvGuard::new(&["APP_ENV", "DATABASE_URL"]);
        // SAFETY: serial test.
        unsafe {
            std::env::set_var("APP_ENV", "production");
        }

        // An explicit builder URL is UrlSource::Explicit — the operator
        // chose it, so production must accept it.
        let config = DatabaseConfig::builder().url("sqlite::memory:").build();
        assert_eq!(config.url_source, UrlSource::Explicit);

        DB::init_with(config)
            .await
            .expect("explicit URL must pass the production guard");
    }

    #[tokio::test]
    #[serial]
    async fn init_with_accepts_default_sqlite_in_testing() {
        let _guard = EnvGuard::new(&["APP_ENV", "DATABASE_URL"]);
        // SAFETY: serial test.
        unsafe {
            std::env::set_var("APP_ENV", "testing");
            std::env::set_var("DATABASE_URL", "sqlite::memory:");
        }

        // Testing is not production-like, so even the env-sourced URL
        // (or the silent fallback) is allowed — the guard is a no-op.
        let config = DatabaseConfig::from_env();
        assert_eq!(config.url_source, UrlSource::Env);

        DB::init_with(config)
            .await
            .expect("testing environment must not trip the production guard");
    }
}

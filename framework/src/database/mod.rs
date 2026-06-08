//! Database module for suprnova framework
//!
//! Provides a SeaORM-based ORM with Laravel-like API.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use suprnova::{Config, DB, DatabaseConfig};
//!
//! // 1. Register database config (in config/mod.rs)
//! Config::register(DatabaseConfig::from_env());
//!
//! // 2. Initialize connection (in bootstrap.rs)
//! DB::init().await.expect("Failed to connect to database");
//!
//! // 3. Use in controllers
//! let conn = DB::connection()?;
//! let users = User::find().all(conn.inner()).await?;
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
/// ```rust,ignore
/// use suprnova::{injectable, Database};
///
/// #[injectable]
/// pub struct CreateUserAction {
///     #[inject]
///     db: Database,
/// }
///
/// impl CreateUserAction {
///     pub async fn execute(&self) {
///         let users = User::find().all(self.db.conn()).await?;
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
/// ```rust,ignore
/// use suprnova::{DB, DatabaseConfig, Config};
///
/// // Initialize (usually in bootstrap.rs)
/// Config::register(DatabaseConfig::from_env());
/// DB::init().await?;
///
/// // Use anywhere in your app
/// let conn = DB::connection()?;
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
    /// ```rust,ignore
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

        // Audit HIGH `database` #1: refuse the silent SQLite fallback
        // in production-like environments. `validate_for_environment`
        // is a no-op in Local/Development/Testing/Custom envs so the
        // documented dev posture ("zero-setup SQLite") is preserved.
        config.validate_for_environment(&crate::Config::environment())?;

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
    /// ```rust,ignore
    /// let config = DatabaseConfig::builder()
    ///     .url("sqlite::memory:")
    ///     .build();
    /// DB::init_with(config).await?;
    /// ```
    pub async fn init_with(config: DatabaseConfig) -> Result<(), FrameworkError> {
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
    /// ```rust,ignore
    /// let conn = DB::connection()?;
    ///
    /// // Use with SeaORM queries
    /// let users = User::find()
    ///     .all(conn.inner())
    ///     .await?;
    /// ```
    pub fn connection() -> Result<DbConnection, FrameworkError> {
        App::resolve::<DbConnection>()
    }

    /// Check if the database connection is initialized
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// if DB::is_connected() {
    ///     let conn = DB::connection()?;
    ///     // ...
    /// }
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

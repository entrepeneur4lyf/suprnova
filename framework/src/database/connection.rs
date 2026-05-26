//! Database connection management

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use std::sync::Arc;
use std::time::Duration;

use crate::database::config::DatabaseConfig;
use crate::error::FrameworkError;

/// Wrapper around SeaORM's DatabaseConnection
///
/// This provides a clonable, thread-safe connection that can be stored
/// in the application container and shared across requests.
///
/// # Example
///
/// ```rust,ignore
/// let conn = DbConnection::connect(&config).await?;
///
/// // Use with SeaORM queries
/// let users = User::find().all(conn.inner()).await?;
/// ```
#[derive(Clone)]
pub struct DbConnection {
    inner: Arc<DatabaseConnection>,
}

impl DbConnection {
    /// Create a new database connection from config
    ///
    /// This establishes a connection pool using the provided configuration.
    /// For SQLite databases, this will automatically create the database file
    /// if it doesn't exist.
    pub async fn connect(config: &DatabaseConfig) -> Result<Self, FrameworkError> {
        // For SQLite, ensure the database file can be created
        let url = if config.url.starts_with("sqlite://") {
            // Extract the file path from the URL
            let path = config.url.trim_start_matches("sqlite://");
            let path = path.trim_start_matches("./");

            // Don't apply to in-memory databases
            if path != ":memory:" && !path.starts_with(":memory:") {
                // Create parent directories if needed
                if let Some(parent) = std::path::Path::new(path).parent()
                    && !parent.as_os_str().is_empty()
                {
                    std::fs::create_dir_all(parent).ok();
                }

                // Touch the file to create it if it doesn't exist
                if !std::path::Path::new(path).exists() {
                    std::fs::File::create(path).ok();
                }
            }

            // Use the file path format that SQLite prefers with create mode
            format!("sqlite:{}?mode=rwc", path)
        } else {
            config.url.clone()
        };

        let mut opt = ConnectOptions::new(&url);
        opt.max_connections(config.max_connections)
            .min_connections(config.min_connections)
            .connect_timeout(Duration::from_secs(config.connect_timeout))
            .sqlx_logging(config.logging);

        let conn = Database::connect(opt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;

        Ok(Self {
            inner: Arc::new(conn),
        })
    }

    /// Wrap an existing SeaORM `DatabaseConnection` as a `DbConnection`.
    ///
    /// Intended for tests and advanced setups that build their own
    /// connection (e.g. in-memory SQLite via `sea_orm::Database::connect`)
    /// and want it visible to `DB::connection()` after registering it
    /// in the container.
    pub fn from_raw(conn: sea_orm::DatabaseConnection) -> Self {
        Self {
            inner: Arc::new(conn),
        }
    }

    /// Get a reference to the underlying SeaORM connection
    ///
    /// Use this when you need to execute raw SeaORM queries.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let conn = DB::connection()?;
    /// let users = User::find()
    ///     .filter(user::Column::Active.eq(true))
    ///     .all(conn.inner())
    ///     .await?;
    /// ```
    pub fn inner(&self) -> &DatabaseConnection {
        &self.inner
    }

    /// Get a reference to the database connection (short alias for `inner()`)
    ///
    /// Use this when passing to SeaORM queries. This provides a cleaner API
    /// than using the `Deref` implementation with `&*`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let users = User::find().all(self.db.conn()).await?;
    /// ```
    pub fn conn(&self) -> &DatabaseConnection {
        &self.inner
    }
}

// Domain 6 audit D6-3 — `pub fn is_closed(&self) -> bool` was removed.
// The previous implementation hardcoded `false` with a comment saying
// "SeaORM doesn't expose this directly, but we can check via ping"; in
// practice it lied about every connection's state. `ping().is_err()`
// would also conflate transient network blips with closed-state and
// would still leave the public name `is_closed` semantically wrong (a
// failed ping is "unhealthy", not "closed"). With one caller — a
// tautological `assert!(!conn.is_closed())` in the registry round-trip
// test — removing the method is cleaner than papering over it.

impl AsRef<DatabaseConnection> for DbConnection {
    fn as_ref(&self) -> &DatabaseConnection {
        &self.inner
    }
}

impl std::ops::Deref for DbConnection {
    type Target = DatabaseConnection;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

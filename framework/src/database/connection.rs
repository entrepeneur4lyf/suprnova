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
        Self::connect_as(config, crate::database::PRIMARY_CONNECTION_NAME).await
    }

    /// Open a pool and fire `ConnectionEstablished` carrying the
    /// logical name `connection_name`. The public [`Self::connect`]
    /// delegates here with `__primary__`; [`ConnectionRegistry::register`]
    /// calls it directly with the caller-supplied name so multi-pool
    /// observers see the real connection identifier in the event.
    pub(crate) async fn connect_as(
        config: &DatabaseConfig,
        connection_name: &str,
    ) -> Result<Self, FrameworkError> {
        // Validate pool config before SeaORM silently accepts a
        // misconfigured `ConnectOptions` (e.g. a zero-sized pool that
        // immediately starves callers).
        config.validate_pool()?;

        // For SQLite, ensure the database file can be created
        let url = if config.url.starts_with("sqlite://") {
            // Normalize once: separate the file portion from any query
            // string the caller already supplied so filesystem ops never
            // run against a query-polluted path, and rebuild the URL with
            // `mode=rwc` merged exactly once.
            let (path, normalized) = normalize_sqlite_url(&config.url);

            // Don't apply to in-memory databases
            if path != ":memory:" && !path.starts_with(":memory:") {
                // Create parent directories if needed. `?mode=rwc` makes
                // SQLite create the database FILE, but it will not make
                // PARENT DIRECTORIES — propagate any failure here with
                // path context so misconfigured paths and permission
                // problems surface as a clear filesystem diagnostic
                // instead of a downstream "unable to open database file."
                if let Some(parent) = std::path::Path::new(&path).parent()
                    && !parent.as_os_str().is_empty()
                {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        FrameworkError::database(format!(
                            "failed to create SQLite parent directory `{}`: {e}",
                            parent.display(),
                        ))
                    })?;
                }
            }

            normalized
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

        let result = Self {
            inner: Arc::new(conn),
        };
        // Fire ConnectionEstablished. The default pool reaches this
        // path with __primary__ semantics; named pools flow through
        // here from ConnectionRegistry::register with their registered
        // name. Listeners observe the connection name; failures are
        // logged-only — a listener bug must not block the pool from
        // coming up.
        let event = crate::database::events::ConnectionEstablished {
            connection_name: connection_name.to_string(),
        };
        if crate::EventFacade::has_listeners::<crate::database::events::ConnectionEstablished>()
            && let Err(e) = crate::EventFacade::dispatch_best_effort(event).await
        {
            tracing::warn!(
                target: "suprnova::database",
                error = %e,
                "ConnectionEstablished listener returned error; ignoring",
            );
        }
        Ok(result)
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

/// Normalize a `sqlite://` URL into `(file_path, connect_url)`.
///
/// Splits the file portion from any caller-supplied query string so
/// filesystem ops only ever see the file, strips a leading `./`, and
/// rebuilds the connect URL with `mode=rwc` merged exactly once. An
/// input that already carries a query (`sqlite://file.db?cache=shared`)
/// keeps that query intact instead of being double-suffixed.
///
/// In-memory targets (`:memory:`) are returned with their query
/// preserved verbatim — `mode=rwc` is meaningless there and SQLite
/// rejects an unexpected `?mode=rwc` on `:memory:`.
pub(crate) fn normalize_sqlite_url(url: &str) -> (String, String) {
    let without_scheme = url.trim_start_matches("sqlite://");
    let (raw_path, query) = match without_scheme.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (without_scheme, None),
    };
    let path = raw_path.trim_start_matches("./");

    if path == ":memory:" || path.starts_with(":memory:") {
        let connect_url = match query {
            Some(q) => format!("sqlite:{path}?{q}"),
            None => format!("sqlite:{path}"),
        };
        return (path.to_string(), connect_url);
    }

    (
        path.to_string(),
        format!("sqlite:{}?{}", path, merge_rwc_query(query)),
    )
}

/// Merge `mode=rwc` into an optional SQLite query string exactly once.
///
/// SQLite needs `mode=rwc` to create the database file on connect. We
/// must add it without (a) duplicating it when the caller already
/// supplied it and (b) clobbering a deliberate `mode=` the caller set
/// (e.g. `mode=ro`). Any other params (`cache=shared`, …) are preserved
/// verbatim and in order.
fn merge_rwc_query(query: Option<&str>) -> String {
    let existing = query.unwrap_or("");
    if existing.is_empty() {
        return "mode=rwc".to_string();
    }
    // If a `mode=` param is already present, the caller's choice wins —
    // appending a second `mode=` would be ambiguous to SQLite.
    let has_mode = existing
        .split('&')
        .any(|pair| pair == "mode" || pair.starts_with("mode="));
    if has_mode {
        existing.to_string()
    } else {
        format!("{existing}&mode=rwc")
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

#[cfg(test)]
mod sqlite_url_tests {
    use super::normalize_sqlite_url;

    #[test]
    fn plain_path_gets_single_rwc() {
        let (path, url) = normalize_sqlite_url("sqlite://database.sqlite");
        assert_eq!(path, "database.sqlite");
        assert_eq!(url, "sqlite:database.sqlite?mode=rwc");
    }

    #[test]
    fn leading_dot_slash_is_stripped() {
        let (path, url) = normalize_sqlite_url("sqlite://./database.db");
        assert_eq!(path, "database.db");
        assert_eq!(url, "sqlite:database.db?mode=rwc");
    }

    #[test]
    fn existing_mode_rwc_is_not_duplicated() {
        // The bug: `sqlite://file.db?mode=rwc` previously produced
        // `sqlite:file.db?mode=rwc?mode=rwc` and a query-polluted path.
        let (path, url) = normalize_sqlite_url("sqlite://file.db?mode=rwc");
        assert_eq!(path, "file.db", "filesystem path must be query-free");
        assert_eq!(url, "sqlite:file.db?mode=rwc");
        // Exactly one query separator and one mode param.
        assert_eq!(url.matches('?').count(), 1);
        assert_eq!(url.matches("mode=rwc").count(), 1);
    }

    #[test]
    fn other_query_params_are_preserved_with_rwc_merged() {
        let (path, url) = normalize_sqlite_url("sqlite://file.db?cache=shared");
        assert_eq!(path, "file.db", "filesystem path must be query-free");
        assert_eq!(url, "sqlite:file.db?cache=shared&mode=rwc");
        assert_eq!(url.matches('?').count(), 1);
        assert_eq!(url.matches("mode=rwc").count(), 1);
    }

    #[test]
    fn caller_mode_choice_is_not_clobbered() {
        // A deliberate `mode=ro` must win — we don't append a conflicting
        // second `mode=`.
        let (path, url) = normalize_sqlite_url("sqlite://file.db?mode=ro");
        assert_eq!(path, "file.db");
        assert_eq!(url, "sqlite:file.db?mode=ro");
        assert_eq!(url.matches("mode=").count(), 1);
    }

    #[test]
    fn in_memory_is_left_alone() {
        let (path, url) = normalize_sqlite_url("sqlite://:memory:");
        assert_eq!(path, ":memory:");
        assert_eq!(url, "sqlite::memory:");
    }

    #[test]
    fn in_memory_with_query_preserves_query_without_rwc() {
        let (path, url) = normalize_sqlite_url("sqlite://:memory:?cache=shared");
        assert_eq!(path, ":memory:");
        assert_eq!(url, "sqlite::memory:?cache=shared");
        assert!(!url.contains("mode=rwc"));
    }
}

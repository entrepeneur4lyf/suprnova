//! Application Bootstrap
//!
//! Registers global middleware and services before the server starts accepting requests.
//!
//! Middleware order matters. Requests flow through the stack top-to-bottom;
//! responses travel back bottom-to-top:
//!
//!   BearerTokenMiddleware -> IncludeMiddleware -> handlers
//!
//! # Switching databases
//!
//! The default is SQLite. To use PostgreSQL set DATABASE_URL in .env:
//!   DATABASE_URL=postgres://user:pass@localhost:5432/{package_name}
//!
//! then change `ToriiConfig::sqlite_in_memory()` below to:
//!   ToriiConfig::from_sea_orm(DB::connection().await)

use suprnova::{
    global_middleware, init_torii, BearerTokenMiddleware, IncludeMiddleware, ToriiConfig, DB,
};

/// Register global middleware and services.
///
/// Called from `main()` before the server starts.
pub async fn register() {
    // Initialise the database connection pool.
    DB::init().await.expect("Failed to connect to database");

    // Initialise Torii authentication.
    // Uses an in-memory SQLite by default for zero-config development.
    // Change to ToriiConfig::from_sea_orm(DB::connection().await) for production.
    let torii_config = ToriiConfig::sqlite_in_memory()
        .await
        .expect("Failed to create Torii config");
    init_torii(torii_config)
        .await
        .expect("Failed to initialise Torii");

    // Bearer-token authentication -- reads Authorization: Bearer <token> and
    // populates the authenticated user in request context.
    global_middleware!(BearerTokenMiddleware);

    // Include middleware -- parses ?include=relation1,relation2 and
    // ?fields[type]=field1,field2 query parameters into task-locals
    // so Resource::collection / Resource::single can resolve them.
    global_middleware!(IncludeMiddleware);
}

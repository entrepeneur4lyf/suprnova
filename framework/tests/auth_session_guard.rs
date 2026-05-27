//! Integration tests for [`suprnova::SessionGuard`]'s lifecycle events
//! and the login → logout round trip.
//!
//! Event assertions use the process-global [`EventFacade::fake`], so the
//! tests in this file serialize on `TEST_LOCK`. Each `tests/*.rs` is its
//! own binary, which isolates this file's fake store from the fakes used
//! by other test files.
//!
//! DB-touching tests run on a shared [`Runtime`] (sqlx pools die with the
//! runtime that created them), mirroring `tests/remember_me.rs`.

use once_cell::sync::Lazy;
use sea_orm_migration::MigratorTrait;
use sea_orm_migration::prelude::*;
use std::any::Any;
use std::sync::Arc;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

use suprnova::auth::events::{Attempting, Authenticated, Failed, Login, Logout};
use suprnova::auth::request_state;
use suprnova::events::testing::{assert_dispatched, assert_not_dispatched};
use suprnova::{
    Authenticatable, Credentials, EventFacade, FrameworkError, SessionGuard, StatefulGuard,
    UserProvider,
};

/// Shared runtime — SQLx pools die with their creating runtime, so every
/// DB-touching path runs here rather than on a per-test runtime.
static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("tokio runtime"));

/// Serializes the event-fake critical sections; the fake store is
/// process-global.
static TEST_LOCK: Mutex<()> = Mutex::const_new(());

/// One-shot: install Crypt, register a shared in-memory SQLite connection
/// in the global container, and migrate `sessions` + `remember_tokens`.
static SETUP: Lazy<()> = Lazy::new(|| {
    let key = suprnova::EncryptionKey::generate();
    let _ = suprnova::crypto::_test_install_key(key);

    RT.block_on(async {
        let config = suprnova::database::DatabaseConfig::builder()
            .url("sqlite::memory:")
            .max_connections(1)
            .min_connections(1)
            .logging(false)
            .build();
        let conn = suprnova::database::DbConnection::connect(&config)
            .await
            .expect("connect in-memory sqlite");
        LocalMigrator::up(conn.inner(), None)
            .await
            .expect("run local migrator");
        suprnova::App::singleton(conn);
    });
});

/// Local migrator: just the `sessions` and `remember_tokens` tables the
/// session + remember-me code reads.
struct LocalMigrator;

#[async_trait::async_trait]
impl MigratorTrait for LocalMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(CreateSessionsTable),
            Box::new(CreateRememberTokensTable),
        ]
    }
}

struct CreateSessionsTable;

impl MigrationName for CreateSessionsTable {
    fn name(&self) -> &str {
        "m20240101_000001_create_sessions_table"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateSessionsTable {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Sessions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Sessions::Id)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Sessions::UserId).string().null())
                    .col(ColumnDef::new(Sessions::Payload).text().not_null())
                    .col(ColumnDef::new(Sessions::CsrfToken).string().not_null())
                    .col(
                        ColumnDef::new(Sessions::LastActivity)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Sessions::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Sessions {
    Table,
    Id,
    UserId,
    Payload,
    CsrfToken,
    LastActivity,
}

struct CreateRememberTokensTable;

impl MigrationName for CreateRememberTokensTable {
    fn name(&self) -> &str {
        "m20240101_000002_create_remember_tokens_table"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateRememberTokensTable {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(RememberTokens::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(RememberTokens::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(RememberTokens::UserId).string().not_null())
                    .col(ColumnDef::new(RememberTokens::Selector).string().not_null())
                    .col(
                        ColumnDef::new(RememberTokens::TokenHash)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RememberTokens::ExpiresAt)
                            .timestamp()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RememberTokens::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(RememberTokens::LastUsedAt)
                            .timestamp()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_test_remember_tokens_selector")
                    .table(RememberTokens::Table)
                    .col(RememberTokens::Selector)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(RememberTokens::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum RememberTokens {
    Table,
    Id,
    UserId,
    Selector,
    TokenHash,
    ExpiresAt,
    CreatedAt,
    LastUsedAt,
}

#[derive(Clone)]
struct TestUser {
    id: String,
}

impl Authenticatable for TestUser {
    fn auth_identifier(&self) -> i64 {
        self.id.parse().unwrap_or(0)
    }
    fn get_auth_identifier(&self) -> String {
        self.id.clone()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Knows one user: id `"7"`, email `"a@b.com"`, password `"secret"`.
struct FakeProvider;

fn the_user() -> Arc<dyn Authenticatable> {
    Arc::new(TestUser { id: "7".into() })
}

#[async_trait::async_trait]
impl UserProvider for FakeProvider {
    async fn retrieve_by_id(
        &self,
        id: &str,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        Ok((id == "7").then(the_user))
    }

    async fn retrieve_by_credentials(
        &self,
        credentials: &serde_json::Value,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        let email = credentials.get("email").and_then(|v| v.as_str());
        Ok((email == Some("a@b.com")).then(the_user))
    }

    async fn validate_credentials(
        &self,
        _user: &dyn Authenticatable,
        credentials: &serde_json::Value,
    ) -> Result<bool, FrameworkError> {
        Ok(credentials.get("password").and_then(|v| v.as_str()) == Some("secret"))
    }
}

fn guard() -> SessionGuard {
    SessionGuard::new(Arc::new(FakeProvider))
}

/// Drive `fut` inside the three task-local scopes a real request installs:
/// the session, the pending-cookies bag, and the auth request state.
async fn run_in_request<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let session_slot = suprnova::session::new_session_slot_for_test();
    let pending_slot = suprnova::session::new_pending_cookies_slot_for_test();
    suprnova::session::session_scope_for_test(
        session_slot,
        suprnova::session::pending_cookies_scope_for_test(
            pending_slot,
            request_state::request_state_scope_for_test(fut),
        ),
    )
    .await
}

#[test]
fn attempt_with_valid_credentials_dispatches_attempting_login_authenticated() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        run_in_request(async {
            let g = guard();
            let user = g
                .attempt(&Credentials::password("a@b.com", "secret"), false)
                .await
                .unwrap();
            assert_eq!(user.map(|u| u.get_auth_identifier()), Some("7".to_string()));
        })
        .await;

        assert_dispatched::<Attempting>(|e| e.guard == "web" && !e.remember);
        assert_dispatched::<Login>(|e| e.guard == "web" && e.user_id == "7" && !e.remember);
        assert_dispatched::<Authenticated>(|e| e.guard == "web" && e.user_id == "7");
        assert_not_dispatched::<Failed>(|_| true);
    });
}

#[test]
fn attempt_with_wrong_password_dispatches_failed_with_user_id() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        run_in_request(async {
            let g = guard();
            let user = g
                .attempt(&Credentials::password("a@b.com", "wrong"), false)
                .await
                .unwrap();
            assert!(user.is_none());
        })
        .await;

        assert_dispatched::<Attempting>(|_| true);
        // Identifier matched but the password did not → Failed carries the id.
        assert_dispatched::<Failed>(|e| e.guard == "web" && e.user_id.as_deref() == Some("7"));
        assert_not_dispatched::<Login>(|_| true);
        assert_not_dispatched::<Authenticated>(|_| true);
    });
}

#[test]
fn attempt_with_unknown_user_dispatches_failed_without_user_id() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        run_in_request(async {
            let g = guard();
            let user = g
                .attempt(&Credentials::password("nobody@b.com", "secret"), false)
                .await
                .unwrap();
            assert!(user.is_none());
        })
        .await;

        assert_dispatched::<Attempting>(|_| true);
        // No user matched → Failed carries no id.
        assert_dispatched::<Failed>(|e| e.guard == "web" && e.user_id.is_none());
        assert_not_dispatched::<Login>(|_| true);
    });
}

#[test]
fn once_dispatches_attempting_and_authenticated_but_not_login() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        run_in_request(async {
            let g = guard();
            assert!(
                g.once(&Credentials::password("a@b.com", "secret"))
                    .await
                    .unwrap()
            );
        })
        .await;

        assert_dispatched::<Attempting>(|_| true);
        assert_dispatched::<Authenticated>(|e| e.user_id == "7");
        // `once` does not persist, so it is not a Login.
        assert_not_dispatched::<Login>(|_| true);
        assert_not_dispatched::<Failed>(|_| true);
    });
}

#[test]
fn login_then_logout_dispatches_login_and_logout_with_user_id() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        run_in_request(async {
            let g = guard();
            g.login(the_user(), false).await.unwrap();
            // logout revokes remember-me tokens (DB) then clears the session.
            g.logout().await.unwrap();
        })
        .await;

        assert_dispatched::<Login>(|e| e.user_id == "7");
        assert_dispatched::<Logout>(|e| e.guard == "web" && e.user_id.as_deref() == Some("7"));
    });
}

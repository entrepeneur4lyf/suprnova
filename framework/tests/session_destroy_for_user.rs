//! Phase 11 R1 — verify `SessionStore::destroy_for_user` revokes every
//! session row for a given user id, leaving rows belonging to other
//! users untouched. The wire-up from `PasswordReset::complete` →
//! `session::destroy_all_for_user` → `DatabaseSessionDriver::destroy_for_user`
//! is then trusted (single-line orchestration), and the surface contract
//! is exercised here.
//!
//! Boots a fresh in-memory SQLite via `TestDatabase::fresh::<M>` with a
//! migrator that ships the framework session schema inline. Mirrors the
//! app-level `m20251208_220000_create_sessions_table` migration so the
//! framework test doesn't need the example-app crate's migration
//! registry.

use sea_orm_migration::MigrationName;
use sea_orm_migration::prelude::*;
use std::time::Duration;
use suprnova::session::{DatabaseSessionDriver, SessionData, SessionStore};
use suprnova::testing::TestDatabase;

/// Migrator containing just the sessions table — matches the schema
/// the example app installs in production via
/// `app/src/migrations/m20251208_220000_create_sessions_table.rs`.
struct TestMigrator;

#[async_trait::async_trait]
impl MigratorTrait for TestMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(SessionsMigration)]
    }
}

struct SessionsMigration;

impl MigrationName for SessionsMigration {
    fn name(&self) -> &str {
        "m20251208_220000_create_sessions_table"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for SessionsMigration {
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

#[tokio::test]
async fn destroy_for_user_removes_only_that_users_rows() {
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let driver = DatabaseSessionDriver::new(Duration::from_secs(3600));

    // Two sessions for alice, one for bob.
    let mut alice1 = SessionData::new("alice-sess-1".into(), "csrf1".into());
    alice1.user_id = Some("alice-uid".into());
    driver.write(&alice1).await.unwrap();

    let mut alice2 = SessionData::new("alice-sess-2".into(), "csrf2".into());
    alice2.user_id = Some("alice-uid".into());
    driver.write(&alice2).await.unwrap();

    let mut bob1 = SessionData::new("bob-sess-1".into(), "csrf3".into());
    bob1.user_id = Some("bob-uid".into());
    driver.write(&bob1).await.unwrap();

    // Preconditions: all three readable.
    assert!(driver.read("alice-sess-1").await.unwrap().is_some());
    assert!(driver.read("alice-sess-2").await.unwrap().is_some());
    assert!(driver.read("bob-sess-1").await.unwrap().is_some());

    // Destroy alice's sessions.
    let deleted = driver.destroy_for_user("alice-uid").await.unwrap();
    assert_eq!(deleted, 2, "destroy_for_user must return the row count");

    // Both alice rows gone, bob untouched.
    assert!(
        driver.read("alice-sess-1").await.unwrap().is_none(),
        "alice's first session must be revoked"
    );
    assert!(
        driver.read("alice-sess-2").await.unwrap().is_none(),
        "alice's second session must be revoked"
    );
    assert!(
        driver.read("bob-sess-1").await.unwrap().is_some(),
        "bob's session must not be touched when revoking alice"
    );
}

#[tokio::test]
async fn destroy_for_user_returns_zero_when_no_matching_rows() {
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let driver = DatabaseSessionDriver::new(Duration::from_secs(3600));
    let deleted = driver.destroy_for_user("ghost-uid").await.unwrap();
    assert_eq!(deleted, 0);
}

#[tokio::test]
async fn module_helper_destroy_all_for_user_delegates_to_driver() {
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();
    let driver = DatabaseSessionDriver::new(Duration::from_secs(3600));

    let mut sess = SessionData::new("helper-sess".into(), "csrf".into());
    sess.user_id = Some("helper-uid".into());
    driver.write(&sess).await.unwrap();

    let deleted = suprnova::session::destroy_all_for_user("helper-uid")
        .await
        .unwrap();
    assert_eq!(deleted, 1);
    assert!(driver.read("helper-sess").await.unwrap().is_none());
}

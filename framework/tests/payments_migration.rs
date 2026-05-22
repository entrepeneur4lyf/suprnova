//! Phase 12 T5 — payments DB mirror migration integration test.
//!
//! Boots a fresh in-memory SQLite via `TestDatabase::fresh::<PaymentsTestMigrator>()`
//! and confirms all six payments tables exist and are queryable.

use sea_orm::ConnectionTrait;
use sea_orm_migration::MigratorTrait;
use suprnova::payments::migrations::migrations as payments_migrations;
use suprnova::testing::TestDatabase;

struct PaymentsTestMigrator;

#[async_trait::async_trait]
impl MigratorTrait for PaymentsTestMigrator {
    fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        payments_migrations()
    }
}

#[tokio::test]
async fn payments_migration_up_creates_all_six_tables() {
    let db = TestDatabase::fresh::<PaymentsTestMigrator>().await.unwrap();
    let conn = db.conn();

    for table in [
        "payments_customers",
        "payments_payment_methods",
        "payments_subscriptions",
        "payments_subscription_items",
        "payments_transactions",
        "payments_webhook_events",
    ] {
        let stmt = sea_orm::Statement::from_string(
            conn.get_database_backend(),
            format!("SELECT COUNT(*) FROM {table}"),
        );
        let res = conn.query_one(stmt).await;
        assert!(res.is_ok(), "table {table} should exist and be queryable");
    }
}

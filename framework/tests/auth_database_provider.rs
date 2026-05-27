//! Integration tests for [`suprnova::DatabaseUserProvider`] against a
//! real `users` table in in-memory SQLite.
//!
//! Follows the `tests/remember_me.rs` setup pattern: a shared runtime
//! (sqlx pools die with their runtime), a process-global connection
//! registered in `App`, and a local migrator for just the table under
//! test.

use once_cell::sync::Lazy;
use sea_orm::{ActiveModelTrait, Set};
use sea_orm_migration::MigratorTrait;
use sea_orm_migration::prelude::*;
use tokio::runtime::Runtime;

use suprnova::{Credentials, DatabaseUserProvider, UserProvider};

static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("tokio runtime"));

/// One-shot: register a shared in-memory SQLite connection, migrate a
/// `users` table, and seed one user (`a@b.com` / `secret`, not admin).
static SETUP: Lazy<()> = Lazy::new(|| {
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

        // Seed: id 1, a@b.com, bcrypt("secret"), not admin.
        let hash = suprnova::hash("secret").expect("hash password");
        let conn = suprnova::DB::connection().expect("db connection");
        users::ActiveModel {
            email: Set("a@b.com".to_string()),
            password: Set(hash),
            is_admin: Set(false),
            ..Default::default()
        }
        .insert(conn.inner())
        .await
        .expect("seed user");
    });
});

struct LocalMigrator;

#[async_trait::async_trait]
impl MigratorTrait for LocalMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(CreateUsersTable)]
    }
}

struct CreateUsersTable;

impl MigrationName for CreateUsersTable {
    fn name(&self) -> &str {
        "m20240101_000001_create_users_table"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateUsersTable {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Users::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Users::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Users::Email).string().not_null())
                    .col(ColumnDef::new(Users::Password).string().not_null())
                    .col(ColumnDef::new(Users::IsAdmin).boolean().not_null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Users::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
    Email,
    Password,
    IsAdmin,
}

/// SeaORM entity for seeding.
mod users {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub email: String,
        pub password: String,
        pub is_admin: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

fn provider() -> DatabaseUserProvider {
    DatabaseUserProvider::new("users")
}

#[test]
fn retrieve_by_id_resolves_known_and_unknown() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let p = provider();
        let user = p.retrieve_by_id("1").await.unwrap().expect("user 1 exists");
        assert_eq!(user.get_auth_identifier(), "1");
        assert!(user.get_auth_password().is_some());

        assert!(p.retrieve_by_id("999").await.unwrap().is_none());
    });
}

#[test]
fn retrieve_by_credentials_matches_on_email() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let p = provider();
        let found = p
            .retrieve_by_credentials(&Credentials::password("a@b.com", "ignored").as_value())
            .await
            .unwrap();
        assert_eq!(
            found.map(|u| u.get_auth_identifier()),
            Some("1".to_string())
        );

        let missing = p
            .retrieve_by_credentials(&Credentials::password("nobody@b.com", "x").as_value())
            .await
            .unwrap();
        assert!(missing.is_none());
    });
}

#[test]
fn validate_credentials_checks_the_password_hash() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let p = provider();
        let user = p.retrieve_by_id("1").await.unwrap().unwrap();

        assert!(
            p.validate_credentials(
                &*user,
                &Credentials::password("a@b.com", "secret").as_value()
            )
            .await
            .unwrap()
        );
        assert!(
            !p.validate_credentials(
                &*user,
                &Credentials::password("a@b.com", "wrong").as_value()
            )
            .await
            .unwrap()
        );
    });
}

// Non-allowlisted credential keys must never become WHERE predicates: a
// hostile `{email, is_admin: true}` (the seeded user is NOT admin) still
// resolves by email alone. If `is_admin` leaked into the query, the
// lookup would filter `is_admin = true` and find nobody.
#[test]
fn credential_allowlist_ignores_non_allowlisted_keys() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let p = provider();
        let creds = Credentials::new()
            .insert("email", "a@b.com")
            .insert("is_admin", true)
            .as_value();
        let found = p.retrieve_by_credentials(&creds).await.unwrap();
        assert_eq!(
            found.map(|u| u.get_auth_identifier()),
            Some("1".to_string()),
            "is_admin must be ignored; lookup filters on email only"
        );
    });
}

// A credential map with no allowlisted key must not match the first row.
#[test]
fn retrieve_by_credentials_with_no_allowlisted_key_returns_none() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let p = provider();
        let creds = Credentials::new().insert("is_admin", true).as_value();
        assert!(p.retrieve_by_credentials(&creds).await.unwrap().is_none());
    });
}

// A custom credential allowlist enables login by an alternate column.
#[test]
fn custom_credential_columns_allow_alternate_lookup() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        // "id" is not normally a credential column; allow it explicitly.
        let p = DatabaseUserProvider::new("users").credential_columns(["email", "id"]);
        let creds = Credentials::new().insert("id", 1).as_value();
        let found = p.retrieve_by_credentials(&creds).await.unwrap();
        assert_eq!(
            found.map(|u| u.get_auth_identifier()),
            Some("1".to_string())
        );
    });
}

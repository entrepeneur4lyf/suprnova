//! User model.
//!
//! Real SeaORM entity backed by the `users` migration shipped with this
//! starter. The `id` and `email` columns mirror the migration; add more
//! fields by extending both the migration and the `Model` struct, then
//! re-running `suprnova migrate`.
//!
//! Authentication credentials live in Torii's own storage (see
//! `src/bootstrap.rs`); this table holds the application's view of
//! users — profiles, preferences, anything you'd join against in
//! ordinary queries.

use sea_orm::entity::prelude::*;
use sea_orm::{ActiveModelTrait, EntityTrait, QueryOrder, Set};
use serde::{Deserialize, Serialize};
use suprnova::{FrameworkError, DB};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub email: String,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Type alias matching the `--api` starter's controller imports.
pub type User = Model;

impl Model {
    /// Look up a single user by primary key.
    ///
    /// Returns `Ok(None)` when the row is not present rather than treating
    /// that as an error — the caller decides whether to surface a 404.
    pub async fn find_by_id(id: i64) -> Result<Option<Self>, FrameworkError> {
        let conn = DB::connection()?;
        Entity::find_by_id(id)
            .one(conn.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))
    }

    /// Look up a single user by their email address.
    ///
    /// Useful for login flows that translate a user-facing identifier
    /// into the persisted record.
    pub async fn find_by_email(email: &str) -> Result<Option<Self>, FrameworkError> {
        let conn = DB::connection()?;
        Entity::find()
            .filter(Column::Email.eq(email))
            .one(conn.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))
    }

    /// Return every user ordered by `id` ascending.
    ///
    /// Suitable for small admin endpoints; switch to the framework's
    /// pagination helpers (`Paginated`, `CursorPaginator`) once the
    /// table outgrows a single page.
    pub async fn all() -> Result<Vec<Self>, FrameworkError> {
        let conn = DB::connection()?;
        Entity::find()
            .order_by_asc(Column::Id)
            .all(conn.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))
    }

    /// Insert a user row keyed by email.
    ///
    /// Application code typically reaches this after `Auth::password()`
    /// has registered the credentials in Torii's own store; this row
    /// is the app-table view of that user that controllers join against.
    pub async fn create(email: impl Into<String>) -> Result<Self, FrameworkError> {
        let conn = DB::connection()?;
        let active = ActiveModel {
            email: Set(email.into()),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        };
        active
            .insert(conn.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))
    }
}

//! User model
//!
//! This file contains custom implementations for the User model.
//! The base entity is auto-generated in src/models/entities/users.rs
//!
//! This file is NEVER overwritten by `suprnova db:sync` - your custom code is safe here.

// Re-export the auto-generated entity
pub use super::entities::users::*;

use suprnova::database::{ModelMut, QueryBuilder};
use suprnova::Authenticatable;
use sea_orm::{entity::prelude::*, Set};
use std::any::Any;

/// Type alias for convenient access
pub type User = Model;

// ============================================================================
// ENTITY CONFIGURATION
// ============================================================================

impl ActiveModelBehavior for ActiveModel {}

impl suprnova::database::Model for Entity {}
impl suprnova::database::ModelMut for Entity {}

// ============================================================================
// ELOQUENT-LIKE API
// Fluent query builder and setter methods for User
// ============================================================================

impl Model {
    /// Start a new query builder
    ///
    /// # Example
    /// ```rust,ignore
    /// let records = User::query().all().await?;
    /// let record = User::query().filter(Column::Id.eq(1)).first().await?;
    /// ```
    pub fn query() -> QueryBuilder<Entity> {
        QueryBuilder::new()
    }

    /// Create a new record builder
    ///
    /// # Example
    /// ```rust,ignore
    /// let record = User::create()
    ///     .set_field("value")
    ///     .insert()
    ///     .await?;
    /// ```
    pub fn create() -> UserBuilder {
        UserBuilder::default()
    }


    /// Save changes to the database
    ///
    /// # Example
    /// ```rust,ignore
    /// let updated = record.set_field("new_value").update().await?;
    /// ```
    pub async fn update(self) -> Result<Self, suprnova::FrameworkError> {
        let active = self.to_active_model();
        Entity::update_one(active).await
    }

    /// Delete this record from the database
    ///
    /// # Example
    /// ```rust,ignore
    /// record.delete().await?;
    /// ```
    pub async fn delete(self) -> Result<u64, suprnova::FrameworkError> {
        Entity::delete_by_pk(self.id).await
    }

    fn to_active_model(&self) -> ActiveModel {
        ActiveModel {
            id: Set(self.id),
            created_at: Set(self.created_at.clone()),
            updated_at: Set(self.updated_at.clone()),
        }
    }
}

// ============================================================================
// BUILDER
// For creating new records with fluent setter pattern
// ============================================================================

/// Builder for creating new User records
#[derive(Default)]
pub struct UserBuilder {
    created_at: Option<String>,
    updated_at: Option<String>,
}

impl UserBuilder {

    /// Insert the record into the database
    ///
    /// # Example
    /// ```rust,ignore
    /// let record = User::create()
    ///     .set_field("value")
    ///     .insert()
    ///     .await?;
    /// ```
    pub async fn insert(self) -> Result<Model, suprnova::FrameworkError> {
        let active = self.build();
        Entity::insert_one(active).await
    }

    fn build(self) -> ActiveModel {
        ActiveModel {
            id: sea_orm::ActiveValue::NotSet,
            created_at: self.created_at.map(Set).unwrap_or(sea_orm::ActiveValue::NotSet),
            updated_at: self.updated_at.map(Set).unwrap_or(sea_orm::ActiveValue::NotSet),
        }
    }
}

// ============================================================================
// CUSTOM METHODS
// Add your custom query and mutation methods below
// ============================================================================

impl Model {
    /// Look up a user by primary key.
    ///
    /// Thin wrapper around the query builder.
    pub async fn find_by_id(id: i32) -> Result<Option<Self>, suprnova::FrameworkError> {
        Self::query().filter(Column::Id.eq(id)).first().await
    }

    /// Return all users, ordered by id ascending.
    pub async fn find_all() -> Result<Vec<Self>, suprnova::FrameworkError> {
        Self::query().all().await
    }

    /// Whether this user holds admin privileges.
    ///
    /// The dogfood entity has no `is_admin` column — this always returns
    /// `false`. A real app would persist this flag and include it in the
    /// migration.
    pub fn is_admin(&self) -> bool {
        false
    }
}

// ============================================================================
// RELATIONS
// Define relationships to other entities here
// ============================================================================

// Example: One-to-Many relation
// impl Entity {
//     pub fn has_many_posts() -> RelationDef {
//         Entity::has_many(super::posts::Entity).into()
//     }
// }

// Example: Belongs-To relation
// impl Entity {
//     pub fn belongs_to_user() -> RelationDef {
//         Entity::belongs_to(super::users::Entity)
//             .from(Column::UserId)
//             .to(super::users::Column::Id)
//             .into()
//     }
// }

// ============================================================================
// AUTHENTICATION
// Implements the Authenticatable trait for Auth::user() support
// ============================================================================

impl Authenticatable for Model {
    fn auth_identifier(&self) -> i64 {
        self.id as i64
    }

    fn auth_identifier_name(&self) -> &'static str {
        "id"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

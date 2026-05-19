//! Todo model
//!
//! This file contains custom implementations for the Todo model.
//! The base entity is auto-generated in src/models/entities/todos.rs
//!
//! This file is NEVER overwritten by `suprnova db:sync` - your custom code is safe here.

// Re-export the auto-generated entity
pub use super::entities::todos::*;

use suprnova::database::{EntityExtMut, QueryBuilder};
use sea_orm::{entity::prelude::*, Set};

/// Type alias for convenient access
pub type Todo = Model;

// ============================================================================
// ENTITY CONFIGURATION
// ============================================================================

impl ActiveModelBehavior for ActiveModel {}

impl suprnova::database::EntityExt for Entity {}
impl suprnova::database::EntityExtMut for Entity {}

// ============================================================================
// ELOQUENT-LIKE API
// Fluent query builder and setter methods for Todo
// ============================================================================

impl Model {
    /// Start a new query builder
    ///
    /// # Example
    /// ```rust,ignore
    /// let records = Todo::query().all().await?;
    /// let record = Todo::query().filter(Column::Id.eq(1)).first().await?;
    /// ```
    pub fn query() -> QueryBuilder<Entity> {
        QueryBuilder::new()
    }

    /// Create a new record builder
    ///
    /// # Example
    /// ```rust,ignore
    /// let record = Todo::create()
    ///     .set_field("value")
    ///     .insert()
    ///     .await?;
    /// ```
    pub fn create() -> TodoBuilder {
        TodoBuilder::default()
    }

    /// Set the title field
    pub fn set_title(mut self, value: impl Into<String>) -> Self {
        self.title = value.into();
        self
    }

    /// Set the description field
    pub fn set_description(mut self, value: Option<impl Into<String>>) -> Self {
        self.description = value.map(|v| v.into());
        self
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
            title: Set(self.title.clone()),
            description: Set(self.description.clone()),
            created_at: Set(self.created_at.clone()),
            updated_at: Set(self.updated_at.clone()),
        }
    }
}

// ============================================================================
// BUILDER
// For creating new records with fluent setter pattern
// ============================================================================

/// Builder for creating new Todo records
#[derive(Default)]
pub struct TodoBuilder {
    title: Option<String>,
    description: Option<Option<String>>,
    created_at: Option<String>,
    updated_at: Option<String>,
}

impl TodoBuilder {
    /// Set the title field
    pub fn set_title(mut self, value: impl Into<String>) -> Self {
        self.title = Some(value.into());
        self
    }

    /// Set the description field
    pub fn set_description(mut self, value: impl Into<String>) -> Self {
        self.description = Some(Some(value.into()));
        self
    }


    /// Insert the record into the database
    ///
    /// # Example
    /// ```rust,ignore
    /// let record = Todo::create()
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
            title: self.title.map(Set).unwrap_or(sea_orm::ActiveValue::NotSet),
            description: self.description.map(Set).unwrap_or(sea_orm::ActiveValue::NotSet),
            created_at: self.created_at.map(Set).unwrap_or(sea_orm::ActiveValue::NotSet),
            updated_at: self.updated_at.map(Set).unwrap_or(sea_orm::ActiveValue::NotSet),
        }
    }
}

// ============================================================================
// CUSTOM METHODS
// Add your custom query and mutation methods below
// ============================================================================

// Example custom finder:
// impl Model {
//     pub async fn find_by_email(email: &str) -> Result<Option<Self>, suprnova::FrameworkError> {
//         Self::query().filter(Column::Email.eq(email)).first().await
//     }
// }

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

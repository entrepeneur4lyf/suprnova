//! User model

use suprnova::database::{Model as DatabaseModel, ModelMut, QueryBuilder};
use sea_orm::entity::prelude::*;
use sea_orm::Set;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub name: String,
    pub email: String,
    #[serde(skip_serializing)]
    pub password: String,
    #[serde(skip_serializing)]
    pub remember_token: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl DatabaseModel for Entity {}
impl ModelMut for Entity {}

/// Type alias for convenient access
pub type User = Model;

impl Model {
    /// Start a query builder
    pub fn query() -> QueryBuilder<Entity> {
        QueryBuilder::new()
    }

    /// Find a user by their email address
    pub async fn find_by_email(email: &str) -> Result<Option<Self>, suprnova::FrameworkError> {
        Self::query()
            .filter(Column::Email.eq(email))
            .first()
            .await
    }

    /// Verify the user's password
    pub fn verify_password(&self, password: &str) -> Result<bool, suprnova::FrameworkError> {
        suprnova::hashing::verify(password, &self.password)
    }

    /// Create a new user with a hashed password
    pub async fn create(
        name: impl Into<String>,
        email: impl Into<String>,
        password: &str,
    ) -> Result<Self, suprnova::FrameworkError> {
        let hashed = suprnova::hashing::hash(password)?;

        let model = ActiveModel {
            name: Set(name.into()),
            email: Set(email.into()),
            password: Set(hashed),
            ..Default::default()
        };

        Entity::insert_one(model).await
    }

    /// Update the user's remember token
    pub async fn update_remember_token(
        &self,
        token: Option<String>,
    ) -> Result<(), suprnova::FrameworkError> {
        let mut active: ActiveModel = self.clone().into();
        active.remember_token = Set(token);
        Entity::update_one(active).await?;
        Ok(())
    }
}

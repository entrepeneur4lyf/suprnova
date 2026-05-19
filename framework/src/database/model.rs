//! Entity-trait extensions (legacy surface — superseded by the Eloquent `Model` trait in `eloquent::Model`).
//!
//! Provides Laravel-like active record convenience methods over SeaORM
//! entities via two extension traits — [`EntityExt`] (read) and
//! [`EntityExtMut`] (write). These are blanket-friendly add-ons on
//! `EntityTrait`; the new full Eloquent `Model` trait shipped in Phase 10A
//! is the modern surface and reserves the bare `Model` name.

use async_trait::async_trait;
use sea_orm::{
    ActiveModelBehavior, ActiveModelTrait, EntityTrait, IntoActiveModel, ModelTrait,
    PaginatorTrait, PrimaryKeyTrait, TryIntoModel,
};

use crate::database::DB;
use crate::error::FrameworkError;

/// Trait providing Laravel-like read operations on SeaORM entities
///
/// Implement this trait on your SeaORM Entity to get convenient static methods
/// for querying records.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::database::EntityExt;
/// use sea_orm::entity::prelude::*;
///
/// #[derive(Clone, Debug, DeriveEntityModel)]
/// #[sea_orm(table_name = "users")]
/// pub struct Model {
///     #[sea_orm(primary_key)]
///     pub id: i32,
///     pub name: String,
///     pub email: String,
/// }
///
/// #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
/// pub enum Relation {}
///
/// impl ActiveModelBehavior for ActiveModel {}
///
/// // Add suprnova's EntityExt trait
/// impl suprnova::database::EntityExt for Entity {}
///
/// // Now you can use:
/// let users = Entity::all().await?;
/// let user = Entity::find_by_pk(1).await?;
/// ```
#[async_trait]
pub trait EntityExt: EntityTrait + Sized
where
    Self::Model: ModelTrait<Entity = Self> + Send + Sync,
{
    /// Find all records
    ///
    /// # Example
    /// ```rust,ignore
    /// let users = user::Entity::all().await?;
    /// ```
    async fn all() -> Result<Vec<Self::Model>, FrameworkError> {
        let db = DB::connection()?;
        Self::find()
            .all(db.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))
    }

    /// Find a record by primary key (generic version)
    ///
    /// # Example
    /// ```rust,ignore
    /// let user = user::Entity::find_by_pk(1).await?;
    /// ```
    async fn find_by_pk<K>(id: K) -> Result<Option<Self::Model>, FrameworkError>
    where
        K: Into<<Self::PrimaryKey as PrimaryKeyTrait>::ValueType> + Send,
    {
        let db = DB::connection()?;
        Self::find_by_id(id)
            .one(db.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))
    }

    /// Find a record by primary key or return an error
    ///
    /// # Example
    /// ```rust,ignore
    /// let user = user::Entity::find_or_fail(1).await?;
    /// ```
    async fn find_or_fail<K>(id: K) -> Result<Self::Model, FrameworkError>
    where
        K: Into<<Self::PrimaryKey as PrimaryKeyTrait>::ValueType> + Send + std::fmt::Debug + Copy,
    {
        Self::find_by_pk(id).await?.ok_or_else(|| {
            FrameworkError::database(format!(
                "{} with id {:?} not found",
                std::any::type_name::<Self>(),
                id
            ))
        })
    }

    /// Count all records
    ///
    /// # Example
    /// ```rust,ignore
    /// let count = user::Entity::count_all().await?;
    /// ```
    async fn count_all() -> Result<u64, FrameworkError> {
        let db = DB::connection()?;
        Self::find()
            .count(db.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))
    }

    /// Check if any records exist
    ///
    /// # Example
    /// ```rust,ignore
    /// if user::Entity::exists_any().await? {
    ///     println!("Users exist!");
    /// }
    /// ```
    async fn exists_any() -> Result<bool, FrameworkError> {
        Ok(Self::count_all().await? > 0)
    }

    /// Get the first record
    ///
    /// # Example
    /// ```rust,ignore
    /// let first_user = user::Entity::first().await?;
    /// ```
    async fn first() -> Result<Option<Self::Model>, FrameworkError> {
        let db = DB::connection()?;
        Self::find()
            .one(db.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))
    }
}

/// Trait providing Laravel-like write operations on SeaORM entities
///
/// Implement this trait alongside `EntityExt` to get insert/update/delete methods.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::database::{EntityExt, EntityExtMut};
/// use sea_orm::Set;
///
/// // Implement both traits
/// impl suprnova::database::EntityExt for Entity {}
/// impl suprnova::database::EntityExtMut for Entity {}
///
/// // Insert a new record
/// let new_user = user::ActiveModel {
///     name: Set("John".to_string()),
///     email: Set("john@example.com".to_string()),
///     ..Default::default()
/// };
/// let user = user::Entity::insert_one(new_user).await?;
///
/// // Delete by ID
/// user::Entity::delete_by_pk(user.id).await?;
/// ```
#[async_trait]
pub trait EntityExtMut: EntityExt
where
    Self::Model: ModelTrait<Entity = Self> + IntoActiveModel<Self::ActiveModel> + Send + Sync,
    Self::ActiveModel: ActiveModelTrait<Entity = Self> + ActiveModelBehavior + Send,
{
    /// Insert a new record
    ///
    /// # Example
    /// ```rust,ignore
    /// let new_user = user::ActiveModel {
    ///     name: Set("John".to_string()),
    ///     email: Set("john@example.com".to_string()),
    ///     ..Default::default()
    /// };
    /// let user = user::Entity::insert_one(new_user).await?;
    /// ```
    async fn insert_one(model: Self::ActiveModel) -> Result<Self::Model, FrameworkError> {
        let db = DB::connection()?;
        model
            .insert(db.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))
    }

    /// Update an existing record
    ///
    /// # Example
    /// ```rust,ignore
    /// let mut user: user::ActiveModel = user.into();
    /// user.name = Set("Updated Name".to_string());
    /// let updated = user::Entity::update_one(user).await?;
    /// ```
    async fn update_one(model: Self::ActiveModel) -> Result<Self::Model, FrameworkError> {
        let db = DB::connection()?;
        model
            .update(db.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))
    }

    /// Delete a record by primary key
    ///
    /// # Example
    /// ```rust,ignore
    /// let rows_deleted = user::Entity::delete_by_pk(1).await?;
    /// ```
    async fn delete_by_pk<K>(id: K) -> Result<u64, FrameworkError>
    where
        K: Into<<Self::PrimaryKey as PrimaryKeyTrait>::ValueType> + Send,
    {
        let db = DB::connection()?;
        let result = Self::delete_by_id(id)
            .exec(db.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(result.rows_affected)
    }

    /// Save a model (insert or update based on whether primary key is set)
    ///
    /// # Example
    /// ```rust,ignore
    /// let user = user::ActiveModel {
    ///     name: Set("John".to_string()),
    ///     ..Default::default()
    /// };
    /// let saved = user::Entity::save_one(user).await?;
    /// ```
    async fn save_one(model: Self::ActiveModel) -> Result<Self::Model, FrameworkError>
    where
        Self::ActiveModel: TryIntoModel<Self::Model>,
    {
        let db = DB::connection()?;
        let saved = model
            .save(db.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        saved
            .try_into_model()
            .map_err(|e| FrameworkError::database(e.to_string()))
    }
}

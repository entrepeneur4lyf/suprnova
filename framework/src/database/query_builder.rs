//! Fluent query builder for Eloquent-like API
//!
//! Provides a chainable query interface that uses the global DB connection.
//!
//! # Example
//!
//! ```rust,ignore
//! use crate::models::todos::{Todo, Column};
//!
//! // Simple query
//! let todos = Todo::query().all().await?;
//!
//! // With filters
//! let todo = Todo::query()
//!     .filter(Column::Title.eq("test"))
//!     .filter(Column::Id.gt(5))
//!     .first()
//!     .await?;
//!
//! // With ordering and pagination
//! let todos = Todo::query()
//!     .order_by_desc(Column::CreatedAt)
//!     .limit(10)
//!     .offset(20)
//!     .all()
//!     .await?;
//! ```

use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect, Select};

// `DB::connection()` is no longer called directly here — Phase 10C T11
// routes through `ExecutorChoice::resolve()` so the same code path
// honours an active `DB::transaction` scope without explicit threading.
use crate::error::FrameworkError;

/// Fluent query builder wrapper
///
/// Wraps SeaORM's `Select` with methods that use the global DB connection.
/// This provides an Eloquent-like query API.
///
/// # Example
///
/// ```rust,ignore
/// let todos = Todo::query()
///     .filter(Column::Active.eq(true))
///     .order_by_asc(Column::Title)
///     .all()
///     .await?;
/// ```
pub struct QueryBuilder<E>
where
    E: EntityTrait,
{
    select: Select<E>,
}

impl<E> QueryBuilder<E>
where
    E: EntityTrait,
    E::Model: Send + Sync,
{
    /// Create a new query builder for the entity
    pub fn new() -> Self {
        Self { select: E::find() }
    }

    /// Add a filter condition
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let todos = Todo::query()
    ///     .filter(Column::Title.eq("test"))
    ///     .filter(Column::Active.eq(true))
    ///     .all()
    ///     .await?;
    /// ```
    pub fn filter<F>(mut self, filter: F) -> Self
    where
        F: sea_orm::sea_query::IntoCondition,
    {
        self.select = self.select.filter(filter);
        self
    }

    /// Add an order by clause (ascending)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let todos = Todo::query()
    ///     .order_by_asc(Column::Title)
    ///     .all()
    ///     .await?;
    /// ```
    pub fn order_by_asc<C>(mut self, col: C) -> Self
    where
        C: ColumnTrait,
    {
        self.select = self.select.order_by(col, Order::Asc);
        self
    }

    /// Add an order by clause (descending)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let todos = Todo::query()
    ///     .order_by_desc(Column::CreatedAt)
    ///     .all()
    ///     .await?;
    /// ```
    pub fn order_by_desc<C>(mut self, col: C) -> Self
    where
        C: ColumnTrait,
    {
        self.select = self.select.order_by(col, Order::Desc);
        self
    }

    /// Add an order by clause with custom order
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use sea_orm::Order;
    /// let todos = Todo::query()
    ///     .order_by(Column::Title, Order::Asc)
    ///     .all()
    ///     .await?;
    /// ```
    pub fn order_by<C>(mut self, col: C, order: Order) -> Self
    where
        C: ColumnTrait,
    {
        self.select = self.select.order_by(col, order);
        self
    }

    /// Limit the number of results
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let todos = Todo::query().limit(10).all().await?;
    /// ```
    pub fn limit(mut self, limit: u64) -> Self {
        self.select = self.select.limit(limit);
        self
    }

    /// Skip a number of results (offset)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Skip first 10, get next 10
    /// let todos = Todo::query().offset(10).limit(10).all().await?;
    /// ```
    pub fn offset(mut self, offset: u64) -> Self {
        self.select = self.select.offset(offset);
        self
    }

    /// Execute query and return all results
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let todos = Todo::query().all().await?;
    /// ```
    pub async fn all(self) -> Result<Vec<E::Model>, FrameworkError> {
        let exec =
            crate::database::transaction::ExecutorChoice::resolve_read(None, None, None).await?;
        exec.select_all(self.select)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))
    }

    /// Execute query and return first result
    ///
    /// Returns `None` if no record matches.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let todo = Todo::query()
    ///     .filter(Column::Id.eq(1))
    ///     .first()
    ///     .await?;
    /// ```
    pub async fn first(self) -> Result<Option<E::Model>, FrameworkError> {
        let exec =
            crate::database::transaction::ExecutorChoice::resolve_read(None, None, None).await?;
        exec.select_one(self.select)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))
    }

    /// Execute query and return first result or error
    ///
    /// Returns an error if no record matches.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let todo = Todo::query()
    ///     .filter(Column::Id.eq(1))
    ///     .first_or_fail()
    ///     .await?;
    /// ```
    pub async fn first_or_fail(self) -> Result<E::Model, FrameworkError> {
        self.first().await?.ok_or_else(|| {
            FrameworkError::database(format!("{} not found", std::any::type_name::<E::Model>()))
        })
    }

    /// Count matching records
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let count = Todo::query()
    ///     .filter(Column::Active.eq(true))
    ///     .count()
    ///     .await?;
    /// ```
    pub async fn count(self) -> Result<u64, FrameworkError> {
        let exec =
            crate::database::transaction::ExecutorChoice::resolve_read(None, None, None).await?;
        exec.select_count(self.select)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))
    }

    /// Check if any records exist matching the query
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let has_active = Todo::query()
    ///     .filter(Column::Active.eq(true))
    ///     .exists()
    ///     .await?;
    /// ```
    pub async fn exists(self) -> Result<bool, FrameworkError> {
        Ok(self.count().await? > 0)
    }

    /// Get access to the underlying SeaORM Select for advanced queries
    ///
    /// Use this when you need SeaORM features not exposed by QueryBuilder.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let select = Todo::query()
    ///     .filter(Column::Active.eq(true))
    ///     .into_select();
    ///
    /// // Use with SeaORM directly
    /// let todos = select.all(db.inner()).await?;
    /// ```
    pub fn into_select(self) -> Select<E> {
        self.select
    }
}

impl<E> Default for QueryBuilder<E>
where
    E: EntityTrait,
    E::Model: Send + Sync,
{
    fn default() -> Self {
        Self::new()
    }
}

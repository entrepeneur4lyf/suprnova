//! Fluent query builder for Eloquent-like API
//!
//! Provides a chainable query interface that uses the global DB connection.
//!
//! # Example
//!
//! ```rust,no_run
//! # use suprnova::FrameworkError;
//! # #[derive(Clone, Copy)] enum Cond {}
//! # #[derive(Clone, Copy)] enum Column { Id, Title, CreatedAt }
//! # impl Column {
//! #     fn eq<T>(self, _v: T) -> Cond { unreachable!() }
//! #     fn gt<T>(self, _v: T) -> Cond { unreachable!() }
//! # }
//! # struct Model;
//! # struct Qb;
//! # impl Qb {
//! #     fn filter(self, _c: Cond) -> Self { self }
//! #     fn order_by_desc(self, _c: Column) -> Self { self }
//! #     fn limit(self, _n: u64) -> Self { self }
//! #     fn offset(self, _n: u64) -> Self { self }
//! #     async fn all(self) -> Result<Vec<Model>, FrameworkError> { Ok(vec![]) }
//! #     async fn first(self) -> Result<Option<Model>, FrameworkError> { Ok(None) }
//! # }
//! # struct Todo;
//! # impl Todo { fn query() -> Qb { Qb } }
//! # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
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
//! # Ok(()) }
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
/// ```rust,no_run
/// # use suprnova::FrameworkError;
/// # #[derive(Clone, Copy)] enum Cond {}
/// # #[derive(Clone, Copy)] enum Column { Title, Active }
/// # impl Column { fn eq<T>(self, _v: T) -> Cond { unreachable!() } }
/// # struct Model;
/// # struct Qb;
/// # impl Qb {
/// #     fn filter(self, _c: Cond) -> Self { self }
/// #     fn order_by_asc(self, _c: Column) -> Self { self }
/// #     async fn all(self) -> Result<Vec<Model>, FrameworkError> { Ok(vec![]) }
/// # }
/// # struct Todo;
/// # impl Todo { fn query() -> Qb { Qb } }
/// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
/// let todos = Todo::query()
///     .filter(Column::Active.eq(true))
///     .order_by_asc(Column::Title)
///     .all()
///     .await?;
/// # Ok(()) }
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
    /// ```rust,no_run
    /// # use suprnova::FrameworkError;
    /// # #[derive(Clone, Copy)] enum Cond {}
    /// # #[derive(Clone, Copy)] enum Column { Title, Active }
    /// # impl Column { fn eq<T>(self, _v: T) -> Cond { unreachable!() } }
    /// # struct Model;
    /// # struct Qb;
    /// # impl Qb {
    /// #     fn filter(self, _c: Cond) -> Self { self }
    /// #     async fn all(self) -> Result<Vec<Model>, FrameworkError> { Ok(vec![]) }
    /// # }
    /// # struct Todo;
    /// # impl Todo { fn query() -> Qb { Qb } }
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// let todos = Todo::query()
    ///     .filter(Column::Title.eq("test"))
    ///     .filter(Column::Active.eq(true))
    ///     .all()
    ///     .await?;
    /// # Ok(()) }
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
    /// ```rust,no_run
    /// # use suprnova::FrameworkError;
    /// # #[derive(Clone, Copy)] enum Column { Title }
    /// # struct Model;
    /// # struct Qb;
    /// # impl Qb {
    /// #     fn order_by_asc(self, _c: Column) -> Self { self }
    /// #     async fn all(self) -> Result<Vec<Model>, FrameworkError> { Ok(vec![]) }
    /// # }
    /// # struct Todo;
    /// # impl Todo { fn query() -> Qb { Qb } }
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// let todos = Todo::query()
    ///     .order_by_asc(Column::Title)
    ///     .all()
    ///     .await?;
    /// # Ok(()) }
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
    /// ```rust,no_run
    /// # use suprnova::FrameworkError;
    /// # #[derive(Clone, Copy)] enum Column { CreatedAt }
    /// # struct Model;
    /// # struct Qb;
    /// # impl Qb {
    /// #     fn order_by_desc(self, _c: Column) -> Self { self }
    /// #     async fn all(self) -> Result<Vec<Model>, FrameworkError> { Ok(vec![]) }
    /// # }
    /// # struct Todo;
    /// # impl Todo { fn query() -> Qb { Qb } }
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// let todos = Todo::query()
    ///     .order_by_desc(Column::CreatedAt)
    ///     .all()
    ///     .await?;
    /// # Ok(()) }
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
    /// ```rust,no_run
    /// # use suprnova::FrameworkError;
    /// # use sea_orm::Order;
    /// # #[derive(Clone, Copy)] enum Column { Title }
    /// # struct Model;
    /// # struct Qb;
    /// # impl Qb {
    /// #     fn order_by(self, _c: Column, _o: Order) -> Self { self }
    /// #     async fn all(self) -> Result<Vec<Model>, FrameworkError> { Ok(vec![]) }
    /// # }
    /// # struct Todo;
    /// # impl Todo { fn query() -> Qb { Qb } }
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// let todos = Todo::query()
    ///     .order_by(Column::Title, Order::Asc)
    ///     .all()
    ///     .await?;
    /// # Ok(()) }
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
    /// ```rust,no_run
    /// # use suprnova::FrameworkError;
    /// # struct Model;
    /// # struct Qb;
    /// # impl Qb {
    /// #     fn limit(self, _n: u64) -> Self { self }
    /// #     async fn all(self) -> Result<Vec<Model>, FrameworkError> { Ok(vec![]) }
    /// # }
    /// # struct Todo;
    /// # impl Todo { fn query() -> Qb { Qb } }
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// let todos = Todo::query().limit(10).all().await?;
    /// # Ok(()) }
    /// ```
    pub fn limit(mut self, limit: u64) -> Self {
        self.select = self.select.limit(limit);
        self
    }

    /// Skip a number of results (offset)
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use suprnova::FrameworkError;
    /// # struct Model;
    /// # struct Qb;
    /// # impl Qb {
    /// #     fn offset(self, _n: u64) -> Self { self }
    /// #     fn limit(self, _n: u64) -> Self { self }
    /// #     async fn all(self) -> Result<Vec<Model>, FrameworkError> { Ok(vec![]) }
    /// # }
    /// # struct Todo;
    /// # impl Todo { fn query() -> Qb { Qb } }
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// // Skip first 10, get next 10
    /// let todos = Todo::query().offset(10).limit(10).all().await?;
    /// # Ok(()) }
    /// ```
    pub fn offset(mut self, offset: u64) -> Self {
        self.select = self.select.offset(offset);
        self
    }

    /// Execute query and return all results
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use suprnova::FrameworkError;
    /// # struct Model;
    /// # struct Qb;
    /// # impl Qb { async fn all(self) -> Result<Vec<Model>, FrameworkError> { Ok(vec![]) } }
    /// # struct Todo;
    /// # impl Todo { fn query() -> Qb { Qb } }
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// let todos = Todo::query().all().await?;
    /// # Ok(()) }
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
    /// ```rust,no_run
    /// # use suprnova::FrameworkError;
    /// # #[derive(Clone, Copy)] enum Cond {}
    /// # #[derive(Clone, Copy)] enum Column { Id }
    /// # impl Column { fn eq<T>(self, _v: T) -> Cond { unreachable!() } }
    /// # struct Model;
    /// # struct Qb;
    /// # impl Qb {
    /// #     fn filter(self, _c: Cond) -> Self { self }
    /// #     async fn first(self) -> Result<Option<Model>, FrameworkError> { Ok(None) }
    /// # }
    /// # struct Todo;
    /// # impl Todo { fn query() -> Qb { Qb } }
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// let todo = Todo::query()
    ///     .filter(Column::Id.eq(1))
    ///     .first()
    ///     .await?;
    /// # Ok(()) }
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
    /// ```rust,no_run
    /// # use suprnova::FrameworkError;
    /// # #[derive(Clone, Copy)] enum Cond {}
    /// # #[derive(Clone, Copy)] enum Column { Id }
    /// # impl Column { fn eq<T>(self, _v: T) -> Cond { unreachable!() } }
    /// # struct Model;
    /// # struct Qb;
    /// # impl Qb {
    /// #     fn filter(self, _c: Cond) -> Self { self }
    /// #     async fn first_or_fail(self) -> Result<Model, FrameworkError> { Ok(Model) }
    /// # }
    /// # struct Todo;
    /// # impl Todo { fn query() -> Qb { Qb } }
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// let todo = Todo::query()
    ///     .filter(Column::Id.eq(1))
    ///     .first_or_fail()
    ///     .await?;
    /// # Ok(()) }
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
    /// ```rust,no_run
    /// # use suprnova::FrameworkError;
    /// # #[derive(Clone, Copy)] enum Cond {}
    /// # #[derive(Clone, Copy)] enum Column { Active }
    /// # impl Column { fn eq<T>(self, _v: T) -> Cond { unreachable!() } }
    /// # struct Qb;
    /// # impl Qb {
    /// #     fn filter(self, _c: Cond) -> Self { self }
    /// #     async fn count(self) -> Result<u64, FrameworkError> { Ok(0) }
    /// # }
    /// # struct Todo;
    /// # impl Todo { fn query() -> Qb { Qb } }
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// let count = Todo::query()
    ///     .filter(Column::Active.eq(true))
    ///     .count()
    ///     .await?;
    /// # Ok(()) }
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
    /// ```rust,no_run
    /// # use suprnova::FrameworkError;
    /// # #[derive(Clone, Copy)] enum Cond {}
    /// # #[derive(Clone, Copy)] enum Column { Active }
    /// # impl Column { fn eq<T>(self, _v: T) -> Cond { unreachable!() } }
    /// # struct Qb;
    /// # impl Qb {
    /// #     fn filter(self, _c: Cond) -> Self { self }
    /// #     async fn exists(self) -> Result<bool, FrameworkError> { Ok(false) }
    /// # }
    /// # struct Todo;
    /// # impl Todo { fn query() -> Qb { Qb } }
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// let has_active = Todo::query()
    ///     .filter(Column::Active.eq(true))
    ///     .exists()
    ///     .await?;
    /// # Ok(()) }
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
    /// ```rust,no_run
    /// # use suprnova::FrameworkError;
    /// # #[derive(Clone, Copy)] enum Cond {}
    /// # #[derive(Clone, Copy)] enum Column { Active }
    /// # impl Column { fn eq<T>(self, _v: T) -> Cond { unreachable!() } }
    /// # struct Model;
    /// # struct Conn;
    /// # struct Sel;
    /// # impl Sel { async fn all(self, _c: &Conn) -> Result<Vec<Model>, FrameworkError> { Ok(vec![]) } }
    /// # struct Qb;
    /// # impl Qb {
    /// #     fn filter(self, _c: Cond) -> Self { self }
    /// #     fn into_select(self) -> Sel { Sel }
    /// # }
    /// # struct Todo;
    /// # impl Todo { fn query() -> Qb { Qb } }
    /// # struct Db { conn: Conn }
    /// # impl Db { fn inner(&self) -> &Conn { &self.conn } }
    /// # async fn ex(db: &Db) -> Result<(), Box<dyn std::error::Error>> {
    /// let select = Todo::query()
    ///     .filter(Column::Active.eq(true))
    ///     .into_select();
    ///
    /// // Use with SeaORM directly
    /// let todos = select.all(db.inner()).await?;
    /// # Ok(()) }
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

//! `BelongsTo` — the inverse of `HasOne` / `HasMany`. The child row
//! carries the FK column; this relation looks up the parent.
//!
//! Mirrors Laravel's
//! [`belongsTo`](https://laravel.com/docs/12.x/eloquent-relationships#one-to-one-inverse).
//! The Suprnova surface adds a chainable `with_default(closure)`
//! exactly matching Laravel's `->withDefault(...)`: when the child's
//! FK is null OR the parent row no longer exists, `first()` returns
//! the closure's stand-in value rather than `None`. Useful for
//! template-rendering paths that would otherwise have to branch on
//! every missing parent.
//!
//! Without `with_default`, missing parents return `None`. The eager-
//! load dispatcher arm in `__eager_load` honours the same fallback —
//! both lazy and eager paths see identical behaviour.
//!
//! The FK column on the child can be nullable (`Option<i64>`). The
//! macro inspects the field type and emits `None` for the `__new`
//! arg whenever the value is `Option::None`, so the runtime path
//! never has to peek inside a `serde_json::Value` to find the null.

use std::marker::PhantomData;
use std::sync::Arc;

use crate::eloquent::model::Model;
use crate::eloquent::relations::{Relation, RelationKind};
use crate::eloquent::EloquentModel;
use crate::error::FrameworkError;

/// One-to-one inverse: child `C` carries the FK pointing at parent `P`.
///
/// Constructed by the macro-emitted relation method
/// (`fn user(&self) -> BelongsTo<Self, User>`); user code never calls
/// [`BelongsTo::__new`] directly.
pub struct BelongsTo<C, P>
where
    C: EloquentModel,
    P: Model,
    P: From<<P::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <P::Entity as sea_orm::EntityTrait>::Model: From<P>
        + sea_orm::IntoActiveModel<<P::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <P::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<P::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// FK column value on the child row, JSON-serialised. `None`
    /// when the FK column is declared as `Option<T>` AND the row has
    /// a null in it. Stored as `serde_json::Value` rather than
    /// `sea_orm::Value` because the builder's `WhereTerm` storage is
    /// JSON; converting once at construction matches the `HasOne`
    /// shape and lets the macro emit
    /// `self.user_id.as_ref().map(|v| serde_json::to_value(v).unwrap_or(...))`
    /// uniformly across nullable / non-nullable FK fields.
    parent_key_value: Option<serde_json::Value>,
    /// FK column name on the child table.
    foreign_key: String,
    /// PK column name on the parent table (defaults to `"id"`,
    /// configurable via `lk = "..."` at the macro declaration site).
    owner_key: String,
    /// Optional default-row factory. Invoked by [`Self::first`] when
    /// the FK is null OR the parent lookup returns no row.
    ///
    /// Wrapped in `Arc` so the wrapper stays cheap to clone (the
    /// eager-load dispatcher arm clones it once per parent row when
    /// constructing each child's per-call template). Exposed via
    /// [`Self::__default_fn`] for that dispatcher; not part of the
    /// public API.
    default_fn: Option<Arc<dyn Fn() -> P + Send + Sync>>,
    /// PhantomData for the child type — see `HasOne::_phantom`.
    _phantom: PhantomData<fn() -> C>,
}

// The `P: Model` bound's where-clause is re-elaborated for the same
// reason `Builder<M: Model>` and `HasOne<L, R>` do — `first()` calls
// `P::query()` which carries its own associated-type bounds. See
// `has_one.rs` for the long-form explanation.
impl<C, P> BelongsTo<C, P>
where
    C: EloquentModel,
    P: Model,
    P: From<<P::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <P::Entity as sea_orm::EntityTrait>::Model: From<P>
        + sea_orm::IntoActiveModel<<P::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <P::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<P::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Construct a `BelongsTo` from the child row's FK value + the
    /// FK and owner-key column names. Invoked by the macro-emitted
    /// relation method; `parent_key_value` is the JSON-serialised
    /// FK value, or `None` for nullable FKs that hold null.
    #[doc(hidden)]
    pub fn __new(
        parent_key_value: Option<serde_json::Value>,
        foreign_key: String,
        owner_key: String,
    ) -> Self {
        Self {
            parent_key_value,
            foreign_key,
            owner_key,
            default_fn: None,
            _phantom: PhantomData,
        }
    }

    /// Override the FK column on the child post-construction.
    pub fn foreign_key(mut self, key: impl Into<String>) -> Self {
        self.foreign_key = key.into();
        self
    }

    /// Override the owner-key column on the parent post-construction.
    pub fn owner_key(mut self, key: impl Into<String>) -> Self {
        self.owner_key = key.into();
        self
    }

    /// Install a default-row closure. Invoked by [`Self::first`]
    /// (and by the eager-load dispatcher arm) when the FK is null
    /// OR no parent row matches.
    ///
    /// Mirrors Laravel's `->withDefault(fn () => new User([...]))`.
    /// Closure type is `Arc<dyn Fn() -> P + Send + Sync>` so the
    /// relation struct can be cloned cheaply by the eager loader
    /// without re-allocating the closure environment.
    pub fn with_default<F>(mut self, default: F) -> Self
    where
        F: Fn() -> P + Send + Sync + 'static,
    {
        self.default_fn = Some(Arc::new(default));
        self
    }

    /// Look up the parent row. Returns:
    /// - `Some(parent)` — FK present, parent exists.
    /// - `Some(default_fn())` — FK null OR parent missing, AND
    ///   `with_default` was installed.
    /// - `None` — FK null OR parent missing, AND no `with_default`.
    pub async fn first(self) -> Result<Option<P>, FrameworkError> {
        let key_value = match &self.parent_key_value {
            None => {
                // FK is null — short-circuit to the default if set.
                return Ok(self.default_fn.as_ref().map(|f| f()));
            }
            Some(v) => v.clone(),
        };
        let parent = P::query()
            .filter(self.owner_key.as_str(), key_value)
            .first()
            .await?;
        match parent {
            Some(p) => Ok(Some(p)),
            None => Ok(self.default_fn.as_ref().map(|f| f())),
        }
    }

    /// Internal accessor used by the eager-load dispatcher arm. Lets
    /// the macro-emitted code clone the default closure to apply per
    /// parent row when the eager IN-query misses or the FK is null.
    /// Not part of the public API; the `#[doc(hidden)]` keeps it out
    /// of docs.rs.
    #[doc(hidden)]
    pub fn __default_fn(&self) -> Option<Arc<dyn Fn() -> P + Send + Sync>> {
        self.default_fn.clone()
    }
}

impl<C, P> Relation for BelongsTo<C, P>
where
    C: EloquentModel,
    P: Model,
    P: From<<P::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <P::Entity as sea_orm::EntityTrait>::Model: From<P>
        + sea_orm::IntoActiveModel<<P::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <P::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<P::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    type Parent = C;
    type Target = P;
    const KIND: RelationKind = RelationKind::BelongsTo;

    fn parent_key(&self) -> &str {
        &self.owner_key
    }

    fn foreign_key(&self) -> &str {
        &self.foreign_key
    }
}

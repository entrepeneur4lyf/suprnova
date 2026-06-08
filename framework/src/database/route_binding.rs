//! Route model binding support
//!
//! Provides automatic model resolution from route parameters.
//!
//! # Automatic Route Model Binding
//!
//! Route model binding is automatic for all SeaORM models whose Entity implements
//! `suprnova::database::EntityExt`. Simply use the Model type as a handler parameter:
//!
//! ```rust,ignore
//! use suprnova::{handler, json_response, Response};
//! use crate::models::user;
//!
//! // Just use the Model in your handler - binding is automatic!
//! #[handler]
//! pub async fn show(user: user::Model) -> Response {
//!     json_response!({ "name": user.name })
//! }
//! ```
//!
//! The parameter name (`user`) is used as the route parameter key. So for a route
//! defined as `/users/{user}`, the `user` parameter will be automatically resolved.
//!
//! If the model is not found, a 404 Not Found response is returned.
//! If the parameter cannot be parsed, a 400 Bad Request response is returned.
//!
//! # Security: binding is identity, not authorization
//!
//! Route model binding answers **"does this row exist?"** — it does **not**
//! answer **"is the current user allowed to see this row?"**. Mirrors Laravel
//! semantics, but the implication is easy to miss:
//!
//! ```rust,ignore
//! // BAD: any authenticated user can view any post by guessing /posts/N.
//! #[handler]
//! pub async fn show(post: post::Model) -> Response {
//!     json_response!({ "title": post.title })
//! }
//! ```
//!
//! Authorize against the bound model in the handler using
//! [`crate::authorization::Gate`] (gates registered directly, or via the
//! [`policy`](crate::policy) attribute macro for per-model rules). The
//! framework's auth surface gives you the current user through
//! [`Auth::user_as::<U>()`](crate::auth::Auth::user_as):
//!
//! ```rust,ignore
//! use suprnova::{handler, json_response, Auth, FrameworkError, Response};
//! use suprnova::authorization::Gate;
//! use crate::models::{Post, User};
//!
//! #[handler]
//! pub async fn show(post: Post) -> Result<Response, FrameworkError> {
//!     let current_user = Auth::user_as::<User>()
//!         .await?
//!         .ok_or(FrameworkError::Unauthorized)?;
//!     Gate::authorize("view-post", &current_user, &post)?;
//!     Ok(json_response!({ "title": post.title }))
//! }
//! ```
//!
//! `Gate::authorize` takes the action name first, then the user, then the
//! resource. It returns `Err(FrameworkError::Unauthorized)` (mapped to 403)
//! when denied. See `framework/tests/authorization.rs` and
//! `app/src/controllers/admin.rs` for working examples.
//!
//! The 404 returned on a missing row does NOT prevent IDOR probing — a 404
//! vs. 403 split discloses existence. If existence-disclosure matters in
//! your threat model, return 404 from the policy too (so unauthorized rows
//! look identical to non-existent ones).

use crate::error::FrameworkError;
use async_trait::async_trait;
use sea_orm::{EntityTrait, ModelTrait as SeaModelTrait, PrimaryKeyTrait};
use std::ops::{Deref, DerefMut};

/// Newtype wrapper that opts a route-bound model into Eloquent's
/// SCOPED `find` path — global scopes, soft-delete filter, and
/// per-model `#[model(connection = "...")]` routing all apply.
///
/// Wrap your Eloquent model struct in `RouteParam<M>` in the
/// `#[handler]` signature:
///
/// ```rust,ignore
/// use suprnova::{handler, json_response, RouteParam, Response};
/// use crate::models::User;
///
/// #[handler]
/// pub async fn show(RouteParam(user): RouteParam<User>) -> Response {
///     // `user` is a fully-hydrated User; soft-deleted rows are
///     // filtered, global scopes applied, and the read routes
///     // through the user's `#[model(connection = "...")]` default.
///     json_response!({ "name": user.name })
/// }
/// ```
///
/// # Two binding paths, two contracts
///
/// The framework ships two route-binding shapes with different scope
/// policies — mirroring Laravel's `Route::model(...)->withTrashed()`
/// opt-in. Pick by intent:
///
/// | Handler signature                | Scope policy |
/// |----------------------------------|--------------|
/// | `user: user::Model`              | **Unscoped** — bypasses Eloquent scopes; soft-deleted rows are exposed. The explicit raw escape hatch (mirrors Laravel's `->withTrashed()`). |
/// | `RouteParam(user): RouteParam<User>` | **Scoped** — applies global scopes + soft-delete filter; the safe default. |
///
/// New code should prefer `RouteParam<User>`. The bare `user::Model`
/// path remains supported as the explicit escape hatch for admin
/// tooling that must reach trashed rows by id.
///
/// `Deref<Target = M>` lets handlers read fields directly through the
/// wrapper (`route_param.name`) when destructuring isn't convenient.
#[derive(Debug, Clone)]
pub struct RouteParam<M>(pub M);

impl<M> RouteParam<M> {
    /// Move the inner model out of the wrapper.
    pub fn into_inner(self) -> M {
        self.0
    }
}

impl<M> Deref for RouteParam<M> {
    type Target = M;
    fn deref(&self) -> &M {
        &self.0
    }
}

impl<M> DerefMut for RouteParam<M> {
    fn deref_mut(&mut self) -> &mut M {
        &mut self.0
    }
}

/// Trait for models that can be automatically resolved from route parameters
///
/// Implement this trait on your SeaORM Model types to enable automatic
/// route model binding in handlers. When a route parameter matches the
/// `param_name()`, the model will be automatically fetched from the database.
///
/// If the model is not found, a 404 Not Found response is returned.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::database::RouteBinding;
/// use suprnova::FrameworkError;
///
/// #[async_trait]
/// impl RouteBinding for user::Model {
///     fn param_name() -> &'static str {
///         "user"  // matches {user} in route like /users/{user}
///     }
///
///     async fn from_route_param(value: &str) -> Result<Self, FrameworkError> {
///         let id: i32 = value.parse()
///             .map_err(|_| FrameworkError::param_parse(value, "i32"))?;
///
///         user::Entity::find_by_pk(id)
///             .await?
///             .ok_or_else(|| FrameworkError::model_not_found("User"))
///     }
/// }
/// ```
#[async_trait]
pub trait RouteBinding: Sized + Send {
    /// The route parameter name to bind from
    ///
    /// This should match the parameter placeholder in your route definition.
    /// For example, if your route is `/users/{user}`, this should return `"user"`.
    fn param_name() -> &'static str;

    /// Fetch the model from the database using the route parameter value
    ///
    /// This method is called automatically by the `#[handler]` macro when
    /// a parameter of this type is declared in the handler function.
    ///
    /// # Returns
    ///
    /// - `Ok(Self)` - The model was found
    /// - `Err(FrameworkError::ModelNotFound)` - Model not found (returns 404)
    /// - `Err(FrameworkError::ParamParse)` - Parameter could not be parsed (returns 400)
    async fn from_route_param(value: &str) -> Result<Self, FrameworkError>;
}

/// Trait for automatic route model binding
///
/// This trait is automatically implemented for all SeaORM models whose Entity
/// implements `suprnova::database::EntityExt`. You don't need to implement this manually.
///
/// Unlike [`RouteBinding`], this trait doesn't require a `param_name()` method.
/// The parameter name is derived from the handler function signature.
///
/// # Example
///
/// ```rust,ignore
/// // Just use Model in handler - binding is automatic!
/// #[handler]
/// pub async fn show(user: user::Model) -> Response {
///     json_response!({ "name": user.name })
/// }
/// ```
#[async_trait]
pub trait AutoRouteBinding: Sized + Send {
    /// Fetch the model from the database using the route parameter value
    ///
    /// This method parses the parameter as the primary key type and fetches
    /// the corresponding model from the database.
    ///
    /// # Returns
    ///
    /// - `Ok(Self)` - The model was found
    /// - `Err(FrameworkError::ModelNotFound)` - Model not found (returns 404)
    /// - `Err(FrameworkError::ParamParse)` - Parameter could not be parsed (returns 400)
    async fn from_route_param(value: &str) -> Result<Self, FrameworkError>;
}

/// Blanket implementation of AutoRouteBinding for all SeaORM models
///
/// This automatically implements route model binding for any SeaORM Model type
/// whose Entity implements `suprnova::database::EntityExt`. Supports any primary key type
/// that implements `FromStr` (i32, i64, String, UUID, etc.).
#[async_trait]
impl<M, E> AutoRouteBinding for M
where
    M: SeaModelTrait<Entity = E> + Send + Sync,
    E: EntityTrait<Model = M> + crate::database::EntityExt + Sync,
    E::PrimaryKey: PrimaryKeyTrait,
    <E::PrimaryKey as PrimaryKeyTrait>::ValueType: std::str::FromStr + Send,
{
    async fn from_route_param(value: &str) -> Result<Self, FrameworkError> {
        let id: <E::PrimaryKey as PrimaryKeyTrait>::ValueType = value.parse().map_err(|_| {
            FrameworkError::param_parse(
                value,
                std::any::type_name::<<E::PrimaryKey as PrimaryKeyTrait>::ValueType>(),
            )
        })?;

        <E as crate::database::EntityExt>::find_by_pk(id)
            .await?
            .ok_or_else(|| {
                // Extract a cleaner model name from the full type name
                let full_name = std::any::type_name::<M>();
                let model_name = full_name.rsplit("::").nth(1).unwrap_or(full_name);
                FrameworkError::model_not_found(model_name)
            })
    }
}

/// Convenience macro to implement RouteBinding for a SeaORM model
///
/// **DEPRECATED**: This macro is no longer needed. Route model binding is now
/// automatic for any model whose Entity implements `suprnova::database::EntityExt`.
/// Simply use the Model type in your handler parameter.
///
/// This macro implements the `RouteBinding` trait for a model, enabling
/// automatic route model binding with 404 handling.
///
/// # Arguments
///
/// - `$entity` - The SeaORM Entity type (e.g., `user::Entity`)
/// - `$model` - The SeaORM Model type (e.g., `user::Model`)
/// - `$param` - The route parameter name (e.g., `"user"`)
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::route_binding;
///
/// // In your model file (e.g., models/user.rs)
/// route_binding!(Entity, Model, "user");
///
/// // Now you can use automatic binding in handlers:
/// #[handler]
/// pub async fn show(user: user::Model) -> Response {
///     json_response!({ "id": user.id, "name": user.name })
/// }
/// ```
///
/// # Route Definition
///
/// The parameter name must match your route definition:
///
/// ```rust,ignore
/// routes! {
///     get!("/users/{user}", controllers::user::show),
/// }
/// ```
#[macro_export]
macro_rules! route_binding {
    ($entity:ty, $model:ty, $param:literal) => {
        #[async_trait::async_trait]
        impl $crate::RouteBinding for $model {
            fn param_name() -> &'static str {
                $param
            }

            async fn from_route_param(value: &str) -> Result<Self, $crate::FrameworkError> {
                let id: i32 = value
                    .parse()
                    .map_err(|_| $crate::FrameworkError::param_parse(value, "i32"))?;

                <$entity as $crate::database::EntityExt>::find_by_pk(id)
                    .await?
                    .ok_or_else(|| $crate::FrameworkError::model_not_found(stringify!($model)))
            }
        }
    };
}

/// Scoped route-model binding for Eloquent user structs.
///
/// Routes through `M::find(id)` (the Eloquent CRUD entrypoint) so the
/// hydrated row applies the model's global scopes, soft-delete
/// filter, and per-model connection — see [`RouteParam`] for the full
/// rationale. The bounds mirror the [`crate::eloquent::Model`] trait
/// surface so any `#[suprnova::model]`-generated struct fits.
#[async_trait]
impl<M> AutoRouteBinding for RouteParam<M>
where
    M: crate::eloquent::Model + Send + Sync,
    M: From<<<M as crate::eloquent::EloquentModel>::Entity as EntityTrait>::Model>,
    <<M as crate::eloquent::EloquentModel>::Entity as EntityTrait>::Model: From<M>
        + sea_orm::IntoActiveModel<
            <<M as crate::eloquent::EloquentModel>::Entity as EntityTrait>::ActiveModel,
        > + serde::Serialize
        + Send
        + Sync,
    <<M as crate::eloquent::EloquentModel>::Entity as EntityTrait>::ActiveModel: Send,
    <<<M as crate::eloquent::EloquentModel>::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType:
        std::str::FromStr + Send + Into<sea_orm::Value>,
{
    async fn from_route_param(value: &str) -> Result<Self, FrameworkError> {
        let id: <<<M as crate::eloquent::EloquentModel>::Entity as EntityTrait>::PrimaryKey
            as PrimaryKeyTrait>::ValueType = value.parse().map_err(|_| {
            FrameworkError::param_parse(
                value,
                std::any::type_name::<
                    <<<M as crate::eloquent::EloquentModel>::Entity as EntityTrait>::PrimaryKey
                        as PrimaryKeyTrait>::ValueType,
                >(),
            )
        })?;
        let row = M::find(id).await?.ok_or_else(|| {
            let full = std::any::type_name::<M>();
            // Strip leading module path so the error reads `User`
            // rather than `app::models::user::User`.
            let model_name = full.rsplit("::").next().unwrap_or(full);
            FrameworkError::model_not_found(model_name)
        })?;
        Ok(RouteParam(row))
    }
}

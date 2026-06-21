//! SeaORM models for framework-owned RBAC tables.

use chrono::{DateTime, Utc};

/// Role row, usually named for a coarse application capability such as
/// `"admin"` or `"author"`.
#[suprnova::model(table = "roles", timestamps)]
pub struct Role {
    /// Primary key.
    pub id: i64,
    /// Role name, unique with [`Self::guard_name`].
    pub name: String,
    /// Human-readable label shown in admin UIs.
    pub display_name: Option<String>,
    /// Guard namespace; defaults to `"web"` for normal session users.
    pub guard_name: String,
    /// Timestamp at which the row was inserted.
    pub created_at: DateTime<Utc>,
    /// Timestamp at which the row was last mutated.
    pub updated_at: DateTime<Utc>,
}

/// Permission row, usually named as a dotted ability such as
/// `"articles.create"`.
#[suprnova::model(table = "permissions", timestamps)]
pub struct Permission {
    /// Primary key.
    pub id: i64,
    /// Permission name, unique with [`Self::guard_name`].
    pub name: String,
    /// Human-readable label shown in admin UIs.
    pub display_name: Option<String>,
    /// Guard namespace; defaults to `"web"` for normal session users.
    pub guard_name: String,
    /// Timestamp at which the row was inserted.
    pub created_at: DateTime<Utc>,
    /// Timestamp at which the row was last mutated.
    pub updated_at: DateTime<Utc>,
}

/// Join row assigning a permission to a role.
#[suprnova::model(table = "role_permissions")]
pub struct RolePermission {
    /// Primary key.
    pub id: i64,
    /// ID from the `roles` table.
    pub role_id: i64,
    /// ID from the `permissions` table.
    pub permission_id: i64,
}

/// Polymorphic join row assigning a role to a model.
#[suprnova::model(table = "model_roles")]
pub struct ModelRole {
    /// Primary key.
    pub id: i64,
    /// Short model discriminator, for example `"User"`.
    pub model_type: String,
    /// Model identifier string. Numeric and opaque IDs both round-trip.
    pub model_id: String,
    /// ID from the `roles` table.
    pub role_id: i64,
}

/// Polymorphic join row assigning a direct permission to a model.
#[suprnova::model(table = "model_permissions")]
pub struct ModelPermission {
    /// Primary key.
    pub id: i64,
    /// Short model discriminator, for example `"User"`.
    pub model_type: String,
    /// Model identifier string. Numeric and opaque IDs both round-trip.
    pub model_id: String,
    /// ID from the `permissions` table.
    pub permission_id: i64,
}

pub use model_permission::{
    ActiveModel as ModelPermissionActiveModel, Column as ModelPermissionColumn,
    Entity as ModelPermissionEntity, Model as ModelPermissionModel,
};
pub use model_role::{
    ActiveModel as ModelRoleActiveModel, Column as ModelRoleColumn, Entity as ModelRoleEntity,
    Model as ModelRoleModel,
};
pub use permission::{
    ActiveModel as PermissionActiveModel, Column as PermissionColumn, Entity as PermissionEntity,
    Model as PermissionModel,
};
pub use role::{
    ActiveModel as RoleActiveModel, Column as RoleColumn, Entity as RoleEntity, Model as RoleModel,
};
pub use role_permission::{
    ActiveModel as RolePermissionActiveModel, Column as RolePermissionColumn,
    Entity as RolePermissionEntity, Model as RolePermissionModel,
};

//! Role and permission helpers for authenticatable models.

use async_trait::async_trait;
use sea_orm::Value;

use crate::{Authenticatable, DB, FrameworkError};

const DEFAULT_GUARD: &str = "web";

fn value(s: impl Into<String>) -> Value {
    Value::from(s.into())
}

fn int_value(i: i64) -> Value {
    Value::from(i)
}

fn short_type_name<T: ?Sized>() -> String {
    std::any::type_name::<T>()
        .rsplit("::")
        .next()
        .unwrap_or("Model")
        .to_string()
}

async fn find_role_id(name: &str, guard_name: &str) -> Result<Option<i64>, FrameworkError> {
    let row = DB::select_one(
        "SELECT id FROM roles WHERE name = ? AND guard_name = ? LIMIT 1",
        vec![value(name), value(guard_name)],
    )
    .await?;
    row.map(|row| row.get_int("id")).transpose()
}

async fn find_permission_id(name: &str, guard_name: &str) -> Result<Option<i64>, FrameworkError> {
    let row = DB::select_one(
        "SELECT id FROM permissions WHERE name = ? AND guard_name = ? LIMIT 1",
        vec![value(name), value(guard_name)],
    )
    .await?;
    row.map(|row| row.get_int("id")).transpose()
}

async fn exists(sql: &str, values: Vec<Value>) -> Result<bool, FrameworkError> {
    let count: i64 = DB::scalar(sql, values).await?;
    Ok(count > 0)
}

/// Create a role on the default `"web"` guard, returning its id.
///
/// The helper is idempotent: an existing `(name, guard_name)` row is returned
/// instead of inserting a duplicate.
pub async fn create_role(name: &str) -> Result<i64, FrameworkError> {
    create_role_on_guard(name, DEFAULT_GUARD).await
}

/// Create a role for a named guard, returning its id.
///
/// Use this when an app separates session and token principals with distinct
/// guards.
pub async fn create_role_on_guard(name: &str, guard_name: &str) -> Result<i64, FrameworkError> {
    if let Some(id) = find_role_id(name, guard_name).await? {
        return Ok(id);
    }
    DB::insert(
        "INSERT INTO roles (name, display_name, guard_name) VALUES (?, ?, ?)",
        vec![value(name), value(name), value(guard_name)],
    )
    .await?;
    find_role_id(name, guard_name)
        .await?
        .ok_or_else(|| FrameworkError::database("rbac create_role: inserted role was not found"))
}

/// Create a permission on the default `"web"` guard, returning its id.
///
/// The helper is idempotent: an existing `(name, guard_name)` row is returned
/// instead of inserting a duplicate.
pub async fn create_permission(name: &str) -> Result<i64, FrameworkError> {
    create_permission_on_guard(name, DEFAULT_GUARD).await
}

/// Create a permission for a named guard, returning its id.
///
/// Permissions are conventionally dotted ability names such as
/// `"articles.publish"`.
pub async fn create_permission_on_guard(
    name: &str,
    guard_name: &str,
) -> Result<i64, FrameworkError> {
    if let Some(id) = find_permission_id(name, guard_name).await? {
        return Ok(id);
    }
    DB::insert(
        "INSERT INTO permissions (name, display_name, guard_name) VALUES (?, ?, ?)",
        vec![value(name), value(name), value(guard_name)],
    )
    .await?;
    find_permission_id(name, guard_name).await?.ok_or_else(|| {
        FrameworkError::database("rbac create_permission: inserted permission was not found")
    })
}

/// Give a permission to a role on the default `"web"` guard.
///
/// Missing roles or permissions are created before the assignment is stored.
pub async fn give_permission_to_role(
    role_name: &str,
    permission_name: &str,
) -> Result<(), FrameworkError> {
    give_permission_to_role_on_guard(role_name, permission_name, DEFAULT_GUARD).await
}

/// Give a permission to a role for a named guard.
///
/// The assignment is idempotent and ignores an existing join row.
pub async fn give_permission_to_role_on_guard(
    role_name: &str,
    permission_name: &str,
    guard_name: &str,
) -> Result<(), FrameworkError> {
    let role_id = create_role_on_guard(role_name, guard_name).await?;
    let permission_id = create_permission_on_guard(permission_name, guard_name).await?;
    if exists(
        "SELECT COUNT(*) FROM role_permissions WHERE role_id = ? AND permission_id = ?",
        vec![int_value(role_id), int_value(permission_id)],
    )
    .await?
    {
        return Ok(());
    }
    DB::insert(
        "INSERT INTO role_permissions (role_id, permission_id) VALUES (?, ?)",
        vec![int_value(role_id), int_value(permission_id)],
    )
    .await?;
    Ok(())
}

/// Assign a role to a model on the default `"web"` guard.
///
/// `model_type` is usually the short Rust type name, for example `"User"`.
pub async fn assign_role_to_model(
    model_type: &str,
    model_id: &str,
    role_name: &str,
) -> Result<(), FrameworkError> {
    assign_role_to_model_on_guard(model_type, model_id, role_name, DEFAULT_GUARD).await
}

/// Assign a role to a model for a named guard.
///
/// The assignment is idempotent and stores `model_id` as a string so numeric,
/// UUID, and external-provider identifiers all work.
pub async fn assign_role_to_model_on_guard(
    model_type: &str,
    model_id: &str,
    role_name: &str,
    guard_name: &str,
) -> Result<(), FrameworkError> {
    let role_id = create_role_on_guard(role_name, guard_name).await?;
    if exists(
        "SELECT COUNT(*) FROM model_roles WHERE model_type = ? AND model_id = ? AND role_id = ?",
        vec![value(model_type), value(model_id), int_value(role_id)],
    )
    .await?
    {
        return Ok(());
    }
    DB::insert(
        "INSERT INTO model_roles (model_type, model_id, role_id) VALUES (?, ?, ?)",
        vec![value(model_type), value(model_id), int_value(role_id)],
    )
    .await?;
    Ok(())
}

/// Give a direct permission to a model on the default `"web"` guard.
///
/// Direct permissions are checked in addition to permissions inherited from
/// assigned roles.
pub async fn give_permission_to_model(
    model_type: &str,
    model_id: &str,
    permission_name: &str,
) -> Result<(), FrameworkError> {
    give_permission_to_model_on_guard(model_type, model_id, permission_name, DEFAULT_GUARD).await
}

/// Give a direct permission to a model for a named guard.
///
/// The assignment is idempotent and creates the permission row when needed.
pub async fn give_permission_to_model_on_guard(
    model_type: &str,
    model_id: &str,
    permission_name: &str,
    guard_name: &str,
) -> Result<(), FrameworkError> {
    let permission_id = create_permission_on_guard(permission_name, guard_name).await?;
    if exists(
        "SELECT COUNT(*) FROM model_permissions WHERE model_type = ? AND model_id = ? AND permission_id = ?",
        vec![value(model_type), value(model_id), int_value(permission_id)],
    )
    .await?
    {
        return Ok(());
    }
    DB::insert(
        "INSERT INTO model_permissions (model_type, model_id, permission_id) VALUES (?, ?, ?)",
        vec![value(model_type), value(model_id), int_value(permission_id)],
    )
    .await?;
    Ok(())
}

/// Check whether a model has a role on the default `"web"` guard.
pub async fn has_role_for_model(
    model_type: &str,
    model_id: &str,
    role_name: &str,
) -> Result<bool, FrameworkError> {
    has_role_for_model_on_guard(model_type, model_id, role_name, DEFAULT_GUARD).await
}

/// Check whether a model has a role for a named guard.
pub async fn has_role_for_model_on_guard(
    model_type: &str,
    model_id: &str,
    role_name: &str,
    guard_name: &str,
) -> Result<bool, FrameworkError> {
    exists(
        "SELECT COUNT(*) FROM model_roles \
         INNER JOIN roles ON roles.id = model_roles.role_id \
         WHERE model_roles.model_type = ? \
           AND model_roles.model_id = ? \
           AND roles.name = ? \
           AND roles.guard_name = ?",
        vec![
            value(model_type),
            value(model_id),
            value(role_name),
            value(guard_name),
        ],
    )
    .await
}

/// Check whether a model has a permission on the default `"web"` guard.
///
/// Both direct model permissions and permissions inherited through assigned
/// roles are considered.
pub async fn has_permission_for_model(
    model_type: &str,
    model_id: &str,
    permission_name: &str,
) -> Result<bool, FrameworkError> {
    has_permission_for_model_on_guard(model_type, model_id, permission_name, DEFAULT_GUARD).await
}

/// Check whether a model has a permission for a named guard.
///
/// Both direct model permissions and permissions inherited through assigned
/// roles are considered.
pub async fn has_permission_for_model_on_guard(
    model_type: &str,
    model_id: &str,
    permission_name: &str,
    guard_name: &str,
) -> Result<bool, FrameworkError> {
    let direct = exists(
        "SELECT COUNT(*) FROM model_permissions \
         INNER JOIN permissions ON permissions.id = model_permissions.permission_id \
         WHERE model_permissions.model_type = ? \
           AND model_permissions.model_id = ? \
           AND permissions.name = ? \
           AND permissions.guard_name = ?",
        vec![
            value(model_type),
            value(model_id),
            value(permission_name),
            value(guard_name),
        ],
    )
    .await?;
    if direct {
        return Ok(true);
    }

    exists(
        "SELECT COUNT(*) FROM model_roles \
         INNER JOIN roles ON roles.id = model_roles.role_id \
         INNER JOIN role_permissions ON role_permissions.role_id = roles.id \
         INNER JOIN permissions ON permissions.id = role_permissions.permission_id \
         WHERE model_roles.model_type = ? \
           AND model_roles.model_id = ? \
           AND permissions.name = ? \
           AND permissions.guard_name = ? \
           AND roles.guard_name = ?",
        vec![
            value(model_type),
            value(model_id),
            value(permission_name),
            value(guard_name),
            value(guard_name),
        ],
    )
    .await
}

/// Trait for authenticatable models that can receive RBAC roles and
/// permissions.
///
/// The default model discriminator is the short Rust type name
/// (`User` for `crate::models::user::User`). Override [`Self::rbac_model_type`]
/// when an app needs a stable custom discriminator.
#[async_trait]
pub trait HasRoles: Authenticatable {
    /// Model discriminator stored in `model_roles.model_type` and
    /// `model_permissions.model_type`.
    fn rbac_model_type(&self) -> String {
        short_type_name::<Self>()
    }

    /// Model identifier stored in `model_roles.model_id` and
    /// `model_permissions.model_id`.
    fn rbac_model_id(&self) -> String {
        self.get_auth_identifier()
    }

    /// Assign this model a role on the default `"web"` guard.
    async fn assign_role(&self, role_name: &str) -> Result<(), FrameworkError> {
        assign_role_to_model(&self.rbac_model_type(), &self.rbac_model_id(), role_name).await
    }

    /// Give this model a direct permission on the default `"web"` guard.
    async fn give_permission_to(&self, permission_name: &str) -> Result<(), FrameworkError> {
        give_permission_to_model(
            &self.rbac_model_type(),
            &self.rbac_model_id(),
            permission_name,
        )
        .await
    }

    /// Check whether this model has a role on the default `"web"` guard.
    async fn has_role(&self, role_name: &str) -> Result<bool, FrameworkError> {
        has_role_for_model(&self.rbac_model_type(), &self.rbac_model_id(), role_name).await
    }

    /// Check whether this model has a permission on the default `"web"` guard.
    ///
    /// Direct permissions and role-inherited permissions are both considered.
    async fn has_permission_to(&self, permission_name: &str) -> Result<bool, FrameworkError> {
        has_permission_for_model(
            &self.rbac_model_type(),
            &self.rbac_model_id(),
            permission_name,
        )
        .await
    }
}

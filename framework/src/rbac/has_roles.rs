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
/// `model_type` is the model discriminator — for [`HasRoles`] implementors
/// this is [`HasRoles::rbac_model_type`], which defaults to the
/// fully-qualified Rust type path.
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
/// The default model discriminator is the fully-qualified Rust type path
/// (`crate::models::user::User`, not the short leaf `User`). Using the full
/// path means two distinct authenticatable types that happen to share a leaf
/// name cannot silently collide on the same `(model_type, model_id)` rows —
/// which would leak one type's roles and permissions onto the other.
///
/// Override [`Self::rbac_model_type`] when an app wants a stable custom
/// discriminator. Two requirements for any override:
///
/// 1. **It must be globally unique** across every authenticatable type that
///    shares the same RBAC tables — two types returning the same string will
///    share roles and permissions for matching ids.
/// 2. **Prefer an override for any persisted discriminator you depend on.**
///    `std::any::type_name` is *not* guaranteed stable across compiler
///    versions or refactors (renaming or moving the type changes it), so apps
///    that persist `model_type` long-term and need it to stay constant should
///    return their own fixed string rather than rely on the default.
#[async_trait]
pub trait HasRoles: Authenticatable {
    /// Model discriminator stored in `model_roles.model_type` and
    /// `model_permissions.model_type`.
    fn rbac_model_type(&self) -> String {
        std::any::type_name::<Self>().to_string()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::Any;
    use std::sync::Arc;

    // Two distinct authenticatable types that share the leaf name `Account`
    // but live in separate modules, so their fully-qualified type paths
    // differ. With the short-leaf-name default they collided; the
    // fully-qualified default must keep them apart.
    mod first {
        use super::*;

        pub struct Account {
            pub id: i64,
        }

        impl Authenticatable for Account {
            fn get_auth_identifier(&self) -> String {
                self.id.to_string()
            }

            fn as_any(&self) -> &dyn Any {
                self
            }

            fn into_arc_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
                self
            }
        }

        impl HasRoles for Account {}
    }

    mod second {
        use super::*;

        pub struct Account {
            pub id: i64,
        }

        impl Authenticatable for Account {
            fn get_auth_identifier(&self) -> String {
                self.id.to_string()
            }

            fn as_any(&self) -> &dyn Any {
                self
            }

            fn into_arc_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
                self
            }
        }

        impl HasRoles for Account {}
    }

    #[test]
    fn distinct_types_with_same_leaf_name_get_distinct_discriminators() {
        let a = first::Account { id: 7 };
        let b = second::Account { id: 7 };

        // Same leaf name, same id — but the discriminators must differ so
        // the two types cannot share roles/permissions rows.
        assert_ne!(
            a.rbac_model_type(),
            b.rbac_model_type(),
            "distinct types sharing a leaf name must not share an RBAC discriminator"
        );
        // The default is the fully-qualified type path, not the leaf.
        assert!(a.rbac_model_type().contains("::"));
        assert!(a.rbac_model_type().ends_with("Account"));
        assert!(b.rbac_model_type().ends_with("Account"));
    }

    #[test]
    fn rbac_model_id_uses_auth_identifier() {
        let a = first::Account { id: 42 };
        assert_eq!(a.rbac_model_id(), "42");
    }
}

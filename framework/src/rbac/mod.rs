//! Lightweight role and permission authorization.
//!
//! The RBAC module stores framework-owned roles, permissions, and
//! polymorphic model assignments. Consumer apps opt in by registering
//! [`migrations::CreateRbacTables`] in their migrator and implementing
//! [`HasRoles`] on their authenticatable user model.

pub mod entity;
mod has_roles;
mod middleware;
pub mod migrations;

pub use has_roles::{
    HasRoles, assign_role_to_model, assign_role_to_model_on_guard, create_permission,
    create_permission_on_guard, create_role, create_role_on_guard, give_permission_to_model,
    give_permission_to_model_on_guard, give_permission_to_role, give_permission_to_role_on_guard,
    has_permission_for_model, has_permission_for_model_on_guard, has_role_for_model,
    has_role_for_model_on_guard,
};
pub use middleware::{PermissionMiddleware, RoleMiddleware};

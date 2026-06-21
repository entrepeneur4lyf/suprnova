//! Framework-owned migrations for role-based access control.

pub mod m_create_rbac_tables;

/// Public alias for the RBAC table migration used by consumer apps.
pub use m_create_rbac_tables::Migration as CreateRbacTables;

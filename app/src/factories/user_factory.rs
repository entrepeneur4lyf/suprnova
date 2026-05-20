//! `UserFactory` — dogfood for the framework's Factory trait against
//! the migrated `#[suprnova::model]` `User` entity.
//!
//! Phase 10A T11 migrated `User` from a hand-rolled SeaORM entity to
//! the `#[suprnova::model]` macro. The macro emits the SeaORM
//! `Model` row type inside the per-struct inner module (here
//! `crate::models::users::user::Model`); that inner row is what the
//! framework's blanket `Persistable for SeaORM::Model` impl covers,
//! so the factory targets it directly. End-user reads still use the
//! Eloquent-facing `User` struct via the macro-emitted
//! `From<user::Model> for User` bridge.

use std::sync::atomic::{AtomicU64, Ordering};
use suprnova::Factory;

use crate::models::users::user::Model as UserRow;

/// Process-wide counter so successive `definition()` calls produce
/// unique emails — the `users.email` column isn't UNIQUE in the
/// schema, but the BaseSeeder dogfood expects distinguishable rows.
static UNIQUE: AtomicU64 = AtomicU64::new(1);

pub struct UserFactory;

impl Factory for UserFactory {
    type Model = UserRow;

    fn definition() -> UserRow {
        let seq = UNIQUE.fetch_add(1, Ordering::Relaxed);
        // Storage-typed (i.e. column-shape) value here — the macro's
        // cast pipeline only enters via `User::create(attrs!{...})`.
        // For the SeaORM-direct persist path the factory writes
        // straight into the column's stored type. `active` is INTEGER
        // because `AsBool::Storage = i64`, but the field name on the
        // inner Model still matches the user-facing column name.
        UserRow {
            // `0` here is a placeholder — the framework's blanket
            // `Persistable` impl flips primary-key columns to `NotSet`
            // before inserting so SQLite assigns the real id.
            id: 0,
            name: format!("Factory User #{seq}"),
            email: format!("factory-{seq}@example.suprnova.app"),
            password: "factory-placeholder".into(),
            remember_token: None,
            // AsBool stores as i64.
            active: 1,
            // AsDateTime stores as String (RFC-3339).
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            // AsOptionalDateTime stores as Option<String>.
            deleted_at: None,
        }
    }
}

//! `UserFactory` — dogfood for the framework's Factory trait against
//! the Eloquent-facing `#[suprnova::model]` `User` struct.
//!
//! Phase 10A T11 polish broadened `Persistable` to cover Eloquent
//! structs directly (via a per-struct impl emitted by the macro), so
//! the factory targets the user-facing `User` rather than the inner
//! SeaORM `user::Model` row. The factory writes runtime values
//! (`active: true`, `chrono::Utc::now()`) — the cast pipeline
//! (`AsBool`, the auto-injected `AsDateTime` / `AsOptionalDateTime`)
//! converts to storage shape on insert.

use std::sync::atomic::{AtomicU64, Ordering};
use suprnova::Factory;

use crate::models::users::User;

/// Process-wide counter so successive `definition()` calls produce
/// unique emails — the `users.email` column isn't UNIQUE in the
/// schema, but the BaseSeeder dogfood expects distinguishable rows.
static UNIQUE: AtomicU64 = AtomicU64::new(1);

pub struct UserFactory;

impl Factory for UserFactory {
    type Model = User;

    fn definition() -> User {
        let seq = UNIQUE.fetch_add(1, Ordering::Relaxed);
        let now = chrono::Utc::now();
        // Runtime shape — no storage-side translation leaks into the
        // factory. The macro-emitted `Persistable for User` bridges
        // through the inner `user::Model` and runs every cast's
        // `to_storage` on insert.
        User {
            // `0` here is a placeholder — `persist_via_seaorm` flips
            // primary-key columns to `NotSet` before inserting so
            // SQLite assigns the real id.
            id: 0,
            name: format!("Factory User #{seq}"),
            email: format!("factory-{seq}@example.suprnova.app"),
            password: "factory-placeholder".into(),
            remember_token: None,
            // AsBool cast handles `bool` → INTEGER at storage time.
            active: true,
            created_at: now,
            updated_at: now,
            // Soft-delete tombstone; the AsOptionalDateTime cast
            // routes `None` → NULL.
            deleted_at: None,
            // Phase 10B T1 — relations scratch state. Always starts
            // empty; the eager loader fills the cache when the row
            // came from a `with([...])` query and the BelongsToMany
            // loader fills `__pivot` when it came from an m2m chain.
            __eager: Default::default(),
            __pivot: None,
        }
    }
}

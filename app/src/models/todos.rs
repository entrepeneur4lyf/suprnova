//! Todo model — migrated to `#[suprnova::model]` in Phase 10A T11.
//!
//! Dogfoods the `AsBool` cast (on `done`) and the auto-managed
//! timestamps on a real DB-backed entity.

use chrono::{DateTime, Utc};
use suprnova::model;

#[model(
    table = "todos",
    fillable = ["title", "description", "done"],
    casts = {
        done = ::suprnova::AsBool,
    },
    timestamps,
)]
pub struct Todo {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub done: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// Re-export the SeaORM types the macro emits inside the per-model
// inner module — see `users.rs` for rationale.
pub use todo::{ActiveModel, Column, Entity};

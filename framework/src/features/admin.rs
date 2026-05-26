//! Admin CRUD for the `features` table.
//!
//! Intended to back Phase 8's admin panel + any custom admin UI a
//! consumer builds. All mutations fire the appropriate event
//! ([`FeatureUpdated`] / [`FeatureDeleted`]) for audit listeners and
//! also call [`crate::features::sync::notify`] so live in-process
//! evaluators (DB snapshot + caches) reflect the new state before the
//! mutation returns. Bind a
//! [`FeatureSync`](crate::features::FeatureSync) into the App
//! container (the default is the composite produced by
//! [`crate::features::bootstrap_database_cached`]) to enable
//! sub-second propagation; without it `notify` is a no-op and the
//! row reaches readers only after a manual reload or TTL.

use crate::database::DB;
use crate::error::FrameworkError;
use crate::features::entity;
use crate::features::events::{FeatureDeleted, FeatureUpdated};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, sea_query::OnConflict,
};
use serde::Serialize;

/// One row in the `features` table, projected for admin consumers.
/// Mirrors the entity but lives separately so it can carry a
/// `Serialize` impl without forcing one onto the active-record
/// entity.
#[derive(Debug, Clone, Serialize)]
pub struct FeatureRow {
    pub id: i64,
    pub name: String,
    pub scope_key: String,
    pub enabled: bool,
    pub description: Option<String>,
    pub updated_by: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<entity::Model> for FeatureRow {
    fn from(m: entity::Model) -> Self {
        // Phase 10A T11 — `entity::Model` now carries the storage
        // shape (RFC-3339 string timestamps from the `AsDateTime`
        // cast). Parse back to `DateTime<Utc>` for the admin/JSON
        // surface; failures fall back to the unix epoch so a corrupt
        // row never panics the admin listing — the FeatureRow is
        // serialised by the admin UI which treats parse errors as
        // "unknown timestamp" rather than fatal.
        let parse = |s: &str| {
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::DateTime::<chrono::Utc>::UNIX_EPOCH)
        };
        Self {
            id: m.id,
            name: m.name,
            scope_key: m.scope_key,
            enabled: m.enabled,
            description: m.description,
            updated_by: m.updated_by,
            created_at: parse(&m.created_at),
            updated_at: parse(&m.updated_at),
        }
    }
}

/// List every flag row, sorted by `(name, scope_key)`. Use this to
/// populate the admin panel's "all flags" table.
pub async fn list() -> Result<Vec<FeatureRow>, FrameworkError> {
    let db = DB::connection()?;
    let rows = entity::Entity::find()
        .order_by_asc(entity::Column::Name)
        .order_by_asc(entity::Column::ScopeKey)
        .all(db.inner())
        .await
        .map_err(|e| FrameworkError::database(format!("features list: {e}")))?;
    Ok(rows.into_iter().map(FeatureRow::from).collect())
}

/// Fetch one flag row by name + scope_key. Returns `None` when the
/// row isn't present.
pub async fn get(name: &str, scope_key: &str) -> Result<Option<FeatureRow>, FrameworkError> {
    let db = DB::connection()?;
    let row = entity::Entity::find()
        .filter(entity::Column::Name.eq(name))
        .filter(entity::Column::ScopeKey.eq(scope_key))
        .one(db.inner())
        .await
        .map_err(|e| FrameworkError::database(format!("features get: {e}")))?
        .map(FeatureRow::from);
    Ok(row)
}

/// Create or update a flag. `scope_key = ""` means a global flag;
/// non-empty values like `"user:42"` or `"team:staff"` create a
/// scoped override. Fires [`FeatureUpdated`] after the row is
/// persisted.
///
/// `actor_id` is the user id of the operator performing the change.
/// `None` denotes a system-initiated change (CLI bootstrap, seed,
/// migration).
///
/// Returns the resulting row so callers can re-render the admin
/// list without a follow-up `get`.
pub async fn upsert(
    name: &str,
    scope_key: &str,
    enabled: bool,
    description: Option<String>,
    actor_id: Option<String>,
) -> Result<FeatureRow, FrameworkError> {
    let db = DB::connection()?;
    // Phase 10A T11 — the inner SeaORM Model now stores timestamps as
    // RFC-3339 strings (the `AsDateTime` cast's `Storage` type). Format
    // the chrono value the same way the cast pipeline does so the
    // round-trip back through the FeatureRow conversion below parses
    // cleanly.
    let now = chrono::Utc::now().to_rfc3339();

    let active = entity::ActiveModel {
        name: Set(name.to_string()),
        scope_key: Set(scope_key.to_string()),
        enabled: Set(enabled),
        description: Set(description.clone()),
        updated_by: Set(actor_id.clone()),
        created_at: Set(now.clone()),
        updated_at: Set(now),
        ..Default::default()
    };

    entity::Entity::insert(active)
        .on_conflict(
            OnConflict::columns([entity::Column::Name, entity::Column::ScopeKey])
                .update_columns([
                    entity::Column::Enabled,
                    entity::Column::Description,
                    entity::Column::UpdatedBy,
                    entity::Column::UpdatedAt,
                ])
                .to_owned(),
        )
        .exec(db.inner())
        .await
        .map_err(|e| FrameworkError::database(format!("features upsert: {e}")))?;

    // Re-fetch to return the canonical row (especially the id +
    // created_at, which the insert above doesn't surface).
    let row = get(name, scope_key)
        .await?
        .ok_or_else(|| FrameworkError::internal("features upsert: row missing after insert"))?;

    // Propagate to live evaluators *before* returning — kill-switch
    // semantics require the in-memory snapshot + any cache to reflect
    // the new row by the time the admin call comes back. No-op when
    // no `FeatureSync` is bound.
    crate::features::sync::notify(name, scope_key).await;

    // Fire-and-forget audit event. A listener panic does not roll
    // back the committed insert.
    let _ = crate::events::EventFacade::dispatch(FeatureUpdated {
        name: name.to_string(),
        scope_key: scope_key.to_string(),
        enabled,
        actor_id,
    })
    .await;

    Ok(row)
}

/// Delete a flag row by name + scope_key. Returns `true` when a row
/// was actually removed, `false` when none matched (so admin UIs can
/// distinguish "deleted X" from "nothing to delete"). Fires
/// [`FeatureDeleted`] only on a real deletion.
pub async fn delete(
    name: &str,
    scope_key: &str,
    actor_id: Option<String>,
) -> Result<bool, FrameworkError> {
    let db = DB::connection()?;
    let result = entity::Entity::delete_many()
        .filter(entity::Column::Name.eq(name))
        .filter(entity::Column::ScopeKey.eq(scope_key))
        .exec(db.inner())
        .await
        .map_err(|e| FrameworkError::database(format!("features delete: {e}")))?;

    let deleted = result.rows_affected > 0;
    if deleted {
        // Propagate the deletion: DB snapshots drop the row, caches
        // drop matching entries. After this, `is_enabled!` falls back
        // to the compile-time default for the flag.
        crate::features::sync::notify(name, scope_key).await;

        let _ = crate::events::EventFacade::dispatch(FeatureDeleted {
            name: name.to_string(),
            scope_key: scope_key.to_string(),
            actor_id,
        })
        .await;
    }
    Ok(deleted)
}

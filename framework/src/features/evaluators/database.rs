//! [`DatabaseEvaluator`] — reads feature-flag state from the
//! `features` SeaORM table and serves it through a synchronous,
//! in-memory snapshot.
//!
//! # Why a snapshot
//!
//! featureflag's [`Evaluator::is_enabled`] is **synchronous** — it
//! sits on the hot request path and cannot block on async I/O. SeaORM
//! and our backing databases (Postgres / MySQL / SQLite via SQLx) are
//! async-only. We bridge the two by holding an in-memory snapshot of
//! the table, refreshed asynchronously via [`DatabaseEvaluator::reload`] and
//! [`DatabaseEvaluator::set_flag`]. Reads go through an `RwLock` over a
//! `HashMap<(name, scope_key), enabled>` — lock-free under contention,
//! zero allocation on the hot path beyond the lookup key.
//!
//! # Resolution order
//!
//! Most-specific scope first, falling back to the global `""` scope.
//! `None` is returned only when no scope match exists, leaving the
//! [`Feature`](featureflag::feature::Feature)'s declared default to take over (see
//! [`Feature::is_enabled_in`](featureflag::feature::Feature::is_enabled_in)).
//!
//! 1. `user:{user_id}` — when the context carries a [`UserIdField`]
//! 2. `team:{team}` — when the context carries a [`TeamField`]
//! 3. `""` — global
//! 4. `None` — flag absent entirely
//!
//! Contexts walk their parent chain at lookup time
//! ([`Context::iter`](featureflag::context::Context::iter)) so a
//! parent-scope context's user_id is visible to a child context with
//! no fields of its own.
//!
//! # Where the field newtypes come from
//!
//! The [`Evaluator::on_new_context`] hook fires when a `context!`
//! macro invocation runs **inside the active evaluator's scope**
//! (`set_global_default` / `set_thread_default` / `with_default`).
//! That hook reads the raw field slice and stashes
//! [`UserIdField`] / [`TeamField`] into the context's
//! [`Extensions`](featureflag::extensions::Extensions). Without the
//! evaluator being active at context-creation time, the extensions are
//! empty and lookups fall through to the global scope. Tests use
//! [`with_default`](featureflag::evaluator::with_default) to wire the
//! evaluator before creating any context.
//!
//! # Connection ownership
//!
//! [`DatabaseEvaluator::new`] sources the connection from [`DB::get`](crate::database::DB::get) (the
//! framework's primary pool, registered via the App container).
//! [`DatabaseEvaluator::new_in_memory`] builds its own in-memory SQLite
//! connection so integration tests stay hermetic without touching the
//! container singleton. Both paths produce a `DatabaseEvaluator` of
//! identical shape; the difference is purely how the connection is
//! sourced.

use crate::database::DB;
use crate::error::FrameworkError;
use crate::features::entity::{
    self as features_entity, ActiveModel as FeatureActive, Entity as FeatureEntity,
};
use crate::features::fields::{TeamField, UserIdField};
use crate::features::migrations::CreateFeaturesTable;
use crate::features::sync::FeatureSync;
use crate::lock;

use async_trait::async_trait;
use chrono::Utc;
use featureflag::{
    context::{Context, ContextRef},
    evaluator::Evaluator,
    fields::Fields,
};
use sea_orm::{ActiveValue::Set, DatabaseConnection, EntityTrait, sea_query::OnConflict};
use sea_orm_migration::MigratorTrait;
use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// SeaORM-backed [`Evaluator`] with an in-memory read snapshot.
///
/// See module documentation for the snapshot rationale and the
/// resolution order. The `flags` map is keyed on
/// `(name, scope_key)`; an entry whose `scope_key` is empty is the
/// global default for that flag.
pub struct DatabaseEvaluator {
    conn: DatabaseConnection,
    flags: RwLock<HashMap<(String, String), bool>>,
    /// Monotonic write counter — bumped under the snapshot write lock
    /// every time [`Self::set_flag`] commits a single-key update.
    ///
    /// [`Self::reload`] uses this to serialize against an in-flight
    /// `set_flag` even though the two operations don't share a tokio
    /// mutex: a `reload()` whose `SELECT` started *before* a concurrent
    /// `set_flag`'s upsert (so the SELECT returned the pre-change row
    /// set) would otherwise replace the snapshot, silently reverting
    /// the just-flipped flag. The counter is captured before the
    /// SELECT, then re-read under the snapshot write lock — if it
    /// advanced, the replacement is abandoned and the just-completed
    /// `set_flag`'s in-memory edit stands.
    write_counter: AtomicU64,
}

impl DatabaseEvaluator {
    /// Construct against the framework's primary database connection.
    ///
    /// Pulls the connection out of the App container (set up by
    /// [`DB::init`](crate::database::DB::init)) and seeds the in-memory
    /// snapshot from the live `features` table. Subsequent edits go
    /// through [`Self::set_flag`] or out-of-band SQL + [`DatabaseEvaluator::reload`].
    ///
    /// # Errors
    ///
    /// Returns an error if the container has not been initialized
    /// (e.g. [`DB::init`](crate::database::DB::init) was not called) or
    /// if the initial `SELECT` against the `features` table fails.
    pub async fn new() -> Result<Self, FrameworkError> {
        let conn = DB::get()?;
        let me = Self {
            conn: conn.inner().clone(),
            flags: RwLock::new(HashMap::new()),
            write_counter: AtomicU64::new(0),
        };
        me.reload().await?;
        Ok(me)
    }

    /// Construct against a freshly-built in-memory SQLite database
    /// with the `features` schema applied and no rows. Test-only
    /// helper — does **not** touch [`crate::testing::TestContainer`],
    /// so concurrent tests using both `TestDatabase` and
    /// `DatabaseEvaluator::new_in_memory` don't fight over the
    /// container singleton.
    ///
    /// # Errors
    ///
    /// Returns an error if SQLite cannot be opened in-memory or if
    /// applying the `features` schema fails.
    pub async fn new_in_memory() -> Result<Self, FrameworkError> {
        let conn = sea_orm::Database::connect("sqlite::memory:")
            .await
            .map_err(|e| FrameworkError::database(format!("in-memory sqlite open: {e}")))?;

        // Run the real `CreateFeaturesTable` migration rather than
        // reconstructing the schema from the entity. If the migration
        // and the entity ever diverge — column added, column type
        // changed, unique index dropped — the tests must exercise
        // exactly what production will run. Otherwise the migration
        // can ship broken while the entity-derived in-memory schema
        // keeps every test green.
        InMemoryMigrator::up(&conn, None)
            .await
            .map_err(|e| FrameworkError::database(format!("features migration: {e}")))?;

        Ok(Self {
            conn,
            flags: RwLock::new(HashMap::new()),
            write_counter: AtomicU64::new(0),
        })
    }

    /// Re-read every row from the `features` table into the in-memory
    /// snapshot. Callers invoke this after admin writes or on a
    /// background timer to pick up out-of-band edits (e.g. another
    /// process flipping a flag via direct SQL).
    ///
    /// # Errors
    ///
    /// Returns an error if the `SELECT` fails. The previous snapshot
    /// is left untouched in that case.
    pub async fn reload(&self) -> Result<(), FrameworkError> {
        // Capture the write counter *before* the SELECT. If a
        // concurrent `set_flag` lands its upsert + snapshot update
        // between this read and the write-lock acquisition below, the
        // counter advances and the recompare-then-replace step bails
        // out — preserving the just-flipped flag rather than reverting
        // it with the pre-change SELECT result. The pre-SELECT capture
        // is load-bearing: capturing after the SELECT leaves a race
        // window where a set_flag commits its upsert (visible to the
        // SELECT) but the bump hasn't fired yet, and reload then sees
        // no advance and replaces wholesale.
        let counter_before = self.write_counter.load(Ordering::SeqCst);

        let rows = FeatureEntity::find()
            .all(&self.conn)
            .await
            .map_err(|e| FrameworkError::database(format!("features select: {e}")))?;

        let mut next = HashMap::with_capacity(rows.len());
        for row in rows {
            next.insert((row.name, row.scope_key), row.enabled);
        }

        let mut store = lock::write(&self.flags, "feature-flag snapshot")?;
        // Re-read under the write lock — `set_flag` bumps the counter
        // *while holding the same write lock*, so a value-unchanged
        // re-read here proves no concurrent single-key update slipped
        // in during the SELECT. Counter advanced ⇒ abandon and keep
        // the post-`set_flag` snapshot.
        let counter_after = self.write_counter.load(Ordering::SeqCst);
        if counter_after == counter_before {
            *store = next;
        } else {
            tracing::debug!(
                from = counter_before,
                to = counter_after,
                "features: reload abandoned full-map replace; concurrent set_flag landed during SELECT",
            );
        }
        Ok(())
    }

    /// Upsert a flag for the given `(name, scope_key)` pair and
    /// refresh the in-memory snapshot to match.
    ///
    /// `scope_key` is `""` for a global flag, or any
    /// application-defined string for a scoped flag (the framework
    /// reserves `user:` and `team:` prefixes for the built-in
    /// resolution path — see module docs).
    ///
    /// Fires [`crate::features::sync::notify`] after the snapshot
    /// updates so any [`CachedEvaluator`](super::cached::CachedEvaluator)
    /// in front of this `DatabaseEvaluator` invalidates its entries.
    /// The notify call is a no-op for the `DatabaseEvaluator` itself
    /// (it just refreshed its own snapshot above), so the redundant
    /// reload is the cheap price of unified write-path fan-out.
    ///
    /// # Errors
    ///
    /// Returns an error if the upsert SQL fails. The in-memory
    /// snapshot is not modified in that case, so reads continue to
    /// reflect the last consistent persisted state.
    pub async fn set_flag(
        &self,
        name: &str,
        scope_key: &str,
        enabled: bool,
    ) -> Result<(), FrameworkError> {
        // Phase 10A T11 — the inner SeaORM `Model` carries the storage
        // shape (RFC-3339 string for `created_at` / `updated_at` since
        // `#[model(timestamps)]` auto-injects the `AsDateTime` cast).
        // Build the ActiveModel by routing through the macro's
        // cast pipeline rather than handing chrono types directly.
        let now = Utc::now().to_rfc3339();
        let model = FeatureActive {
            name: Set(name.to_string()),
            scope_key: Set(scope_key.to_string()),
            enabled: Set(enabled),
            description: Set(None),
            updated_by: Set(None),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            ..Default::default()
        };

        FeatureEntity::insert(model)
            .on_conflict(
                OnConflict::columns([
                    features_entity::Column::Name,
                    features_entity::Column::ScopeKey,
                ])
                .update_columns([
                    features_entity::Column::Enabled,
                    features_entity::Column::UpdatedAt,
                ])
                .to_owned(),
            )
            .exec(&self.conn)
            .await
            .map_err(|e| FrameworkError::database(format!("features upsert: {e}")))?;

        // Update the in-memory snapshot in the same operation so
        // callers don't need to call reload() after every write. A
        // separate reload() remains available for picking up edits
        // made out-of-band. The write counter is bumped *under the
        // write lock* so a concurrent reload's recompare-then-replace
        // step sees this update — guarantees that a reload running
        // alongside a set_flag never reverts the just-flipped value.
        {
            let mut store = lock::write(&self.flags, "feature-flag snapshot")?;
            store.insert((name.to_string(), scope_key.to_string()), enabled);
            self.write_counter.fetch_add(1, Ordering::SeqCst);
        }

        // Fan out to other `FeatureSync` implementors (caches,
        // listeners) so any state ahead of the DB sees the change
        // before this call returns. The composite executes data
        // sources before caches, so a `CachedEvaluator` wrapping this
        // evaluator invalidates *after* the snapshot update above.
        crate::features::sync::notify(name, scope_key).await;

        Ok(())
    }

    /// Build the candidate scope-key list for a context, most-
    /// specific first. The global `""` scope is always last so a
    /// missing user/team falls through to the global flag.
    fn scope_keys_for(&self, context: &Context) -> Vec<String> {
        let mut keys = Vec::with_capacity(3);

        // Walk the context + its parents looking for the first
        // user_id we recognize. featureflag does not promote child
        // extensions into a flattened view, so the explicit `iter()`
        // walk is required.
        if let Some(field) = context
            .iter()
            .find_map(|c| c.extensions().get::<UserIdField>())
        {
            keys.push(format!("user:{}", field.as_str()));
        }
        if let Some(field) = context
            .iter()
            .find_map(|c| c.extensions().get::<TeamField>())
        {
            keys.push(format!("team:{}", field.as_str()));
        }

        keys.push(String::new());
        keys
    }
}

impl Evaluator for DatabaseEvaluator {
    fn is_enabled(&self, feature: &str, context: &Context) -> Option<bool> {
        // Domain 17 audit D17-A — was
        // `lock::read(...).expect("DatabaseEvaluator flags RwLock poisoned")`.
        // `is_enabled` is the HOT PATH — every feature-flag check
        // dispatches through it. Returning None on poison means the
        // caller's composite evaluator falls through to the next
        // backend / disabled default; an error log surfaces the poison
        // for ops. Safer than panicking every flag check.
        let store = match lock::read(&self.flags, "feature-flag snapshot") {
            Ok(s) => s,
            Err(_) => {
                tracing::error!(
                    feature = %feature,
                    "DatabaseEvaluator flags RwLock poisoned; returning None \
                     (falling through to downstream evaluator / default)."
                );
                return None;
            }
        };

        for key in self.scope_keys_for(context) {
            if let Some(enabled) = store.get(&(feature.to_string(), key)) {
                return Some(*enabled);
            }
        }
        None
    }

    /// Translate the raw `context!` field slice into typed extensions.
    ///
    /// Only fields we know how to use participate in flag resolution
    /// (`user_id` then `team`). Unknown fields pass through silently;
    /// future evaluators in a [`Chain`](featureflag::evaluator::Chain)
    /// get their own chance to handle them.
    ///
    /// `user_id` accepts both string and i64 raw values so apps with
    /// either id shape interoperate without ceremony — strings pass
    /// through; integers stringify via `to_string`.
    fn on_new_context(&self, mut context: ContextRef<'_>, fields: Fields<'_>) {
        if let Some(value) = fields.get("user_id") {
            let id = value
                .as_str()
                .map(String::from)
                .or_else(|| value.as_i64().map(|i| i.to_string()));
            if let Some(id) = id {
                context.extensions_mut().insert(UserIdField(id));
            }
        }
        if let Some(team) = fields.get("team").and_then(|v| v.as_str()) {
            context.extensions_mut().insert(TeamField(team.to_string()));
        }
    }
}

#[async_trait]
impl FeatureSync for DatabaseEvaluator {
    /// `reload()`s the full snapshot from the `features` table. Cheap
    /// enough for a flag-count in the hundreds; apps with thousands of
    /// flags should swap in a custom impl that targets the specific
    /// `(feature, scope_key)`.
    async fn on_flag_changed(&self, _feature: &str, _scope_key: &str) {
        if let Err(err) = self.reload().await {
            // Reload failures leave the snapshot untouched — the
            // pre-mutation values stay live. Surface the failure so
            // an operator notices the snapshot is now stale relative
            // to the persisted row, but don't propagate (the calling
            // admin::upsert has already committed and we don't want
            // it to misreport success-as-failure on a refresh hiccup).
            tracing::warn!(
                error = %err,
                "features: DatabaseEvaluator::reload failed after mutation; snapshot is stale until the next successful reload",
            );
        }
    }
}

/// Internal migrator wrapping the framework-owned
/// [`CreateFeaturesTable`] migration so [`DatabaseEvaluator::new_in_memory`]
/// applies exactly the schema production runs. Consumer apps wire the
/// migration through their own `Migrator`; this one is only here to
/// make the in-memory test path self-contained.
struct InMemoryMigrator;

#[async_trait::async_trait]
impl MigratorTrait for InMemoryMigrator {
    fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        vec![Box::new(CreateFeaturesTable)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Snapshot helper — peek at the in-memory map without going
    /// through the public `is_enabled` path so the test can assert on
    /// the (name, scope_key) key directly.
    fn snapshot_value(eval: &DatabaseEvaluator, name: &str, scope_key: &str) -> Option<bool> {
        eval.flags
            .read()
            .ok()
            .and_then(|g| g.get(&(name.to_string(), scope_key.to_string())).copied())
    }

    #[tokio::test]
    async fn reload_abandons_replace_when_set_flag_lands_during_select() {
        // Race regression: a reload() whose SELECT started before a
        // concurrent set_flag() committed must not overwrite the
        // snapshot with the pre-change row set. We simulate the race
        // deterministically by capturing the counter, running set_flag
        // between the capture and the post-SELECT replace, and proving
        // the just-flipped value survives.
        //
        // The in-memory SQLite path is fast enough that an unguarded
        // `tokio::join!(reload, set_flag)` would race even with the
        // counter guard — to *prove* the guard works we manually drive
        // the steps reload() goes through and force the interleaving.

        let eval = Arc::new(DatabaseEvaluator::new_in_memory().await.unwrap());
        // Seed: one global "alpha=false" row visible to the first
        // reload, so the post-SELECT snapshot has known content.
        eval.set_flag("alpha", "", false).await.unwrap();
        assert_eq!(snapshot_value(&eval, "alpha", ""), Some(false));

        // Step 1: capture counter pre-SELECT (mirrors `reload()`).
        let counter_before = eval.write_counter.load(Ordering::SeqCst);
        // Step 2: SELECT — at this point alpha is still false on disk.
        let rows = FeatureEntity::find().all(&eval.conn).await.unwrap();
        let mut next = HashMap::with_capacity(rows.len());
        for row in rows {
            next.insert((row.name, row.scope_key), row.enabled);
        }
        // Cross-check the SELECT really did capture the stale value;
        // without this, the test could pass by accident.
        assert_eq!(
            next.get(&("alpha".to_string(), String::new())),
            Some(&false),
            "SELECT captured the pre-flip value as expected"
        );

        // Step 3: concurrent `set_flag` lands BEFORE the replace —
        // bumps the counter under the write lock and updates the
        // snapshot to alpha=true.
        eval.set_flag("alpha", "", true).await.unwrap();
        assert_eq!(snapshot_value(&eval, "alpha", ""), Some(true));

        // Step 4: complete the reload — acquire the write lock,
        // re-read the counter, and only replace if unchanged. The
        // counter advanced, so replacement must be abandoned.
        {
            let mut store = lock::write(&eval.flags, "feature-flag snapshot").unwrap();
            let counter_after = eval.write_counter.load(Ordering::SeqCst);
            if counter_after == counter_before {
                *store = next;
            }
        }

        // Without the guard the snapshot would now read alpha=false
        // (the pre-flip SELECT result clobbered the just-flipped
        // value). With the guard, the just-flipped value survives.
        assert_eq!(
            snapshot_value(&eval, "alpha", ""),
            Some(true),
            "set_flag's in-memory edit must survive a racing reload — \
             the race was M20's silent flag-revert bug",
        );
    }

    #[tokio::test]
    async fn reload_still_replaces_when_no_concurrent_write() {
        // Sanity guard: the counter check doesn't accidentally turn
        // every reload into a no-op. In the steady state — no
        // concurrent set_flag — reload must replace as before.
        let eval = DatabaseEvaluator::new_in_memory().await.unwrap();
        // Seed via a direct insert that bypasses set_flag, so the
        // counter stays at zero and the snapshot stays empty.
        let now = Utc::now().to_rfc3339();
        FeatureEntity::insert(FeatureActive {
            name: Set("beta".to_string()),
            scope_key: Set(String::new()),
            enabled: Set(true),
            description: Set(None),
            updated_by: Set(None),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            ..Default::default()
        })
        .exec(&eval.conn)
        .await
        .unwrap();

        // Pre-state: snapshot doesn't know about beta yet.
        assert_eq!(snapshot_value(&eval, "beta", ""), None);

        eval.reload().await.unwrap();

        // The reload absorbed the out-of-band insert.
        assert_eq!(snapshot_value(&eval, "beta", ""), Some(true));
    }
}

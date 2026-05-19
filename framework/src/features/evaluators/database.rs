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
//! the table, refreshed asynchronously via [`Self::reload`] and
//! [`Self::set_flag`]. Reads go through an `RwLock` over a
//! `HashMap<(name, scope_key), enabled>` — lock-free under contention,
//! zero allocation on the hot path beyond the lookup key.
//!
//! # Resolution order
//!
//! Most-specific scope first, falling back to the global `""` scope.
//! `None` is returned only when no scope match exists, leaving the
//! [`Feature`]'s declared default to take over (see
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
//! [`Self::new`] sources the connection from [`DB::get`] (the
//! framework's primary pool, registered via the App container).
//! [`Self::new_in_memory`] builds its own in-memory SQLite
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

use chrono::Utc;
use featureflag::{
    context::{Context, ContextRef},
    evaluator::Evaluator,
    fields::Fields,
};
use sea_orm::{
    sea_query::OnConflict, ActiveValue::Set, DatabaseConnection, EntityTrait,
};
use sea_orm_migration::MigratorTrait;
use std::collections::HashMap;
use std::sync::RwLock;

/// SeaORM-backed [`Evaluator`] with an in-memory read snapshot.
///
/// See module documentation for the snapshot rationale and the
/// resolution order. The `flags` map is keyed on
/// `(name, scope_key)`; an entry whose `scope_key` is empty is the
/// global default for that flag.
pub struct DatabaseEvaluator {
    conn: DatabaseConnection,
    flags: RwLock<HashMap<(String, String), bool>>,
}

impl DatabaseEvaluator {
    /// Construct against the framework's primary database connection.
    ///
    /// Pulls the connection out of the App container (set up by
    /// [`DB::init`](crate::database::DB::init)) and seeds the in-memory
    /// snapshot from the live `features` table. Subsequent edits go
    /// through [`Self::set_flag`] or out-of-band SQL + [`Self::reload`].
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
        let rows = FeatureEntity::find()
            .all(&self.conn)
            .await
            .map_err(|e| FrameworkError::database(format!("features select: {e}")))?;

        let mut next = HashMap::with_capacity(rows.len());
        for row in rows {
            next.insert((row.name, row.scope_key), row.enabled);
        }

        let mut store = self
            .flags
            .write()
            .expect("DatabaseEvaluator flags RwLock poisoned");
        *store = next;
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
        let now = Utc::now();
        let model = FeatureActive {
            name: Set(name.to_string()),
            scope_key: Set(scope_key.to_string()),
            enabled: Set(enabled),
            description: Set(None),
            updated_by: Set(None),
            created_at: Set(now),
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
        // made out-of-band.
        let mut store = self
            .flags
            .write()
            .expect("DatabaseEvaluator flags RwLock poisoned");
        store.insert((name.to_string(), scope_key.to_string()), enabled);
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
        if let Some(UserIdField(id)) = context
            .iter()
            .find_map(|c| c.extensions().get::<UserIdField>())
        {
            keys.push(format!("user:{}", id));
        }
        if let Some(TeamField(team)) = context
            .iter()
            .find_map(|c| c.extensions().get::<TeamField>())
        {
            keys.push(format!("team:{}", team));
        }

        keys.push(String::new());
        keys
    }
}

impl Evaluator for DatabaseEvaluator {
    fn is_enabled(&self, feature: &str, context: &Context) -> Option<bool> {
        let store = self
            .flags
            .read()
            .expect("DatabaseEvaluator flags RwLock poisoned");

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
    fn on_new_context(&self, mut context: ContextRef<'_>, fields: Fields<'_>) {
        if let Some(id) = fields.get("user_id").and_then(|v| v.as_i64()) {
            context.extensions_mut().insert(UserIdField(id));
        }
        if let Some(team) = fields.get("team").and_then(|v| v.as_str()) {
            context
                .extensions_mut()
                .insert(TeamField(team.to_string()));
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

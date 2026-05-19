//! Compile-time model registry.
//!
//! Every `#[suprnova::model]` expansion emits `inventory::submit!`
//! with a `ModelEntry` describing the model. Downstream consumers
//! (Phase 8 Admin, `model:prune`, future tooling) walk the registry
//! at boot via `models()` or look up by table via
//! `find_model_by_table`.

/// One entry per `#[suprnova::model]`-annotated struct, emitted at
/// compile time. Fields are all `&'static` so the entry is a const.
#[derive(Debug, Clone, Copy)]
pub struct ModelEntry {
    /// The Rust type name (e.g. `"User"`).
    pub type_name: &'static str,
    /// The SQL table name (e.g. `"users"`).
    pub table: &'static str,
    /// The fully-qualified module path where the model lives.
    pub module_path: &'static str,
    /// The primary key column name (default `"id"`).
    pub primary_key: &'static str,
}

inventory::collect!(ModelEntry);

/// Iterator over every registered model in the binary. Order is
/// link-time; do not depend on it.
pub fn models() -> impl Iterator<Item = &'static ModelEntry> {
    inventory::iter::<ModelEntry>()
}

/// Find a model by its SQL table name. `None` if no model registers
/// that table.
pub fn find_model_by_table(table: &str) -> Option<&'static ModelEntry> {
    models().find(|m| m.table == table)
}

//! Compile-time model registry.
//!
//! Every `#[suprnova::model]` expansion emits `inventory::submit!`
//! with a `ModelEntry` describing the model. Downstream consumers
//! (Phase 8 Admin, `model:prune`, future tooling) walk the registry
//! at boot via `models()` or look up by table via
//! `find_model_by_table`.

use crate::error::FrameworkError;

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
///
/// Errors when two or more entries register the same table name from
/// different modules — `inventory` iteration order is link-time and
/// not deterministic, so silently returning whichever entry happened
/// to be discovered first would route table-keyed operations
/// (admin lookup, ad-hoc tooling) to a non-deterministic model. The
/// error names both colliding modules so the dev can rename or alias
/// one. Duplicate registrations of the SAME `(table, module_path,
/// type_name)` triple (which happens on macro re-expansion within
/// the same crate) are NOT a collision — they reference the same
/// logical model.
pub fn find_model_by_table(table: &str) -> Result<Option<&'static ModelEntry>, FrameworkError> {
    find_entry_by_table(models(), table)
}

/// Pure version of [`find_model_by_table`] over any iterator of
/// `&ModelEntry`. Exposed `pub(crate)` so the registry's tests can
/// exercise duplicate detection without polluting the process-wide
/// inventory.
pub(crate) fn find_entry_by_table<'a, I>(
    entries: I,
    table: &str,
) -> Result<Option<&'a ModelEntry>, FrameworkError>
where
    I: IntoIterator<Item = &'a ModelEntry>,
{
    let mut hit: Option<&ModelEntry> = None;
    for entry in entries {
        if entry.table != table {
            continue;
        }
        match hit {
            None => hit = Some(entry),
            Some(prev) => {
                // Same (table, module_path, type_name) = same logical
                // model re-registered (macro re-expansion in the same
                // crate). Distinct module paths = a real collision.
                if prev.module_path != entry.module_path || prev.type_name != entry.type_name {
                    return Err(FrameworkError::internal(format!(
                        "model registry: table `{table}` is registered by both \
                         `{}::{}` and `{}::{}` — rename one model or alias its \
                         table via `#[model(table = \"...\")]` so table-keyed \
                         lookups stay deterministic",
                        prev.module_path, prev.type_name, entry.module_path, entry.type_name,
                    )));
                }
            }
        }
    }
    Ok(hit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        type_name: &'static str,
        table: &'static str,
        module_path: &'static str,
    ) -> ModelEntry {
        ModelEntry {
            type_name,
            table,
            module_path,
            primary_key: "id",
        }
    }

    #[test]
    fn find_entry_by_table_returns_single_match() {
        let entries = vec![
            entry("User", "users", "crate::users"),
            entry("Post", "posts", "crate::posts"),
        ];
        let got = find_entry_by_table(&entries, "users").unwrap();
        assert_eq!(got.unwrap().type_name, "User");
    }

    #[test]
    fn find_entry_by_table_returns_none_when_missing() {
        let entries = vec![entry("User", "users", "crate::users")];
        let got = find_entry_by_table(&entries, "no_such_table").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn find_entry_by_table_errors_on_cross_module_collision() {
        // Two modules each register a model under the same table.
        // Link-time order is nondeterministic; the registry must
        // refuse to pick a winner.
        let entries = vec![
            entry("Session", "sessions", "auth::sessions"),
            entry("Session", "sessions", "billing::sessions"),
        ];
        let err = find_entry_by_table(&entries, "sessions").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("auth::sessions"), "msg = {msg}");
        assert!(msg.contains("billing::sessions"), "msg = {msg}");
    }

    #[test]
    fn find_entry_by_table_ignores_self_dup_within_same_module() {
        // Same (type_name, table, module_path) triple appearing twice
        // is not a collision: macro re-expansion under the same path.
        let entries = vec![
            entry("User", "users", "crate::users"),
            entry("User", "users", "crate::users"),
        ];
        let got = find_entry_by_table(&entries, "users").unwrap();
        assert!(got.is_some());
    }
}

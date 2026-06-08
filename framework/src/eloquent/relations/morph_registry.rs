//! Compile-time morph type registry.
//!
//! Every struct annotated `#[suprnova::model(morph_type = "...")]` emits
//! one [`MorphTypeEntry`] via `inventory::submit!`. The registry provides
//! string-to-`TypeId` and `TypeId`-to-string lookups consumed by the
//! per-family morph enum (T6) and the morph m2m loader (T7), and walked
//! by Phase 8 (Admin) to render the polymorphic relation graph.
//!
//! Structurally identical to the [`ModelEntry`](crate::eloquent::ModelEntry)
//! registry — opt-in: only structs that actually declare a
//! `morph_type = "..."` attribute appear here. Plain `#[suprnova::model]`
//! structs without that attribute are deliberately absent (the
//! `morph_type_not_registered_for_non_morph_models` test pins this).

use std::any::TypeId;
use std::collections::HashMap;
use std::sync::OnceLock;

/// One entry per `#[suprnova::model(morph_type = "...")]`-annotated
/// struct, emitted at compile time.
///
/// All non-fn fields are `&'static` so the entry is a const initialiser
/// (a requirement of `inventory::submit!`). The `type_id` field is a
/// `fn() -> TypeId` rather than a stored `TypeId` because `TypeId` is
/// not constructible in a const context on stable Rust — wrapping
/// `TypeId::of::<T>` (itself a `const fn`) keeps the entry `Copy` and
/// the lookup is one indirection.
#[derive(Debug, Clone, Copy)]
pub struct MorphTypeEntry {
    /// String stored in the polymorphic table's `*_type` column (e.g.
    /// `"post"`, `"video"`). Matches the value of `morph_type = "..."`
    /// on the model's `#[suprnova::model]` attribute.
    pub morph_type: &'static str,
    /// The Rust type name (`"Post"`).
    pub type_name: &'static str,
    /// The SQL table name (`"posts"`).
    pub table: &'static str,
    /// `TypeId::of::<T>` thunk — wrapped as `fn() -> TypeId` because
    /// `TypeId` itself isn't a stable const, so it can't be stored
    /// directly in an `inventory::submit!` constant.
    pub type_id: fn() -> TypeId,
}

inventory::collect!(MorphTypeEntry);

/// Iterator over every registered morph type in the binary. Order is
/// link-time; do not depend on it.
pub fn morph_types() -> impl Iterator<Item = &'static MorphTypeEntry> {
    inventory::iter::<MorphTypeEntry>()
}

/// Morph-string index built once on first lookup. Inventory is static
/// over the lifetime of the binary so the index never needs to grow;
/// `OnceLock` gives us a free single-init slot without dragging in
/// `once_cell` for this one site.
fn morph_by_name() -> &'static HashMap<&'static str, &'static MorphTypeEntry> {
    static IDX: OnceLock<HashMap<&'static str, &'static MorphTypeEntry>> = OnceLock::new();
    IDX.get_or_init(|| morph_types().map(|e| (e.morph_type, e)).collect())
}

/// Morph-TypeId index (reverse lookup). Same shape as `morph_by_name`;
/// we materialise the `TypeId` values during init so the lookup itself
/// doesn't call the per-entry thunk.
fn morph_by_type_id() -> &'static HashMap<TypeId, &'static MorphTypeEntry> {
    static IDX: OnceLock<HashMap<TypeId, &'static MorphTypeEntry>> = OnceLock::new();
    IDX.get_or_init(|| morph_types().map(|e| ((e.type_id)(), e)).collect())
}

/// Find a morph type by its stored `*_type` string. `None` if no model
/// registers that string — distinguishes "registered but not in this
/// MorphTo's target list" from "completely unknown" at runtime. O(1)
/// after the first lookup builds the index; linear scans previously
/// scaled with the number of `#[suprnova::model(morph_type)]` decls,
/// which matters once a polymorphic table fans out across many target
/// types and the loader runs per-row.
pub fn find_morph_type(name: &str) -> Option<&'static MorphTypeEntry> {
    morph_by_name().get(name).copied()
}

/// Reverse lookup: find the registered morph type for a Rust `TypeId`.
/// Useful for debug / admin tooling that wants to render the morph_type
/// string for a known concrete type. O(1) after first init.
pub fn find_morph_type_by_id(id: TypeId) -> Option<&'static MorphTypeEntry> {
    morph_by_type_id().get(&id).copied()
}

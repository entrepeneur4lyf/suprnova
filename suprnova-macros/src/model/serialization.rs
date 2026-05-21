//! Phase 10C T5b + T6 — emit per-model serialization helpers.
//!
//! T5b ships `field_value(&self, name: &str) -> Option<Value>` — the
//! macro-emitted accessor that powers `Collection<M>`'s string-keyed
//! methods (`pluck("col")`, `group_by("col")`, `sort_by("col")`,
//! `where_eq("col", v)`, `sum::<T>("col")`, ...). One match arm per
//! declared column field; unknown names return `None` (calling code
//! silently skips those rows).
//!
//! T6 extends this file with `to_array` / `to_json` overrides that
//! honour `hidden = [...]` / `visible = [...]` / `appends = [...]`.
//!
//! ## Why a separate module?
//!
//! `derive_eloquent.rs` already runs at ~50 KB. Serialisation glue is
//! its own concern with its own lifetime (T6 will extend it) and
//! deserves a dedicated home, parallel to `events.rs` / `observers.rs`.
//!
//! ## Field filtering
//!
//! The macro auto-injects `__eager: EagerLoadCache` and
//! `__pivot: Option<Arc<dyn Any + ...>>` on every model (see
//! `model.rs::inject_eager_pivot_fields`). Those are `#[serde(skip)]`
//! scratch state, not columns — so callers MUST pass the same
//! pre-filtered `field_idents` slice that `derive_eloquent` uses for
//! the per-column code paths. The emit helper itself doesn't filter;
//! it consumes whatever it's given.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Ident;

/// Emit the `field_value(&self, name: &str) -> Option<Value>` method
/// body that goes inside the per-model
/// `impl ::suprnova::eloquent::Model for #struct_ident` block.
///
/// The implementation matches `name` against every column field's
/// stringified ident and calls `serde_json::to_value(&self.<field>)`
/// for the hit. Serialisation errors lower to `None` (matching the
/// "missing/unknown column" branch). Callers that need to distinguish
/// "field absent" from "field serialisation failed" should reach for
/// `serde_json::to_value` directly.
///
/// `idents` is the same slice `derive_eloquent` builds for per-column
/// code paths — already filtered to exclude the auto-injected
/// `__eager` / `__pivot` runtime-state fields.
pub fn emit_field_value(idents: &[Ident]) -> TokenStream {
    let arms = idents.iter().map(|ident| {
        let name = ident.to_string();
        quote! {
            #name => ::suprnova::serde_json::to_value(&self.#ident).ok(),
        }
    });

    quote! {
        fn field_value(&self, name: &str) -> ::core::option::Option<::suprnova::serde_json::Value> {
            match name {
                #(#arms)*
                _ => ::core::option::Option::None,
            }
        }
    }
}

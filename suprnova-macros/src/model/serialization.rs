//! Phase 10C T5b + T6 — emit per-model serialization helpers.
//!
//! T5b ships `field_value(&self, name: &str) -> Option<Value>` — the
//! macro-emitted accessor that powers `Collection<M>`'s string-keyed
//! methods (`pluck("col")`, `group_by("col")`, `sort_by("col")`,
//! `where_eq("col", v)`, `sum::<T>("col")`, ...). One match arm per
//! declared column field; unknown names return `None` (calling code
//! silently skips those rows).
//!
//! T6 extends this file with two more emitters:
//!
//! - [`emit_to_array_override`] — overrides
//!   [`Model::to_array`](crate::eloquent::Model::to_array) when the
//!   model declares `hidden = [...]`, `visible = [...]`, or
//!   `appends = [...]`. The override unconditionally strips
//!   `__eager` / `__pivot` (Phase 10B P6 contract), applies the
//!   visible whitelist + hidden denylist, then injects appends.
//! - [`emit_append_accessor_dispatch`] — overrides
//!   [`Model::__append_accessor`](crate::eloquent::Model::__append_accessor)
//!   with a `match` block dispatching each declared name to the user's
//!   `#[suprnova::accessor]`-tagged method.
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

/// Phase 10C T6 — emit a `to_array` override when the model declares
/// any of `hidden = [...]`, `visible = [...]`, or `appends = [...]`.
/// When all three are empty, returns an empty token stream so the
/// trait default (which already strips `__eager` / `__pivot`) wins.
///
/// The emitted body:
///
/// 1. Serialise `self` via `serde_json::to_value` (Serialize::serialize
///    paths run all the field-level casts; `__eager` / `__pivot` are
///    `#[serde(skip)]` so they shouldn't be in the map to begin with).
/// 2. Unconditionally remove `__eager` / `__pivot` keys — load-bearing
///    P6 contract; the strip survives even if a user later adds
///    `__eager` / `__pivot` to a `visible = [...]` whitelist.
/// 3. Apply `visible` as a whitelist when non-empty: every key NOT in
///    the list is dropped.
/// 4. Apply `hidden` as a denylist: every listed key is removed from
///    the surviving keys.
/// 5. Inject appends: for each name in `appends`, call
///    `self.__append_accessor(name)` and insert the result. Appends
///    run last so they always show up (matches Laravel — `$appends`
///    always serialises, even when sharing a name with a hidden
///    field).
pub fn emit_to_array_override(
    hidden: &[String],
    visible: Option<&[String]>,
    appends: &[String],
) -> TokenStream {
    let visible_is_set = visible.is_some_and(|v| !v.is_empty());
    let visible_slice: &[String] = visible.unwrap_or(&[]);
    if hidden.is_empty() && !visible_is_set && appends.is_empty() {
        return TokenStream::new();
    }

    let hidden_lits = hidden.iter().map(|s| quote! { #s });
    let visible_lits = visible_slice.iter().map(|s| quote! { #s });
    let append_lits = appends.iter().map(|s| quote! { #s });

    // The visible filter is gated by `!VISIBLE.is_empty()` so the same
    // body works whether or not the user declared `visible = [...]`.
    // When the attribute is omitted (None), the emitter passes an
    // empty slice and the `.is_empty()` guard skips the retain pass.
    quote! {
        fn to_array(&self) -> ::suprnova::serde_json::Value {
            let mut value = ::suprnova::serde_json::to_value(self)
                .unwrap_or(::suprnova::serde_json::Value::Null);
            let map = match value.as_object_mut() {
                ::core::option::Option::Some(m) => m,
                ::core::option::Option::None => return value,
            };

            // Phase 10B P6 contract: __eager + __pivot stay out of
            // serialisation. They carry #[serde(skip)] on the struct,
            // but we re-strip here so the contract survives any
            // future hand-rolled Serialize impl.
            map.remove("__eager");
            map.remove("__pivot");

            // Visible whitelist: keep only listed keys when non-empty.
            const __SUPRNOVA_VISIBLE: &[&str] = &[ #(#visible_lits),* ];
            if !__SUPRNOVA_VISIBLE.is_empty() {
                map.retain(|k, _| {
                    __SUPRNOVA_VISIBLE.iter().any(|v| *v == k.as_str())
                });
            }

            // Hidden denylist: remove every listed key from the
            // surviving map.
            const __SUPRNOVA_HIDDEN: &[&str] = &[ #(#hidden_lits),* ];
            for h in __SUPRNOVA_HIDDEN.iter() {
                map.remove(*h);
            }

            // Appends: invoke #[accessor]-tagged methods AFTER the
            // filters run. Appends always show up — Laravel parity.
            const __SUPRNOVA_APPENDS: &[&str] = &[ #(#append_lits),* ];
            for a in __SUPRNOVA_APPENDS.iter() {
                if let ::core::option::Option::Some(v) =
                    <Self as ::suprnova::eloquent::Model>::__append_accessor(self, a)
                {
                    map.insert((*a).to_string(), v);
                }
            }

            value
        }
    }
}

/// Phase 10C T6 — emit the `__append_accessor` override when the
/// model declares `appends = [...]`. The body is a `match` that
/// dispatches each declared name to the corresponding method on the
/// user's `impl #struct`, calling it and serialising the result.
///
/// Empty `appends` returns no tokens so the trait default (returning
/// `None` for every name) wins.
///
/// Each accessor name in `appends` is parsed as a Rust ident. The
/// macro doesn't validate that the corresponding method exists on the
/// user's `impl` — a missing method surfaces as a clear compiler
/// error pointing at the dispatcher's `self.<name>()` call site,
/// which is the right shape for the user to fix.
pub fn emit_append_accessor_dispatch(appends: &[String]) -> TokenStream {
    if appends.is_empty() {
        return TokenStream::new();
    }

    let arms = appends.iter().map(|name| {
        let method: syn::Ident =
            syn::parse_str(name).expect("accessor name parses as a Rust ident");
        let name_str = name.clone();
        quote! {
            #name_str => ::core::option::Option::Some(
                ::suprnova::serde_json::to_value(self.#method())
                    .unwrap_or(::suprnova::serde_json::Value::Null),
            ),
        }
    });

    quote! {
        fn __append_accessor(
            &self,
            name: &str,
        ) -> ::core::option::Option<::suprnova::serde_json::Value> {
            match name {
                #(#arms)*
                _ => ::core::option::Option::None,
            }
        }
    }
}

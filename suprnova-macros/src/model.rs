//! `#[suprnova::model]` attribute macro.
//!
//! Phase 10A core: table, primary key, SeaORM Entity/Model/ActiveModel,
//! Column enum, inventory submission, and the `EloquentModel` marker
//! impl.
//!
//! Later tasks in Phase 10A extend the dispatcher:
//! - Task 4 — Model trait CRUD methods (fills derive_eloquent.rs)
//! - Task 6 — Fillable / Guarded
//! - Task 7a-c — Casts (casts.rs slot wires from_storage / to_storage)
//! - Task 8 — Accessors / mutators (function-level macros, separate file)
//! - Task 9 — Timestamps
//! - Task 10 — Soft deletes

use proc_macro2::TokenStream;
use quote::quote;
use syn::Result;

mod casts;
mod columns;
mod derive_eloquent;
mod derive_seaorm;
mod parse;
pub mod prunable;
mod relations;

use parse::ModelInput;

pub fn expand(attr: TokenStream, item: TokenStream) -> Result<TokenStream> {
    let mut input = ModelInput::parse(attr, item)?;

    // Inject derives on the user's struct itself. Without these, the
    // user-facing API breaks:
    //   - Serialize → required by `to_json` / `to_array` (Task 8)
    //   - Deserialize → required by `fill` / `from_attrs_unsaved` /
    //     runtime cast pipeline (Tasks 4, 7b, 8)
    //   - Clone → required by `replicate`, in-place updates, test code
    //     like `let mut handle = original.clone()` (Tasks 4, 9)
    //   - Debug → required by assertion failure messages in tests
    //
    // The macro never sees the user's existing derives, so we always
    // add ours; conflicts ("Clone is already derived") produce a clear
    // compiler error pointing to the user struct, which is acceptable
    // because no Suprnova model should be deriving these manually.
    let injected: syn::Attribute = syn::parse_quote! {
        #[derive(::core::clone::Clone, ::core::fmt::Debug, ::serde::Serialize, ::serde::Deserialize)]
    };
    input.item.attrs.push(injected);

    // Phase 10B T1 — auto-inject `__eager: EagerLoadCache` and
    // `__pivot: Option<Arc<dyn Any + Send + Sync>>` on every model.
    //
    // These are runtime scratch state: the eager cache stores rows
    // loaded by `with([...])`, and the pivot slot stores the pivot
    // row attached by a `BelongsToMany` loader (T4). Both are
    // `#[serde(skip)]` so they don't surface in JSON, and they're
    // filtered out of `derive_seaorm` + `columns` + the per-column
    // `From<inner::Model>` / `Default` / `replicate_with` paths in
    // `derive_eloquent` (which materialise the user struct).
    //
    // The fields are `pub` not because users should touch them
    // directly, but because the test smoke (`framework/tests/eloquent_macro_smoke_relations.rs`)
    // constructs the struct literal explicitly to verify the fields
    // exist. Real model construction goes through
    // `From<inner::Model>` or `Self::default()`, which initialise
    // the slots via `Default::default()`.
    inject_eager_pivot_fields(&mut input.item)?;

    let seaorm = derive_seaorm::emit(&input)?;
    let columns = columns::emit(&input)?;
    let eloquent = derive_eloquent::emit(&input)?;
    let relations_tokens = relations::emit(&input)?;
    let registry = emit_registry(&input);
    let morph_registry = emit_morph_registry(&input);

    let module_name = input.module_name();
    let struct_def = input.struct_def();

    Ok(quote! {
        #struct_def

        #[allow(non_snake_case, non_camel_case_types)]
        pub mod #module_name {
            // SeaORM's `DeriveEntityModel` macro internally references
            // `EnumIter`, `DerivePrimaryKey`, `PrimaryKeyTrait` (and a
            // handful of other types) by unqualified name, so we pull
            // its prelude into the inner module scope. This is the same
            // pattern the `cargo run --bin generate-entities` (CLI-driven
            // db:sync) emits in `app/src/models/entities/*.rs`.
            use ::suprnova::sea_orm::entity::prelude::*;
            use super::*;

            #seaorm
            #columns
        }

        #eloquent
        #relations_tokens
        #registry
        #morph_registry
    })
}

/// Append `__eager: EagerLoadCache` and
/// `__pivot: Option<Arc<dyn Any + Send + Sync>>` to the user's struct
/// definition. Both fields carry `#[serde(skip)]` so they're not part
/// of JSON serialization, and they're filtered out of the inner
/// SeaORM Model + the per-column macro code paths.
///
/// Errors only if the input struct isn't `Fields::Named` — which
/// `derive_seaorm` already rejects with a clear message earlier, so
/// in practice this is unreachable on the happy path.
fn inject_eager_pivot_fields(item: &mut syn::ItemStruct) -> Result<()> {
    let named = match &mut item.fields {
        syn::Fields::Named(named) => named,
        _ => {
            return Err(syn::Error::new_spanned(
                &item.ident,
                "#[model] only supports structs with named fields",
            ));
        }
    };

    // Idempotency guard. The macro shouldn't run twice on the same
    // struct, but if a future refactor reorders the pipeline, double-
    // injecting would produce a duplicate-field rustc error that
    // doesn't point at the underlying cause. Skip when the slots are
    // already present.
    let has_eager = named
        .named
        .iter()
        .any(|f| f.ident.as_ref().is_some_and(|i| i == "__eager"));
    let has_pivot = named
        .named
        .iter()
        .any(|f| f.ident.as_ref().is_some_and(|i| i == "__pivot"));

    if !has_eager {
        let f: syn::Field = syn::Field::parse_named.parse2(quote! {
            /// Eager-load cache. Populated by `Builder::with([...])`
            /// and read by the macro-emitted `<rel>_loaded()` /
            /// `<rel>_count()` accessors. Empty by default.
            #[serde(skip)]
            #[allow(non_snake_case)]
            #[doc(hidden)]
            pub __eager: ::suprnova::EagerLoadCache
        })?;
        named.named.push(f);
    }

    if !has_pivot {
        let f: syn::Field = syn::Field::parse_named.parse2(quote! {
            /// Per-row pivot context. Filled by
            /// [`BelongsToMany`](::suprnova::eloquent::relations) loaders
            /// (T4) and read via [`pivot::<P>()`](Self::pivot). `None`
            /// by default.
            #[serde(skip)]
            #[allow(non_snake_case)]
            #[doc(hidden)]
            pub __pivot: ::core::option::Option<
                ::std::sync::Arc<dyn ::std::any::Any + ::core::marker::Send + ::core::marker::Sync>,
            >
        })?;
        named.named.push(f);
    }

    Ok(())
}

// Import the syn parser trait used by `inject_eager_pivot_fields`.
// Kept scoped to this module so it doesn't shadow `syn::parse` calls
// elsewhere.
use syn::parse::Parser as _;

fn emit_registry(input: &ModelInput) -> TokenStream {
    let type_name = input.struct_name_str();
    let table = &input.table;
    let primary_key = &input.primary_key;

    quote! {
        ::suprnova::inventory::submit! {
            ::suprnova::ModelEntry {
                type_name: #type_name,
                table: #table,
                module_path: module_path!(),
                primary_key: #primary_key,
            }
        }
    }
}

/// Phase 10B T8 — emit one `inventory::submit!(MorphTypeEntry { ... })`
/// per `#[suprnova::model(morph_type = "...")]` struct. Models without
/// the attribute return an empty token stream and never enter the
/// registry (pinned by the
/// `morph_type_not_registered_for_non_morph_models` integration test).
///
/// `TypeId::of::<T>` is `const fn` so it's stored as a `fn() -> TypeId`
/// thunk; the `MorphTypeEntry` itself stays `Copy` and works inside
/// `inventory::submit!`'s const-initialiser slot.
fn emit_morph_registry(input: &ModelInput) -> TokenStream {
    let morph_type = match input.morph_type.as_ref() {
        Some(s) => s,
        None => return TokenStream::new(),
    };
    let type_name = input.struct_name_str();
    let table = &input.table;
    let struct_ident = &input.item.ident;

    quote! {
        ::suprnova::inventory::submit! {
            ::suprnova::MorphTypeEntry {
                morph_type: #morph_type,
                type_name: #type_name,
                table: #table,
                type_id: ::std::any::TypeId::of::<#struct_ident>,
            }
        }
    }
}

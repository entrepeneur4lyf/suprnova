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

    let seaorm = derive_seaorm::emit(&input)?;
    let columns = columns::emit(&input)?;
    let eloquent = derive_eloquent::emit(&input)?;
    let registry = emit_registry(&input);

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
        #registry
    })
}

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

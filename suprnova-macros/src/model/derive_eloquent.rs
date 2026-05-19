//! Emits Eloquent trait impls. Task 3 ships just the `EloquentModel`
//! marker — enough to register the type as a Suprnova model and to
//! bridge the user's struct name (e.g. `User`) to the inner-module
//! SeaORM `Entity` / `Column` types (`user::Entity`, `user::Column`).
//!
//! Tasks 4 / 5 / 6 / 7a-c / 8 / 9 / 10 each extend this file with
//! their slice of the generated impl (CRUD lifecycle, Builder
//! constructor, fillable, casts, accessors / mutators, timestamps,
//! soft deletes).

use proc_macro2::TokenStream;
use quote::quote;
use syn::Result;

use super::parse::ModelInput;

pub fn emit(input: &ModelInput) -> Result<TokenStream> {
    let struct_ident = &input.item.ident;
    let module_name = input.module_name();
    let table = &input.table;

    Ok(quote! {
        impl ::suprnova::eloquent::EloquentModel for #struct_ident {
            type Entity = #module_name::Entity;
            type Column = #module_name::Column;
            // Emit the literal table string. `EntityName::table_name`
            // is a runtime method in SeaORM 1.1, not `const fn`, so
            // we can't call it in a `const TABLE` initialiser. The
            // string the parser captured into `input.table` is the
            // same value SeaORM hands back at runtime — the macro is
            // the single source of truth for the table name.
            const TABLE: &'static str = #table;
        }
    })
}

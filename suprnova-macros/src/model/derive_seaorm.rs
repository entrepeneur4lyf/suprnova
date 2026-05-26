//! Emits the SeaORM Entity / Model / ActiveModel triple. The user's
//! struct *becomes* the SeaORM `Model` via `DeriveEntityModel`, placed
//! inside the per-model inner module (`user::Model`, `smoke_user::Model`,
//! etc.). The user's original struct stays at parent scope under its
//! camel-case name (e.g. `User`); the Eloquent emit (`derive_eloquent.rs`)
//! bridges the two via the `EloquentModel` trait.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Result;

use super::parse::ModelInput;

pub fn emit(input: &ModelInput) -> Result<TokenStream> {
    let table = &input.table;
    let primary_key = &input.primary_key;
    let auto_increment = input.auto_increment;

    // Filter the struct's fields into `(field_ident, ty, is_pk)`.
    let fields = match &input.item.fields {
        syn::Fields::Named(named) => &named.named,
        _ => {
            return Err(syn::Error::new_spanned(
                &input.item.ident,
                "#[model] only supports structs with named fields",
            ));
        }
    };

    let mut pk_field_idents = Vec::new();
    let mut field_decls = Vec::new();
    // T7a — per-cast-field type aliases. SeaORM 1.1's `DeriveEntityModel`
    // macro re-parses field types when wiring `ColumnTrait::def`, and
    // its parser mangles qualified `<T as Trait>::Storage` projections
    // into a single identifier (e.g. `AsBoolas::path::Cast`). To dodge
    // that bug we emit a type alias per cast field at module scope and
    // reference the alias in the field declaration — a plain
    // identifier is one token, immune to the re-parse pass.
    let mut storage_aliases = Vec::new();

    for f in fields {
        let ident = f.ident.as_ref().expect("named field");
        // Phase 10B T1 — skip the eager/pivot fields the relations
        // emitter auto-injects onto the user struct. They're runtime
        // scratch space (an `EagerLoadCache` + an opaque pivot box),
        // not database columns; the inner SeaORM Model must not see
        // them, and `From<Model> for UserStruct` constructs them via
        // `Default::default()` (derive_eloquent.rs).
        let ident_str = ident.to_string();
        if ident_str == "__eager" || ident_str == "__pivot" {
            continue;
        }
        let user_ty = &f.ty;
        let is_pk = ident == primary_key;

        // For cast fields the inner SeaORM Model uses the Cast's
        // `Storage` type (i.e. the on-disk shape) rather than the
        // user's `Runtime` type. Without this, SeaORM can't map e.g.
        // `bool` to an INTEGER column or `Decimal` to TEXT — the row
        // materialisation fails at the driver boundary. The user's
        // struct keeps the runtime type; the From<...> impls in
        // derive_eloquent::emit bridge between the two via
        // `Cast::to_storage` / `Cast::from_storage`. PK fields keep
        // their declared type — PKs aren't cast-routed.
        let field_ty = if !is_pk {
            if let Some(cast_ty) = input.cast_for_field(&ident.to_string()) {
                let alias = format_ident!("__Suprnova_Cast_Storage_{ident}");
                storage_aliases.push(quote! {
                    #[allow(non_camel_case_types)]
                    pub type #alias = <#cast_ty as ::suprnova::eloquent::casts::Cast>::Storage;
                });
                quote! { #alias }
            } else {
                quote! { #user_ty }
            }
        } else {
            quote! { #user_ty }
        };

        if is_pk {
            pk_field_idents.push(ident.clone());
            let pk_attr = if auto_increment {
                quote! { #[sea_orm(primary_key)] }
            } else {
                quote! { #[sea_orm(primary_key, auto_increment = false)] }
            };
            field_decls.push(quote! {
                #pk_attr
                pub #ident: #field_ty
            });
        } else {
            field_decls.push(quote! {
                pub #ident: #field_ty
            });
        }
    }

    if pk_field_idents.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.item.ident,
            format!("#[model] struct must have a field named `{primary_key}` (the primary key)"),
        ));
    }

    Ok(quote! {
        // Cast storage type aliases — see the loop above for why these
        // exist as standalone aliases rather than inline projections.
        #(#storage_aliases)*

        // The user's struct becomes the SeaORM `Model` here. SeaORM's
        // `DeriveEntityModel` macro generates `Entity`, `Column`,
        // `PrimaryKey`, `ActiveModel`, and `EntityTrait`/`EntityName`
        // impls from this declaration. Emitted inside the per-model
        // module so the unprefixed SeaORM names (Entity / Column /
        // Relation) can't collide when two `#[suprnova::model]` structs
        // live in the same parent file.
        #[derive(::suprnova::sea_orm::DeriveEntityModel, Clone, Debug, PartialEq, ::serde::Serialize, ::serde::Deserialize)]
        #[sea_orm(table_name = #table)]
        pub struct Model {
            #(#field_decls,)*
        }

        // Empty Relation enum — Phase 10B fills this in. The macro
        // always emits the empty form so the DeriveEntityModel
        // requirement (which references `Relation` from inside the
        // generated `EntityTrait` impl) is satisfied.
        #[derive(Copy, Clone, Debug, ::suprnova::sea_orm::EnumIter, ::suprnova::sea_orm::DeriveRelation)]
        pub enum Relation {}

        impl ::suprnova::sea_orm::ActiveModelBehavior for ActiveModel {}
    })
}

//! Emits the SeaORM Entity / Model / ActiveModel triple. The user's
//! struct *becomes* the SeaORM `Model` via `DeriveEntityModel`, placed
//! inside the per-model inner module (`user::Model`, `smoke_user::Model`,
//! etc.). The user's original struct stays at parent scope under its
//! camel-case name (e.g. `User`); the Eloquent emit (`derive_eloquent.rs`)
//! bridges the two via the `EloquentModel` trait.

use proc_macro2::TokenStream;
use quote::quote;
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

    for f in fields {
        let ident = f.ident.as_ref().expect("named field");
        let ty = &f.ty;
        if *ident == *primary_key {
            pk_field_idents.push(ident.clone());
            let pk_attr = if auto_increment {
                quote! { #[sea_orm(primary_key)] }
            } else {
                quote! { #[sea_orm(primary_key, auto_increment = false)] }
            };
            field_decls.push(quote! {
                #pk_attr
                pub #ident: #ty
            });
        } else {
            field_decls.push(quote! {
                pub #ident: #ty
            });
        }
    }

    if pk_field_idents.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.item.ident,
            format!(
                "#[model] struct must have a field named `{primary_key}` (the primary key)"
            ),
        ));
    }

    Ok(quote! {
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

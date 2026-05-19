//! Emits Eloquent trait impls for the user's struct.
//!
//! Task 3 shipped the `EloquentModel` marker — enough to register the
//! type as a Suprnova model and bridge the user's struct name (e.g.
//! `User`) to the inner-module SeaORM `Entity` / `Column` types
//! (`user::Entity`, `user::Column`).
//!
//! Task 4 (this expansion) adds:
//! - `From<#inner::Model> for User` and `From<User> for #inner::Model`
//!   so the trait surface can round-trip between Suprnova and SeaORM
//! - `Default for User` so `replicate` can build a fresh shell
//! - `impl Model for User` with the per-model hook fills
//!   (`primary_key_value`, `apply_attrs_to_active_model`, ...)
//! - `impl ReplicateExt for User` with field-by-field clone
//! - `impl FirstOrCreate for User` with the `from_attrs_unsaved` hook
//!
//! Tasks 6 / 7a-c / 8 / 9 / 10 each extend this file with their slice
//! of the generated impl (fillable, casts, accessors/mutators,
//! timestamps, soft deletes).

use proc_macro2::TokenStream;
use quote::quote;
use syn::Result;

use super::parse::ModelInput;

pub fn emit(input: &ModelInput) -> Result<TokenStream> {
    let struct_ident = &input.item.ident;
    let module_name = input.module_name();
    // The literal table string from the macro attribute — used in the
    // `const TABLE` initialiser further down. SeaORM 1.1's
    // `EntityName::table_name` isn't `const fn`, so we can't call it
    // here; the parser captured the same value the runtime would
    // return.
    let table = &input.table;
    let pk_name = &input.primary_key;
    let pk_ident = quote::format_ident!("{pk_name}");

    let fields = match &input.item.fields {
        syn::Fields::Named(named) => &named.named,
        _ => unreachable!("validated in derive_seaorm"),
    };

    let field_idents: Vec<_> = fields
        .iter()
        .map(|f| f.ident.as_ref().expect("named").clone())
        .collect();
    let field_strs: Vec<String> = field_idents.iter().map(|i| i.to_string()).collect();

    let apply_arms = field_idents
        .iter()
        .zip(field_strs.iter())
        .map(|(ident, name)| {
            quote! {
                #name => {
                    am.#ident = ::suprnova::sea_orm::Set(
                        ::suprnova::serde_json::from_value(val.clone()).map_err(|e| {
                            ::suprnova::FrameworkError::validation(
                                #name,
                                ::std::format!("cannot decode JSON into column type: {e}"),
                            )
                        })?
                    );
                }
            }
        });

    let replicate_arms = field_idents
        .iter()
        .zip(field_strs.iter())
        .map(|(ident, name)| {
            let is_pk = name == pk_name;
            if is_pk {
                quote! { #ident: ::core::default::Default::default() }
            } else {
                quote! {
                    #ident: if except.iter().any(|e| e == #name) {
                        ::core::default::Default::default()
                    } else {
                        self.#ident.clone()
                    }
                }
            }
        });

    let from_attrs_unsaved_arms = field_idents
        .iter()
        .zip(field_strs.iter())
        .map(|(ident, name)| {
            quote! {
                #name => {
                    s.#ident = ::suprnova::serde_json::from_value(v.clone()).unwrap_or_default();
                }
            }
        });

    // For `save()`, we need every non-PK field marked as Set so SeaORM
    // emits a real UPDATE statement. The SeaORM-derived
    // `IntoActiveModel<ActiveModel> for Model` produces all-Unchanged
    // values (see DeriveActiveModel's `impl From<Model> for ActiveModel`
    // at sea-orm-macros 1.1.20/src/derives/active_model.rs:92), which
    // makes the resulting UPDATE a no-op. So we build the ActiveModel
    // explicitly: PK as Unchanged (it's the WHERE clause), every other
    // field as Set.
    let active_model_for_update_arms = field_idents
        .iter()
        .zip(field_strs.iter())
        .map(|(ident, name)| {
            let is_pk = name == pk_name;
            if is_pk {
                quote! { am.#ident = ::suprnova::sea_orm::ActiveValue::Unchanged(self.#ident.clone()); }
            } else {
                quote! { am.#ident = ::suprnova::sea_orm::Set(self.#ident.clone()); }
            }
        });

    Ok(quote! {
        impl ::suprnova::eloquent::EloquentModel for #struct_ident {
            type Entity = #module_name::Entity;
            type Column = #module_name::Column;
            // Literal table string captured at parse time — see T3
            // for why this is the literal rather than a SeaORM call.
            const TABLE: &'static str = #table;
        }

        // Bridge the user struct <-> SeaORM Model row. The inner
        // module's `Model` is what SeaORM returns from queries; the
        // user struct is what their code names. Same field set, so
        // field-by-field move both ways.
        impl ::core::convert::From<#module_name::Model> for #struct_ident {
            fn from(row: #module_name::Model) -> Self {
                Self { #( #field_idents: row.#field_idents, )* }
            }
        }

        impl ::core::convert::From<#struct_ident> for #module_name::Model {
            fn from(s: #struct_ident) -> Self {
                Self { #( #field_idents: s.#field_idents, )* }
            }
        }

        // `Default` lets `ReplicateExt::replicate_with` build a fresh
        // shell when clearing fields. Field types must individually
        // impl `Default`; we don't try to be clever here.
        impl ::core::default::Default for #struct_ident {
            fn default() -> Self {
                Self { #( #field_idents: ::core::default::Default::default(), )* }
            }
        }

        #[::suprnova::__async_trait::async_trait]
        impl ::suprnova::eloquent::Model for #struct_ident {
            fn primary_key_name() -> &'static str { #pk_name }

            fn fillable_filter() -> ::suprnova::eloquent::Fillable {
                // T4 default — guard the macro-parsed PK name (NOT a
                // hardcoded "id") so models with `primary_key = "uid"`
                // still have their PK protected from mass assignment.
                // T6 swaps this branch out when `fillable` / `guarded`
                // are specified.
                ::suprnova::eloquent::Fillable::guarded(::std::vec![#pk_name])
            }

            fn primary_key_value(
                &self,
            ) -> <<Self::Entity as ::suprnova::EntityTrait>::PrimaryKey as ::suprnova::PrimaryKeyTrait>::ValueType {
                self.#pk_ident.clone().into()
            }

            fn primary_key_value_json(&self) -> ::suprnova::serde_json::Value {
                ::suprnova::serde_json::to_value(&self.#pk_ident)
                    .unwrap_or(::suprnova::serde_json::Value::Null)
            }

            fn reset_primary_key(&mut self) {
                self.#pk_ident = ::core::default::Default::default();
            }

            fn active_model_from_attrs(
                attrs: ::suprnova::eloquent::Attrs,
            ) -> ::core::result::Result<
                <Self::Entity as ::suprnova::EntityTrait>::ActiveModel,
                ::suprnova::FrameworkError,
            > {
                let mut am = <<Self::Entity as ::suprnova::EntityTrait>::ActiveModel as ::core::default::Default>::default();
                <Self as ::suprnova::eloquent::Model>::apply_attrs_to_active_model(&mut am, attrs)?;
                ::core::result::Result::Ok(am)
            }

            fn apply_attrs_to_active_model(
                am: &mut <Self::Entity as ::suprnova::EntityTrait>::ActiveModel,
                attrs: ::suprnova::eloquent::Attrs,
            ) -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                for (name, val) in attrs.iter() {
                    match name {
                        #(#apply_arms,)*
                        other => {
                            return ::core::result::Result::Err(
                                ::suprnova::FrameworkError::validation(
                                    other,
                                    ::std::format!(
                                        "unknown column `{other}` on {}",
                                        ::core::stringify!(#struct_ident),
                                    ),
                                ),
                            );
                        }
                    }
                }
                ::core::result::Result::Ok(())
            }

            fn into_active_model_for_update(
                self,
            ) -> ::core::result::Result<
                <Self::Entity as ::suprnova::EntityTrait>::ActiveModel,
                ::suprnova::FrameworkError,
            > {
                let mut am = <<Self::Entity as ::suprnova::EntityTrait>::ActiveModel
                    as ::core::default::Default>::default();
                #( #active_model_for_update_arms )*
                ::core::result::Result::Ok(am)
            }
        }

        impl ::suprnova::eloquent::ReplicateExt for #struct_ident {
            fn replicate_with(&self, except: ::std::vec::Vec<::std::string::String>) -> Self {
                Self { #( #replicate_arms, )* }
            }
        }

        #[::suprnova::__async_trait::async_trait]
        impl ::suprnova::eloquent::FirstOrCreate for #struct_ident {
            fn from_attrs_unsaved(
                attrs: ::suprnova::eloquent::Attrs,
            ) -> ::core::result::Result<Self, ::suprnova::FrameworkError> {
                let mut s = <Self as ::core::default::Default>::default();
                for (name, v) in attrs.iter() {
                    match name {
                        #(#from_attrs_unsaved_arms,)*
                        _ => {}
                    }
                }
                ::core::result::Result::Ok(s)
            }
        }
    })
}

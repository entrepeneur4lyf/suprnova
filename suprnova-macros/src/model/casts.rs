//! Per-field cast wiring for the model macro. Called from
//! `derive_eloquent::emit` and `derive_seaorm::emit` to generate the
//! storage / runtime bridge for fields that carry a declared cast.
//!
//! Three responsibilities:
//!
//! 1. `apply_arm(...)` — used by `apply_attrs_to_active_model` to
//!    decode incoming JSON into the Runtime type, then route through
//!    `Cast::to_storage` before assigning to the ActiveModel field.
//! 2. `from_storage_arm(...)` — used by `From<inner::Model>` to
//!    decode the inner module's `Storage`-typed field back into the
//!    user struct's `Runtime` type via `Cast::from_storage`.
//! 3. `to_storage_arm(...)` — used by `From<UserStruct>` to encode
//!    the user struct's `Runtime`-typed field into the inner module's
//!    `Storage` type before handing back to SeaORM.
//!
//! The cast is fallible in both directions, so the generated arms
//! propagate via `?` where the surrounding function returns
//! `Result<_, FrameworkError>`. The two `From<...>` impls aren't
//! fallible by signature, so they `.expect("...")` — these can only
//! fail when the row in the database is corrupt (e.g. a non-RFC-3339
//! string in an `AsDateTime` column), which is a deployment-time
//! data-integrity issue rather than a user-input error.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

/// Generate the `match` arm for `apply_attrs_to_active_model` for one
/// field. Cast fields route through `Cast::to_storage` after JSON
/// decoding; non-cast fields fall back to the existing
/// `serde_json::from_value` flow.
pub fn apply_arm(name: &str, ident: &syn::Ident, cast_ty: Option<&Type>) -> TokenStream {
    match cast_ty {
        Some(cast_ty) => quote! {
            #name => {
                let runtime: <#cast_ty as ::suprnova::eloquent::casts::Cast>::Runtime =
                    ::suprnova::serde_json::from_value(val.clone())
                        .map_err(|e| ::suprnova::FrameworkError::validation(
                            #name,
                            ::std::format!("cast decode: {e}"),
                        ))?;
                let storage = <#cast_ty as ::suprnova::eloquent::casts::Cast>::to_storage(&runtime)?;
                am.#ident = ::suprnova::sea_orm::Set(storage);
            }
        },
        None => quote! {
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
        },
    }
}

/// Generate the struct-init arm for `From<inner::Model> for UserStruct`.
/// Cast fields call `Cast::from_storage` to inflate the storage shape
/// back into the runtime type.
pub fn from_storage_arm(ident: &syn::Ident, cast_ty: Option<&Type>) -> TokenStream {
    match cast_ty {
        Some(cast_ty) => quote! {
            #ident: <#cast_ty as ::suprnova::eloquent::casts::Cast>::from_storage(&row.#ident)
                .expect("cast from_storage failed — corrupt data in database column")
        },
        None => quote! { #ident: row.#ident },
    }
}

/// Generate the struct-init arm for `From<UserStruct> for inner::Model`.
/// Cast fields call `Cast::to_storage` to flatten the runtime shape
/// into the inner storage type.
pub fn to_storage_arm(ident: &syn::Ident, cast_ty: Option<&Type>) -> TokenStream {
    match cast_ty {
        Some(cast_ty) => quote! {
            #ident: <#cast_ty as ::suprnova::eloquent::casts::Cast>::to_storage(&s.#ident)
                .expect("cast to_storage failed — invalid runtime value")
        },
        None => quote! { #ident: s.#ident },
    }
}

/// Generate the `am.<field> = ...` statement for
/// `into_active_model_for_update`. Cast fields route through
/// `Cast::to_storage` (fallible — propagated via `?`); non-cast
/// fields use the existing `Set(self.<field>.clone())` shape. PK
/// fields are emitted by the caller with `ActiveValue::Unchanged`
/// before this arm runs.
pub fn active_model_update_stmt(
    ident: &syn::Ident,
    is_pk: bool,
    cast_ty: Option<&Type>,
) -> TokenStream {
    if is_pk {
        quote! { am.#ident = ::suprnova::sea_orm::ActiveValue::Unchanged(self.#ident.clone()); }
    } else {
        match cast_ty {
            Some(cast_ty) => quote! {
                am.#ident = ::suprnova::sea_orm::Set(
                    <#cast_ty as ::suprnova::eloquent::casts::Cast>::to_storage(&self.#ident)?
                );
            },
            None => quote! {
                am.#ident = ::suprnova::sea_orm::Set(self.#ident.clone());
            },
        }
    }
}

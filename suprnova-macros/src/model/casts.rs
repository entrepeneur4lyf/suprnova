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
//! fallible by signature, so they panic on failure — these can only
//! fail when the row in the database is corrupt (e.g. a non-RFC-3339
//! string in an `AsDateTime` column, or a deprecated enum variant
//! that no longer parses), which is a deployment-time data-integrity
//! issue rather than a user-input error.
//!
//! Domain 5 audit M-D5-1: panic messages include the offending field
//! name and the original `FrameworkError` so an operator can locate
//! which column failed and why directly from the trace — no
//! spelunking required. Domain 2's middleware safety net translates
//! the panic to a 500 response with the message in the tracing log.

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

/// Generate a `match` arm for `apply_attrs_to_active_model` that
/// routes through a user-supplied mutator before writing to the
/// ActiveModel. Used when the field name appears in
/// `#[model(mutators = [...])]`.
///
/// Shape:
///
/// 1. Build a fresh `Self::default()` scratch instance.
/// 2. Call `scratch.set_<field>(val.clone())?` — the user's body owns
///    the deserialise + transform.
/// 3. Serialise `scratch.<field>` back into JSON so the rest of the
///    write path matches the direct apply (same cast / from_value
///    behaviour as a non-mutator field with that name).
/// 4. Apply the resulting JSON the same way `apply_arm` would have —
///    via `Cast::to_storage` if the field also declares a cast,
///    otherwise direct `serde_json::from_value` into the storage type.
///
/// T8: mutator fields take precedence over the direct apply path.
/// A field that's both `mutators = [...]` and `casts = { ... }` keeps
/// both — the mutator transforms the runtime value, the cast handles
/// the storage shape.
pub fn mutator_apply_arm(
    name: &str,
    ident: &syn::Ident,
    cast_ty: Option<&Type>,
) -> TokenStream {
    let setter = quote::format_ident!("set_{}", ident);
    let storage_apply = match cast_ty {
        Some(cast_ty) => quote! {
            let runtime: <#cast_ty as ::suprnova::eloquent::casts::Cast>::Runtime =
                ::suprnova::serde_json::from_value(transformed)
                    .map_err(|e| ::suprnova::FrameworkError::validation(
                        #name,
                        ::std::format!("cast decode after mutator: {e}"),
                    ))?;
            let storage = <#cast_ty as ::suprnova::eloquent::casts::Cast>::to_storage(&runtime)?;
            am.#ident = ::suprnova::sea_orm::Set(storage);
        },
        None => quote! {
            am.#ident = ::suprnova::sea_orm::Set(
                ::suprnova::serde_json::from_value(transformed).map_err(|e| {
                    ::suprnova::FrameworkError::validation(
                        #name,
                        ::std::format!("cannot decode mutator output into column type: {e}"),
                    )
                })?
            );
        },
    };
    quote! {
        #name => {
            let mut scratch = <Self as ::core::default::Default>::default();
            scratch.#setter(val.clone())?;
            let transformed: ::suprnova::serde_json::Value =
                ::suprnova::serde_json::to_value(&scratch.#ident)
                    .map_err(|e| ::suprnova::FrameworkError::validation(
                        #name,
                        ::std::format!("mutator output serialize: {e}"),
                    ))?;
            #storage_apply
        }
    }
}

/// Generate the struct-init arm for `From<inner::Model> for UserStruct`.
/// Cast fields call `Cast::from_storage` to inflate the storage shape
/// back into the runtime type.
///
/// Domain 5 audit M-D5-1: panic on cast failure now includes the
/// field name and the original `FrameworkError`. The behaviour
/// (panic, not Result) is unchanged — `From` is infallible by
/// signature and changing that to `TryFrom` would break every
/// row-materialisation call site in the framework. Domain 2's
/// middleware safety net translates the panic to a 500 response.
pub fn from_storage_arm(ident: &syn::Ident, cast_ty: Option<&Type>) -> TokenStream {
    match cast_ty {
        Some(cast_ty) => {
            let field_name = ident.to_string();
            quote! {
                #ident: <#cast_ty as ::suprnova::eloquent::casts::Cast>::from_storage(&row.#ident)
                    .unwrap_or_else(|__cast_err| ::std::panic!(
                        "cast from_storage failed for field `{}`: {} \
                         (corrupt data in database column or schema drift)",
                        #field_name,
                        __cast_err,
                    ))
            }
        }
        None => quote! { #ident: row.#ident },
    }
}

/// Generate the struct-init arm for `From<UserStruct> for inner::Model`.
/// Cast fields call `Cast::to_storage` to flatten the runtime shape
/// into the inner storage type.
///
/// Domain 5 audit M-D5-1: panic on cast failure now includes the
/// field name and the original `FrameworkError`. Same panic-vs-Result
/// rationale as [`from_storage_arm`].
pub fn to_storage_arm(ident: &syn::Ident, cast_ty: Option<&Type>) -> TokenStream {
    match cast_ty {
        Some(cast_ty) => {
            let field_name = ident.to_string();
            quote! {
                #ident: <#cast_ty as ::suprnova::eloquent::casts::Cast>::to_storage(&s.#ident)
                    .unwrap_or_else(|__cast_err| ::std::panic!(
                        "cast to_storage failed for field `{}`: {} \
                         (invalid runtime value)",
                        #field_name,
                        __cast_err,
                    ))
            }
        }
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

#[cfg(test)]
mod tests {
    //! Domain 5 audit M-D5-1 regression: the `From<inner::Model>` and
    //! `From<UserStruct>` cast-failure panics must name the offending
    //! field and surface the original `FrameworkError`. Without this,
    //! a deployment that introduces schema drift (deprecated enum
    //! variant left in old rows) produces opaque panics that force
    //! operators to bisect columns to find the bad one.

    use super::*;
    use proc_macro2::Span;
    use syn::parse_quote;

    fn ident(name: &str) -> syn::Ident {
        syn::Ident::new(name, Span::call_site())
    }

    #[test]
    fn from_storage_arm_panic_includes_field_name() {
        let id = ident("email_verified_at");
        let cast: Type = parse_quote!(AsOptionalDateTime);
        let rendered = from_storage_arm(&id, Some(&cast)).to_string();
        // Field name appears verbatim — operator can grep this to find the column.
        assert!(
            rendered.contains("email_verified_at"),
            "panic message must name the field; got: {rendered}"
        );
        // Confirms we're emitting the unwrap_or_else / panic form, not `.expect`.
        assert!(
            rendered.contains("unwrap_or_else") && rendered.contains("panic"),
            "from_storage_arm must use unwrap_or_else + panic for diagnostic; got: {rendered}"
        );
        // The format string must interpolate the original FrameworkError too.
        assert!(
            rendered.contains("__cast_err"),
            "panic must format the source error; got: {rendered}"
        );
    }

    #[test]
    fn to_storage_arm_panic_includes_field_name() {
        let id = ident("encrypted_token");
        let cast: Type = parse_quote!(AsEncrypted<String>);
        let rendered = to_storage_arm(&id, Some(&cast)).to_string();
        assert!(
            rendered.contains("encrypted_token"),
            "panic message must name the field; got: {rendered}"
        );
        assert!(
            rendered.contains("unwrap_or_else") && rendered.contains("panic"),
            "to_storage_arm must use unwrap_or_else + panic for diagnostic; got: {rendered}"
        );
        assert!(
            rendered.contains("__cast_err"),
            "panic must format the source error; got: {rendered}"
        );
    }

    #[test]
    fn non_cast_field_arms_are_trivial_assignments() {
        // Sanity: a non-cast field must NOT emit panic-related plumbing —
        // the From arm is a straight field assignment.
        let id = ident("id");
        let rendered_from = from_storage_arm(&id, None).to_string();
        let rendered_to = to_storage_arm(&id, None).to_string();
        assert!(
            !rendered_from.contains("panic"),
            "non-cast from arm should not panic; got: {rendered_from}"
        );
        assert!(
            !rendered_to.contains("panic"),
            "non-cast to arm should not panic; got: {rendered_to}"
        );
    }
}

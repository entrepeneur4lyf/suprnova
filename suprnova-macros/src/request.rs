//! Request derive macro implementation
//!
//! Generates the FormRequest trait implementation and adds necessary derives.
//! Works for both JSON and form-urlencoded request bodies.

use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, parse_macro_input};

/// Parsed `#[form_request(...)]` struct-level options.
struct FormRequestAttrs {
    /// Per-struct override for the request-body byte cap. `None` falls
    /// through to the process-global cap at runtime.
    max_body_bytes: Option<proc_macro2::TokenStream>,
    /// When `true`, suppress the macro's default `impl FormRequest` and
    /// let the caller write their own — needed when overriding
    /// `authorize` / `after_validation` / `after_validation_async`,
    /// since you cannot add a second `impl FormRequest`. Mirrors the
    /// `#[multipart(custom_hooks)]` opt-out shape.
    custom_hooks: bool,
}

/// Parse struct-level `#[form_request(...)]` options.
///
/// Supported:
/// - `max_body_bytes = N` — per-struct cap on total request body size, in
///   bytes. When absent, the trait default delegates to the process-global
///   cap (see `suprnova::http::body::global_max_request_body_bytes`).
/// - `custom_hooks` — suppress the auto-emitted `impl FormRequest` so the
///   caller can write their own to override `authorize` /
///   `after_validation` / `after_validation_async`.
fn parse_form_request_attrs(attrs: &[syn::Attribute]) -> Result<FormRequestAttrs, syn::Error> {
    let mut max_body_bytes: Option<proc_macro2::TokenStream> = None;
    let mut custom_hooks = false;
    for attr in attrs {
        if attr.path().is_ident("form_request") {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("max_body_bytes") {
                    let value: syn::Expr = meta.value()?.parse()?;
                    max_body_bytes = Some(quote! { #value });
                    return Ok(());
                }
                if meta.path.is_ident("custom_hooks") {
                    custom_hooks = true;
                    return Ok(());
                }
                Err(meta.error("unknown #[form_request(...)] option"))
            })?;
        }
    }
    Ok(FormRequestAttrs {
        max_body_bytes,
        custom_hooks,
    })
}

/// Emit the body of the generated `impl FormRequest` block, including any
/// overridden trait methods (currently just `max_body_bytes`).
fn impl_body(max_body_bytes: Option<proc_macro2::TokenStream>) -> proc_macro2::TokenStream {
    if let Some(expr) = max_body_bytes {
        quote! {
            fn max_body_bytes() -> usize {
                (#expr) as usize
            }
        }
    } else {
        quote! {}
    }
}

/// Implementation of the `#[derive(FormRequest)]` derive macro
///
/// This macro generates the `FormRequest` trait implementation for a struct.
/// The struct must also derive `::suprnova::serde::Deserialize` and `::suprnova::validator::Validate`.
///
/// For the best DX, use all three derives together:
///
/// ```rust,no_run
/// # use suprnova::FormRequestDerive as FormRequest;
/// #[derive(::suprnova::serde::Deserialize, ::suprnova::validator::Validate, FormRequest)]
/// pub struct CreateUserRequest {
///     #[validate(email)]
///     pub email: String,
/// }
/// ```
///
/// Or with the suprnova prelude which re-exports these:
///
/// ```rust,ignore
/// use suprnova::{FormRequest, Deserialize, Validate};
///
/// #[derive(Deserialize, Validate, FormRequest)]
/// pub struct CreateUserRequest {
///     #[validate(email)]
///     pub email: String,
/// }
/// ```
///
/// ## Struct-level options
///
/// `#[form_request(max_body_bytes = N)]` overrides the request body cap
/// for this FormRequest. Defaults to the process-global cap
/// (`suprnova::http::body::global_max_request_body_bytes()`, itself
/// derived from the 8 MiB compile-time default).
///
/// ```rust,ignore
/// #[derive(Deserialize, Validate, FormRequest)]
/// #[form_request(max_body_bytes = 64 * 1024 * 1024)] // 64 MiB
/// pub struct ImportPayload {
///     pub rows: Vec<Row>,
/// }
/// ```
pub fn derive_request_impl(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let generics = &input.generics;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let attrs = match parse_form_request_attrs(&input.attrs) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };

    // Opt-out: the caller is writing `impl FormRequest` by hand to
    // override authorize / after_validation / after_validation_async.
    // Emitting our default impl too would collide.
    if attrs.custom_hooks {
        return TokenStream::new();
    }

    let body = impl_body(attrs.max_body_bytes);

    let output = quote! {
        impl #impl_generics ::suprnova::FormRequest for #name #ty_generics #where_clause {
            #body
        }
    };

    output.into()
}

/// Implementation of the `#[request]` attribute macro
///
/// This attribute macro provides the cleanest DX by automatically adding
/// the necessary derives. Just use `#[request]` and you're done:
///
/// ```rust,no_run
/// use suprnova::request;
///
/// #[request]
/// pub struct CreateUserRequest {
///     #[validate(email)]
///     pub email: String,
///
///     #[validate(length(min = 8))]
///     pub password: String,
/// }
/// ```
///
/// This expands to:
///
/// ```rust,no_run
/// #[derive(::suprnova::serde::Deserialize, ::suprnova::validator::Validate)]
/// pub struct CreateUserRequest {
///     #[validate(email)]
///     pub email: String,
///
///     #[validate(length(min = 8))]
///     pub password: String,
/// }
///
/// impl ::suprnova::FormRequest for CreateUserRequest {}
/// ```
///
/// ## Content Type Support
///
/// The `#[request]` attribute works with both:
/// - `application/json` - JSON request bodies
/// - `application/x-www-form-urlencoded` - HTML form submissions
///
/// The content type is automatically detected from the request headers.
pub fn request_attr_impl(_attr: TokenStream, input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let vis = &input.vis;
    let generics = &input.generics;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    // Re-emit user attributes EXCEPT `#[form_request(...)]`, which we
    // consume below. `#[request]` does not add a `FormRequest` derive
    // (it emits the impl directly), so leaving `#[form_request]` on
    // the struct would surface as a rustc "cannot find attribute"
    // error because no derive registers it as a helper attribute.
    let attrs: Vec<&syn::Attribute> = input
        .attrs
        .iter()
        .filter(|a| !a.path().is_ident("form_request"))
        .collect();

    // Get struct data
    let data = match &input.data {
        syn::Data::Struct(data) => data,
        _ => {
            return syn::Error::new_spanned(&input, "#[request] can only be used on structs")
                .to_compile_error()
                .into();
        }
    };

    let fields = &data.fields;

    // The same `#[form_request(...)]` struct attributes are honored
    // under the attribute-macro form. They live in `#attrs` (which we
    // re-emit on the struct verbatim) and are harmlessly ignored by
    // syn/serde/validator; we parse them here only to drive impl
    // emission.
    let parsed_attrs = match parse_form_request_attrs(&input.attrs) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };

    // Unit and tuple structs need a trailing `;` because `#fields`
    // expands to nothing or `(T)` respectively without a terminator;
    // named-field structs are terminated by their own `{ ... }`.
    let semi = match fields {
        syn::Fields::Named(_) => quote!(),
        syn::Fields::Unit | syn::Fields::Unnamed(_) => quote!(;),
    };

    let form_request_impl = if parsed_attrs.custom_hooks {
        // Caller writes their own `impl FormRequest` to override hooks.
        quote!()
    } else {
        let body = impl_body(parsed_attrs.max_body_bytes);
        quote! {
            impl #impl_generics ::suprnova::FormRequest for #name #ty_generics #where_clause {
                #body
            }
        }
    };

    let output = quote! {
        #(#attrs)*
        #[derive(::suprnova::serde::Deserialize, ::suprnova::validator::Validate)]
        #vis struct #name #generics #fields #semi

        #form_request_impl
    };

    output.into()
}

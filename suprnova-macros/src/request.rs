//! Request derive macro implementation
//!
//! Generates the FormRequest trait implementation and adds necessary derives.
//! Works for both JSON and form-urlencoded request bodies.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

/// Parse struct-level `#[form_request(...)]` options.
///
/// Currently supports:
/// - `max_body_bytes = N` — per-struct cap on total request body size, in
///   bytes. When absent, the trait default delegates to the process-global
///   cap (see `suprnova::http::body::global_max_request_body_bytes`).
fn parse_form_request_attrs(
    attrs: &[syn::Attribute],
) -> Result<Option<proc_macro2::TokenStream>, syn::Error> {
    let mut max_body_bytes: Option<proc_macro2::TokenStream> = None;
    for attr in attrs {
        if attr.path().is_ident("form_request") {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("max_body_bytes") {
                    let value: syn::Expr = meta.value()?.parse()?;
                    max_body_bytes = Some(quote! { #value });
                    return Ok(());
                }
                Err(meta.error("unknown #[form_request(...)] option"))
            })?;
        }
    }
    Ok(max_body_bytes)
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
/// The struct must also derive `serde::Deserialize` and `validator::Validate`.
///
/// For the best DX, use all three derives together:
///
/// ```rust,ignore
/// #[derive(serde::Deserialize, validator::Validate, FormRequest)]
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

    let max_body_bytes = match parse_form_request_attrs(&input.attrs) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let body = impl_body(max_body_bytes);

    let output = quote! {
        impl #impl_generics suprnova::FormRequest for #name #ty_generics #where_clause {
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
/// ```rust,ignore
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
/// ```rust,ignore
/// #[derive(serde::Deserialize, validator::Validate)]
/// pub struct CreateUserRequest {
///     #[validate(email)]
///     pub email: String,
///
///     #[validate(length(min = 8))]
///     pub password: String,
/// }
///
/// impl suprnova::FormRequest for CreateUserRequest {}
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
    let attrs = &input.attrs;
    let generics = &input.generics;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

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

    // The same `#[form_request(max_body_bytes = N)]` struct attribute is
    // honored under the attribute-macro form. It lives in `#attrs` (which
    // we re-emit on the struct verbatim) and is harmlessly ignored by
    // syn/serde/validator; we parse it here only to generate the trait
    // impl override.
    let max_body_bytes = match parse_form_request_attrs(&input.attrs) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let body = impl_body(max_body_bytes);

    let output = quote! {
        #(#attrs)*
        #[derive(serde::Deserialize, validator::Validate)]
        #vis struct #name #generics #fields

        impl #impl_generics suprnova::FormRequest for #name #ty_generics #where_clause {
            #body
        }
    };

    output.into()
}

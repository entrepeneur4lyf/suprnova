//! Request derive macro implementation
//!
//! Generates the FormRequest trait implementation and adds necessary derives.
//! Works for both JSON and form-urlencoded request bodies.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

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
pub fn derive_request_impl(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let generics = &input.generics;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let output = quote! {
        impl #impl_generics suprnova::FormRequest for #name #ty_generics #where_clause {}
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

    let output = quote! {
        #(#attrs)*
        #[derive(serde::Deserialize, validator::Validate)]
        #vis struct #name #generics #fields

        impl #impl_generics suprnova::FormRequest for #name #ty_generics #where_clause {}
    };

    output.into()
}

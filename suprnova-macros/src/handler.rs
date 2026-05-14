//! Handler attribute macro implementation
//!
//! Transforms controller functions to automatically extract typed parameters
//! from HTTP requests, including path parameters and route model binding.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, FnArg, ItemFn, Pat, Type};

/// Parameter classification for extraction strategy
enum ParamKind {
    /// Request type - pass through unchanged
    Request,
    /// Primitive type (i32, String, etc.) - extract from path params via FromParam
    Primitive,
    /// Model type (*::Model) - extract via RouteBinding
    Model,
    /// Other types - extract via FromRequest (FormRequest, etc.)
    FormRequest,
}

/// Implementation of the `#[handler]` attribute macro
///
/// Supports multiple parameter extraction:
///
/// - `Request` - passes through unchanged
/// - Primitives (`i32`, `String`, etc.) - extracted from path params via `FromParam`
/// - Model types (`user::Model`) - extracted via `RouteBinding` (auto 404 if not found)
/// - Other types - extracted via `FromRequest` (FormRequest validation)
///
/// # Examples
///
/// ```rust,ignore
/// // No parameters
/// #[handler]
/// pub async fn index() -> Response { ... }
///
/// // Request passthrough
/// #[handler]
/// pub async fn show(req: Request) -> Response { ... }
///
/// // Path parameter extraction
/// #[handler]
/// pub async fn show(id: i32) -> Response { ... }
///
/// // Route model binding
/// #[handler]
/// pub async fn show(user: user::Model) -> Response { ... }
///
/// // FormRequest validation
/// #[handler]
/// pub async fn store(form: CreateUserRequest) -> Response { ... }
///
/// // Mixed parameters
/// #[handler]
/// pub async fn update(user: user::Model, form: UpdateUserRequest) -> Response { ... }
/// ```
pub fn handler_impl(_attr: TokenStream, input: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(input as ItemFn);

    let fn_vis = &input_fn.vis;
    let fn_name = &input_fn.sig.ident;
    let fn_generics = &input_fn.sig.generics;
    let fn_output = &input_fn.sig.output;
    let fn_block = &input_fn.block;
    let fn_attrs = &input_fn.attrs;

    let is_async = input_fn.sig.asyncness.is_some();
    let async_token = if is_async {
        quote! { async }
    } else {
        quote! {}
    };

    // Collect all parameters
    let params: Vec<_> = input_fn.sig.inputs.iter().collect();

    // Handle no parameters case
    if params.is_empty() {
        let output = quote! {
            #(#fn_attrs)*
            #fn_vis #async_token fn #fn_name #fn_generics(_: suprnova::Request) #fn_output {
                #fn_block
            }
        };
        return output.into();
    }

    // Process parameters and generate extraction code
    let mut extractions = Vec::new();
    let mut has_request_consumer = false;
    let mut has_request_param = false;

    for param in &params {
        match param {
            FnArg::Typed(pat_type) => {
                let param_pat = &pat_type.pat;
                let param_type = &pat_type.ty;
                let param_name = extract_param_name(param_pat);

                let kind = classify_param_type(param_type);

                let extraction = generate_extraction(
                    param_pat,
                    param_type,
                    &param_name,
                    &kind,
                    &mut has_request_consumer,
                    &mut has_request_param,
                );
                extractions.push(extraction);
            }
            FnArg::Receiver(_) => {
                return syn::Error::new_spanned(
                    param,
                    "#[handler] does not support methods with self receiver",
                )
                .to_compile_error()
                .into();
            }
        }
    }

    // Generate the transformed function
    let output = if has_request_param {
        // If we have a Request param, we need to handle it specially
        quote! {
            #(#fn_attrs)*
            #fn_vis #async_token fn #fn_name #fn_generics(__suprnova_req: suprnova::Request) #fn_output {
                let __suprnova_params = __suprnova_req.params().clone();
                #(#extractions)*
                #fn_block
            }
        }
    } else {
        quote! {
            #(#fn_attrs)*
            #fn_vis #async_token fn #fn_name #fn_generics(__suprnova_req: suprnova::Request) #fn_output {
                let __suprnova_params = __suprnova_req.params().clone();
                #(#extractions)*
                #fn_block
            }
        }
    };

    output.into()
}

/// Extract the parameter name as a string from the pattern
fn extract_param_name(pat: &Pat) -> String {
    match pat {
        Pat::Ident(pat_ident) => pat_ident.ident.to_string(),
        Pat::Wild(_) => "_".to_string(),
        _ => "param".to_string(),
    }
}

/// Classify the parameter type to determine extraction strategy
fn classify_param_type(ty: &Type) -> ParamKind {
    match ty {
        Type::Path(type_path) => {
            let segments = &type_path.path.segments;

            // Check for Request type
            if segments.len() == 1 && segments[0].ident == "Request" {
                return ParamKind::Request;
            }
            if segments.len() == 2
                && segments[0].ident == "suprnova"
                && segments[1].ident == "Request"
            {
                return ParamKind::Request;
            }

            // Check for primitive types
            if segments.len() == 1 {
                let ident = segments[0].ident.to_string();
                if is_primitive_type_name(&ident) {
                    return ParamKind::Primitive;
                }
            }

            // Check for Model type (path ends with ::Model)
            if let Some(last_segment) = segments.last() {
                if last_segment.ident == "Model" && segments.len() >= 2 {
                    return ParamKind::Model;
                }
            }

            // Default to FormRequest for other types
            ParamKind::FormRequest
        }
        _ => ParamKind::FormRequest,
    }
}

/// Check if a type name is a primitive that should use FromParam
fn is_primitive_type_name(name: &str) -> bool {
    matches!(
        name,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "isize"
            | "String"
    )
}

/// Generate extraction code for a parameter based on its classification
fn generate_extraction(
    pat: &Pat,
    ty: &Type,
    param_name: &str,
    kind: &ParamKind,
    has_consumer: &mut bool,
    has_request: &mut bool,
) -> TokenStream2 {
    match kind {
        ParamKind::Request => {
            *has_request = true;
            *has_consumer = true;
            quote! {
                let #pat: #ty = __suprnova_req;
            }
        }
        ParamKind::Primitive => {
            // Extract from path params using FromParam
            quote! {
                let #pat: #ty = {
                    let __value = __suprnova_params.get(#param_name)
                        .ok_or_else(|| suprnova::FrameworkError::param(#param_name))?;
                    <#ty as suprnova::FromParam>::from_param(__value)?
                };
            }
        }
        ParamKind::Model => {
            // Route model binding using AutoRouteBinding trait
            // The parameter name comes from the function signature
            quote! {
                let #pat: #ty = {
                    let __value = __suprnova_params.get(#param_name)
                        .ok_or_else(|| suprnova::FrameworkError::param(#param_name))?;
                    <#ty as suprnova::AutoRouteBinding>::from_route_param(__value).await?
                };
            }
        }
        ParamKind::FormRequest => {
            // Use FromRequest trait (consumes request body)
            *has_consumer = true;
            quote! {
                let #pat: #ty = <#ty as suprnova::FromRequest>::from_request(__suprnova_req).await?;
            }
        }
    }
}

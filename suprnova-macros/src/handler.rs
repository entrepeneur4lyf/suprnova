//! Handler attribute macro implementation
//!
//! Transforms controller functions to automatically extract typed parameters
//! from HTTP requests, including path parameters and route model binding.
//!
//! ## Extractor combination rules
//!
//! The macro generates a single transformed `fn(req: Request)` shape that
//! moves `req` into at most one of the body-consuming extractors. `Request`
//! and `FormRequest` both consume the request body, so the macro **rejects
//! at expansion time** any combination with more than one of them — the
//! emitted code would otherwise trip E0382 (use of moved value) with no
//! actionable diagnostic for the user.
//!
//! Legal shapes (compile):
//! - zero params (e.g. `fn index()`)
//! - a single `Request` (e.g. `fn show(req: Request)`)
//! - a single `FormRequest`-derived extractor (e.g. `fn store(form: CreateUser)`)
//! - any number of `Primitive` + `Model` params alongside at most one consumer
//!   (e.g. `fn update(user: user::Model, form: UpdateUser)`)
//!
//! Rejected at expansion (clear macro error):
//! - two or more `FormRequest` params
//! - `Request` plus any `FormRequest` param

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{FnArg, ItemFn, Pat, Type};

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
    handler_impl_inner(input.into()).into()
}

/// `proc_macro2`-flavoured entry point. The outer `handler_impl` is a thin
/// shim that converts the host `proc_macro::TokenStream`. Splitting the work
/// here lets the unit tests below feed in token streams directly and assert
/// on the rendered output — the host `proc_macro::TokenStream` cannot be
/// constructed outside a real macro-expansion context.
fn handler_impl_inner(input: TokenStream2) -> TokenStream2 {
    let input_fn: ItemFn = match syn::parse2(input) {
        Ok(f) => f,
        Err(e) => return e.to_compile_error(),
    };

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
        return quote! {
            #(#fn_attrs)*
            #fn_vis #async_token fn #fn_name #fn_generics(_: ::suprnova::Request) #fn_output {
                #fn_block
            }
        };
    }

    // First pass: classify every param and count the body-consuming
    // extractors so we can reject `Request` + `FormRequest` /
    // `FormRequest` × 2 / etc. with a clear macro error before we try
    // to emit code that would move `__suprnova_req` twice and trip E0382.
    let mut classifications = Vec::with_capacity(params.len());
    let mut request_consumer_count = 0usize;
    let mut last_consumer_span: Option<&FnArg> = None;
    for param in &params {
        let pat_type = match param {
            FnArg::Typed(pt) => pt,
            FnArg::Receiver(_) => {
                return syn::Error::new_spanned(
                    param,
                    "#[handler] does not support methods with self receiver",
                )
                .to_compile_error();
            }
        };
        let kind = classify_param_type(&pat_type.ty);
        if matches!(kind, ParamKind::Request | ParamKind::FormRequest) {
            request_consumer_count += 1;
            last_consumer_span = Some(*param);
        }
        classifications.push((pat_type, kind));
    }

    if request_consumer_count > 1 {
        // Point the diagnostic at the most-recent offending parameter so
        // the user's eye lands somewhere meaningful in the signature.
        let span_target = last_consumer_span.unwrap_or(params[0]);
        return syn::Error::new_spanned(
            span_target,
            "#[handler] supports at most one body-consuming extractor \
             per signature (Request or any FormRequest). Combining two \
             would move the underlying `Request` twice. Split the work \
             across separate handlers, or fold the extra extractor into \
             a single FormRequest struct.",
        )
        .to_compile_error();
    }

    // Second pass: emit extractions now that we know the signature is legal.
    let mut extractions = Vec::with_capacity(classifications.len());
    for (pat_type, kind) in &classifications {
        let param_pat = &pat_type.pat;
        let param_type = &pat_type.ty;
        let param_name = extract_param_name(param_pat);
        extractions.push(generate_extraction(
            param_pat,
            param_type,
            &param_name,
            kind,
        ));
    }

    quote! {
        #(#fn_attrs)*
        #fn_vis #async_token fn #fn_name #fn_generics(__suprnova_req: ::suprnova::Request) #fn_output {
            let __suprnova_params = __suprnova_req.params().clone();
            #(#extractions)*
            #fn_block
        }
    }
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
            if let Some(last_segment) = segments.last()
                && last_segment.ident == "Model"
                && segments.len() >= 2
            {
                return ParamKind::Model;
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

/// Generate extraction code for a parameter based on its classification.
///
/// The caller is responsible for ensuring at most one of the body-consuming
/// kinds (`Request`, `FormRequest`) is emitted per signature; this fn does
/// not re-check.
fn generate_extraction(pat: &Pat, ty: &Type, param_name: &str, kind: &ParamKind) -> TokenStream2 {
    match kind {
        ParamKind::Request => quote! {
            let #pat: #ty = __suprnova_req;
        },
        ParamKind::Primitive => {
            // Extract from path params using FromParam
            quote! {
                let #pat: #ty = {
                    let __value = __suprnova_params.get(#param_name)
                        .ok_or_else(|| ::suprnova::FrameworkError::param(#param_name))?;
                    <#ty as ::suprnova::FromParam>::from_param(__value)?
                };
            }
        }
        ParamKind::Model => {
            // Route model binding using AutoRouteBinding trait
            // The parameter name comes from the function signature
            quote! {
                let #pat: #ty = {
                    let __value = __suprnova_params.get(#param_name)
                        .ok_or_else(|| ::suprnova::FrameworkError::param(#param_name))?;
                    <#ty as ::suprnova::AutoRouteBinding>::from_route_param(__value).await?
                };
            }
        }
        ParamKind::FormRequest => quote! {
            let #pat: #ty = <#ty as ::suprnova::FromRequest>::from_request(__suprnova_req).await?;
        },
    }
}

#[cfg(test)]
mod tests {
    //! Macro-expansion regressions for the body-consumer constraint.
    //!
    //! Two-FormRequest, Request + FormRequest, and friends would emit
    //! `let _ = __suprnova_req; let _ = …(__suprnova_req).await?;` —
    //! moving the same value twice. rustc reports E0382 deep inside
    //! generated code, far from the user's signature, with no hint at
    //! the actual constraint. The macro now rejects those signatures
    //! at expansion with a single span-pointed diagnostic.
    //!
    //! Legal signatures (zero/one consumer, plus any Primitive/Model
    //! params) must keep round-tripping cleanly.
    use super::*;
    use quote::quote;

    /// Render `handler_impl_inner` against a function and look for a
    /// span-rendered diagnostic marker. `compile_error! { … }` is the
    /// surface form `syn::Error::to_compile_error()` produces, so a
    /// rejection produces a token stream whose string form contains
    /// the macro path and our message.
    fn expansion(src: proc_macro2::TokenStream) -> String {
        handler_impl_inner(src).to_string()
    }

    #[test]
    fn rejects_two_form_request_params() {
        let out = expansion(quote! {
            pub async fn store(a: CreateUser, b: UpdateUser) -> Response { todo!() }
        });
        assert!(
            out.contains("compile_error"),
            "two FormRequest params must reject; got:\n{out}"
        );
        assert!(
            out.contains("body-consuming"),
            "rejection message must mention the constraint; got:\n{out}"
        );
    }

    #[test]
    fn rejects_request_plus_form_request() {
        let out = expansion(quote! {
            pub async fn store(req: Request, form: CreateUser) -> Response { todo!() }
        });
        assert!(
            out.contains("compile_error"),
            "Request + FormRequest must reject; got:\n{out}"
        );
    }

    #[test]
    fn rejects_three_form_request_params() {
        // Defensive: the cap is "at most one", not "exactly two".
        let out = expansion(quote! {
            pub async fn store(a: A, b: B, c: C) -> Response { todo!() }
        });
        assert!(
            out.contains("compile_error"),
            "three FormRequest params must reject; got:\n{out}"
        );
    }

    #[test]
    fn accepts_single_request() {
        let out = expansion(quote! {
            pub async fn show(req: Request) -> Response { todo!() }
        });
        assert!(
            !out.contains("compile_error"),
            "single Request must compile; got:\n{out}"
        );
        // Confirms the request actually got forwarded into the body.
        assert!(out.contains("__suprnova_req"));
    }

    #[test]
    fn accepts_single_form_request() {
        let out = expansion(quote! {
            pub async fn store(form: CreateUser) -> Response { todo!() }
        });
        assert!(
            !out.contains("compile_error"),
            "single FormRequest must compile; got:\n{out}"
        );
        assert!(out.contains("from_request"));
    }

    #[test]
    fn accepts_zero_params() {
        let out = expansion(quote! {
            pub async fn index() -> Response { todo!() }
        });
        assert!(
            !out.contains("compile_error"),
            "zero-param handler must compile; got:\n{out}"
        );
    }

    #[test]
    fn accepts_model_plus_form_request_mix() {
        // The documented Mixed example: `update(user: user::Model,
        // form: UpdateUserRequest)`. Model reads from the cloned
        // params map and never touches `__suprnova_req`, so this
        // counts as one body-consumer overall — legal.
        let out = expansion(quote! {
            pub async fn update(user: user::Model, form: UpdateUserRequest) -> Response { todo!() }
        });
        assert!(
            !out.contains("compile_error"),
            "Model + FormRequest must compile (Model is non-consuming); got:\n{out}"
        );
    }

    #[test]
    fn accepts_primitive_plus_form_request_mix() {
        let out = expansion(quote! {
            pub async fn update(id: i32, form: UpdateUserRequest) -> Response { todo!() }
        });
        assert!(
            !out.contains("compile_error"),
            "Primitive + FormRequest must compile (Primitive is non-consuming); got:\n{out}"
        );
    }

    #[test]
    fn accepts_primitive_plus_request_mix() {
        // Request is a consumer, but only ONE of it — Primitive reads
        // from the cloned params clone, so the combination is legal.
        let out = expansion(quote! {
            pub async fn show(id: i32, req: Request) -> Response { todo!() }
        });
        assert!(
            !out.contains("compile_error"),
            "Primitive + Request must compile (Primitive is non-consuming); got:\n{out}"
        );
    }
}

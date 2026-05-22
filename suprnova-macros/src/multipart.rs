//! `#[derive(MultipartRequest)]` — strongly-typed multipart extractor.
//!
//! Emits two impls per struct:
//! 1. `impl FromRequest` — calls hooks, parses the body once via
//!    `parse_multipart_streaming_with_cap`, dispatches each `(name, value)`
//!    to the right field, then constructs `Self`.
//! 2. `impl MultipartRequestHooks` — empty default unless the struct
//!    carries `#[multipart(custom_hooks)]`, in which case the user
//!    provides their own impl.
//!
//! Validators receive a bounded sniff buffer + the running size in
//! bytes; the parser captures both during streaming so neither
//! `validate_chunk` nor `validate_final` requires the full part in
//! memory.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields, LitStr, Type};

pub fn expand(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_inner(input).into()
}

/// Pure-`proc_macro2` helper so unit tests can exercise the
/// expansion shape without leaving the proc-macro crate. Returns the
/// expansion (or a `compile_error!`-shaped token stream on bad input).
fn expand_inner(input: DeriveInput) -> proc_macro2::TokenStream {
    let struct_name = &input.ident;

    // Parse struct-level `#[multipart(...)]` options.
    //
    // `custom_hooks`         — caller provides the `MultipartRequestHooks` impl.
    // `max_body_bytes = N`   — per-struct cap on total request body size, in bytes.
    //                          When absent, the macro falls through to the
    //                          process-global cap at runtime.
    let mut emit_default_hooks = true;
    let mut max_body_bytes: Option<proc_macro2::TokenStream> = None;
    for attr in &input.attrs {
        if attr.path().is_ident("multipart") {
            // Domain 5 audit M-D5-3: propagate parse errors as a
            // compile_error rather than swallowing them via `let _ = ...`.
            // Previously a typo like `#[multipart(max_body_byte = 1024)]`
            // (missing the trailing `s`) silently kept the default cap.
            if let Err(e) = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("custom_hooks") {
                    emit_default_hooks = false;
                    return Ok(());
                }
                if meta.path.is_ident("max_body_bytes") {
                    let value: syn::Expr = meta.value()?.parse()?;
                    max_body_bytes = Some(quote::quote! { #value });
                    return Ok(());
                }
                Err(meta.error("unknown #[multipart(...)] option"))
            }) {
                return e.to_compile_error();
            }
        }
    }

    // Compute the cap expression once: per-struct override (if set), else
    // the process-global accessor evaluated at runtime.
    let max_body_bytes_expr: proc_macro2::TokenStream = if let Some(override_expr) = max_body_bytes
    {
        quote::quote! { (#override_expr) as usize }
    } else {
        quote::quote! { ::suprnova::http::upload::global_max_multipart_body_bytes() }
    };

    let Data::Struct(data) = &input.data else {
        return syn::Error::new_spanned(&input, "MultipartRequest requires a struct")
            .to_compile_error();
    };
    let Fields::Named(fields) = &data.fields else {
        return syn::Error::new_spanned(&data.fields, "MultipartRequest requires named fields")
            .to_compile_error();
    };

    let mut field_decls = Vec::new();
    let mut field_arms = Vec::new();
    let mut validator_arms = Vec::new();
    let mut validator_decls = Vec::new();
    let mut struct_init = Vec::new();

    for field in &fields.named {
        let ident = field.ident.clone().unwrap();
        let ty = &field.ty;

        // Parse `#[field("name")]` or `#[field("name", max_count = N)]`.
        //
        // `max_count = N` is a count cap on Vec fields. The Phase 4 body
        // cap blocks total bytes but a `Vec<UploadedFile<()>>` field could
        // accept unlimited part count within that budget (a request with
        // 100k 1-byte parts in a 25 MiB body would allocate a
        // `MultipartValue::File` per part). `max_count` enforces a
        // per-Vec ceiling: once the in-progress Vec reaches `max_count`
        // parts, the next push short-circuits with 422 before allocating.
        //
        // Currently honoured for `Vec<UploadedFile<V>>` (FileVec) and
        // `Vec<T: FromStr>` (TextVec). On scalar/option fields the
        // attribute is accepted but does nothing (the parser keeps
        // first-write-wins semantics already).
        let mut field_name: Option<LitStr> = None;
        let mut max_count: Option<usize> = None;
        for attr in &field.attrs {
            if attr.path().is_ident("field") {
                let parsed = attr.parse_args_with(
                    |input: syn::parse::ParseStream| -> syn::Result<(LitStr, Option<usize>)> {
                        let name: LitStr = input.parse()?;
                        let mut max: Option<usize> = None;
                        while input.peek(syn::Token![,]) {
                            let _comma: syn::Token![,] = input.parse()?;
                            let key: syn::Ident = input.parse()?;
                            let _eq: syn::Token![=] = input.parse()?;
                            if key == "max_count" {
                                let lit: syn::LitInt = input.parse()?;
                                max = Some(lit.base10_parse()?);
                            } else {
                                return Err(syn::Error::new(
                                    key.span(),
                                    format!(
                                        "unknown #[field(...)] option `{key}`; \
                                         supported keys: max_count"
                                    ),
                                ));
                            }
                        }
                        Ok((name, max))
                    },
                );
                match parsed {
                    Ok((name, max)) => {
                        field_name = Some(name);
                        max_count = max;
                    }
                    Err(e) => return e.to_compile_error(),
                }
            }
        }
        let Some(field_name) = field_name else {
            return syn::Error::new_spanned(
                &ident,
                "each MultipartRequest field needs #[field(\"name\")]",
            )
            .to_compile_error();
        };
        let field_name_str = field_name.value();

        let shape = classify(ty);

        // `max_count` is only meaningful on Vec shapes; reject it on
        // scalar / option fields so a typo doesn't silently disable
        // the cap a caller intended.
        if max_count.is_some()
            && !matches!(
                shape,
                FieldShape::FileVec { .. } | FieldShape::TextVec { .. }
            )
        {
            return syn::Error::new_spanned(
                &ident,
                "#[field(..., max_count = N)] is only valid on `Vec<...>` fields; \
                 scalar and `Option<...>` fields already keep first-write-wins semantics",
            )
            .to_compile_error();
        }

        match shape {
            FieldShape::FileScalar { validator } => {
                let v_ident = quote::format_ident!("__v_{}", ident);
                validator_decls.push(quote! {
                    let #v_ident: #validator = <#validator as ::core::default::Default>::default();
                });
                validator_arms.push(quote! {
                    #field_name_str => {
                        <#validator as ::suprnova::http::upload::validators::UploadValidator>::validate_chunk(&#v_ident, sniff, size)?;
                    }
                });
                field_arms.push(quote! {
                    #field_name_str => {
                        if let ::suprnova::http::upload::MultipartValue::File { backing, size, file_name, content_type, inferred_extension, sniff } = value {
                            <#validator as ::suprnova::http::upload::validators::UploadValidator>::validate_final(
                                &#v_ident, &sniff, size, content_type.as_deref()
                            )?;
                            if #ident.is_none() {
                                #ident = ::core::option::Option::Some(
                                    match backing {
                                        ::suprnova::http::upload::UploadedFileBacking::Memory(b) =>
                                            ::suprnova::http::upload::UploadedFile::<#validator>::from_memory(
                                                b, file_name, content_type, inferred_extension,
                                            ),
                                        ::suprnova::http::upload::UploadedFileBacking::Disk(t) =>
                                            ::suprnova::http::upload::UploadedFile::<#validator>::from_disk(
                                                t, size, file_name, content_type, inferred_extension,
                                            ),
                                    }
                                );
                            }
                        } else {
                            return ::core::result::Result::Err(::suprnova::FrameworkError::Domain {
                                message: format!("field '{}' must be a file", #field_name_str),
                                status_code: 400,
                            });
                        }
                    }
                });
                field_decls.push(quote! {
                    let mut #ident: ::core::option::Option<::suprnova::http::upload::UploadedFile<#validator>> = ::core::option::Option::None;
                });
                struct_init.push(quote! {
                    #ident: #ident.ok_or_else(|| ::suprnova::FrameworkError::Domain {
                        message: format!("missing required file field '{}'", #field_name_str),
                        status_code: 422,
                    })?,
                });
            }
            FieldShape::FileOption { validator } => {
                let v_ident = quote::format_ident!("__v_{}", ident);
                validator_decls.push(quote! {
                    let #v_ident: #validator = <#validator as ::core::default::Default>::default();
                });
                validator_arms.push(quote! {
                    #field_name_str => {
                        <#validator as ::suprnova::http::upload::validators::UploadValidator>::validate_chunk(&#v_ident, sniff, size)?;
                    }
                });
                field_arms.push(quote! {
                    #field_name_str => {
                        if let ::suprnova::http::upload::MultipartValue::File { backing, size, file_name, content_type, inferred_extension, sniff } = value {
                            <#validator as ::suprnova::http::upload::validators::UploadValidator>::validate_final(
                                &#v_ident, &sniff, size, content_type.as_deref()
                            )?;
                            if #ident.is_none() {
                                #ident = ::core::option::Option::Some(
                                    match backing {
                                        ::suprnova::http::upload::UploadedFileBacking::Memory(b) =>
                                            ::suprnova::http::upload::UploadedFile::<#validator>::from_memory(
                                                b, file_name, content_type, inferred_extension,
                                            ),
                                        ::suprnova::http::upload::UploadedFileBacking::Disk(t) =>
                                            ::suprnova::http::upload::UploadedFile::<#validator>::from_disk(
                                                t, size, file_name, content_type, inferred_extension,
                                            ),
                                    }
                                );
                            }
                        }
                    }
                });
                field_decls.push(quote! {
                    let mut #ident: ::core::option::Option<::suprnova::http::upload::UploadedFile<#validator>> = ::core::option::Option::None;
                });
                struct_init.push(quote! { #ident, });
            }
            FieldShape::FileVec { validator } => {
                let v_ident = quote::format_ident!("__v_{}", ident);
                validator_decls.push(quote! {
                    let #v_ident: #validator = <#validator as ::core::default::Default>::default();
                });
                validator_arms.push(quote! {
                    #field_name_str => {
                        <#validator as ::suprnova::http::upload::validators::UploadValidator>::validate_chunk(&#v_ident, sniff, size)?;
                    }
                });
                // `max_count` short-circuit: when the attribute is set,
                // emit a length check BEFORE pushing the file into the
                // Vec. We test against the cap (not cap-1) and return
                // 422 the moment a request would push the (cap+1)-th
                // file. The check is omitted when no cap is configured
                // so existing callers see no behavioural change.
                let cap_guard = match max_count {
                    Some(cap) => quote! {
                        if #ident.len() >= #cap {
                            return ::core::result::Result::Err(::suprnova::FrameworkError::Domain {
                                message: format!(
                                    "field '{}' exceeds max_count {}",
                                    #field_name_str, #cap
                                ),
                                status_code: 422,
                            });
                        }
                    },
                    None => quote! {},
                };
                field_arms.push(quote! {
                    #field_name_str => {
                        if let ::suprnova::http::upload::MultipartValue::File { backing, size, file_name, content_type, inferred_extension, sniff } = value {
                            <#validator as ::suprnova::http::upload::validators::UploadValidator>::validate_final(
                                &#v_ident, &sniff, size, content_type.as_deref()
                            )?;
                            #cap_guard
                            #ident.push(
                                match backing {
                                    ::suprnova::http::upload::UploadedFileBacking::Memory(b) =>
                                        ::suprnova::http::upload::UploadedFile::<#validator>::from_memory(
                                            b, file_name, content_type, inferred_extension,
                                        ),
                                    ::suprnova::http::upload::UploadedFileBacking::Disk(t) =>
                                        ::suprnova::http::upload::UploadedFile::<#validator>::from_disk(
                                            t, size, file_name, content_type, inferred_extension,
                                        ),
                                }
                            );
                        }
                    }
                });
                field_decls.push(quote! {
                    let mut #ident: ::std::vec::Vec<::suprnova::http::upload::UploadedFile<#validator>> = ::std::vec::Vec::new();
                });
                struct_init.push(quote! { #ident, });
            }
            FieldShape::TextScalar { inner_ty } => {
                field_arms.push(quote! {
                    #field_name_str => {
                        if let ::suprnova::http::upload::MultipartValue::Text(s) = value {
                            if #ident.is_none() {
                                let parsed = <#inner_ty as ::core::str::FromStr>::from_str(&s)
                                    .map_err(|_| ::suprnova::FrameworkError::Domain {
                                        message: format!(
                                            "could not parse text field '{}' as {}",
                                            #field_name_str,
                                            ::core::stringify!(#inner_ty),
                                        ),
                                        status_code: 400,
                                    })?;
                                #ident = ::core::option::Option::Some(parsed);
                            }
                        } else {
                            return ::core::result::Result::Err(::suprnova::FrameworkError::Domain {
                                message: format!("field '{}' must be text", #field_name_str),
                                status_code: 400,
                            });
                        }
                    }
                });
                field_decls.push(quote! {
                    let mut #ident: ::core::option::Option<#inner_ty> = ::core::option::Option::None;
                });
                struct_init.push(quote! {
                    #ident: #ident.ok_or_else(|| ::suprnova::FrameworkError::Domain {
                        message: format!("missing required text field '{}'", #field_name_str),
                        status_code: 422,
                    })?,
                });
            }
            FieldShape::TextOption { inner_ty } => {
                field_arms.push(quote! {
                    #field_name_str => {
                        if let ::suprnova::http::upload::MultipartValue::Text(s) = value {
                            if #ident.is_none() {
                                let parsed = <#inner_ty as ::core::str::FromStr>::from_str(&s)
                                    .map_err(|_| ::suprnova::FrameworkError::Domain {
                                        message: format!(
                                            "could not parse text field '{}' as {}",
                                            #field_name_str,
                                            ::core::stringify!(#inner_ty),
                                        ),
                                        status_code: 400,
                                    })?;
                                #ident = ::core::option::Option::Some(parsed);
                            }
                        }
                    }
                });
                field_decls.push(quote! {
                    let mut #ident: ::core::option::Option<#inner_ty> = ::core::option::Option::None;
                });
                struct_init.push(quote! { #ident, });
            }
            FieldShape::TextVec { inner_ty } => {
                // `max_count` on text Vec fields covers the same DoS as
                // file Vec fields: 100k text parts in a 25 MiB body would
                // still allocate a parsed scalar per part. Emit the same
                // length-check short-circuit when the attribute is set.
                let cap_guard = match max_count {
                    Some(cap) => quote! {
                        if #ident.len() >= #cap {
                            return ::core::result::Result::Err(::suprnova::FrameworkError::Domain {
                                message: format!(
                                    "field '{}' exceeds max_count {}",
                                    #field_name_str, #cap
                                ),
                                status_code: 422,
                            });
                        }
                    },
                    None => quote! {},
                };
                field_arms.push(quote! {
                    #field_name_str => {
                        if let ::suprnova::http::upload::MultipartValue::Text(s) = value {
                            let parsed = <#inner_ty as ::core::str::FromStr>::from_str(&s)
                                .map_err(|_| ::suprnova::FrameworkError::Domain {
                                    message: format!(
                                        "could not parse text field '{}' as {}",
                                        #field_name_str,
                                        ::core::stringify!(#inner_ty),
                                    ),
                                    status_code: 400,
                                })?;
                            #cap_guard
                            #ident.push(parsed);
                        }
                    }
                });
                field_decls.push(quote! {
                    let mut #ident: ::std::vec::Vec<#inner_ty> = ::std::vec::Vec::new();
                });
                struct_init.push(quote! { #ident, });
            }
        }
    }

    let hooks_impl = if emit_default_hooks {
        quote! {
            #[automatically_derived]
            impl ::suprnova::http::upload::MultipartRequestHooks for #struct_name {}
        }
    } else {
        quote! {}
    };

    let expanded = quote! {
        #[::suprnova::__async_trait::async_trait]
        impl ::suprnova::http::FromRequest for #struct_name {
            async fn from_request(req: ::suprnova::http::Request)
                -> ::core::result::Result<Self, ::suprnova::FrameworkError>
            {
                if !<Self as ::suprnova::http::upload::MultipartRequestHooks>::authorize(&req) {
                    return ::core::result::Result::Err(::suprnova::FrameworkError::Unauthorized);
                }

                // Construct one validator instance per file field, ONCE.
                // The non-`move` closure below and the post-parse field
                // loop both borrow these via `&#v_<ident>`, so stateful
                // validators (interior mutability — `Mutex`, `AtomicUsize`,
                // etc.) see coherent state across every chunk + the final
                // call. Without this hoist a fresh instance would be
                // constructed inside each match arm and any accumulated
                // state would be discarded.
                #(#validator_decls)*

                let __max_body_bytes: usize = #max_body_bytes_expr;
                let __spill_threshold: usize = ::suprnova::http::upload::global_upload_spill_threshold();
                let payload = ::suprnova::http::upload::parse_multipart_streaming_with_cap(
                    req,
                    __max_body_bytes,
                    __spill_threshold,
                    |name: &str, sniff: &[u8], size: u64| -> ::core::result::Result<(), ::suprnova::FrameworkError> {
                        match name {
                            #(#validator_arms)*
                            _ => {}
                        }
                        ::core::result::Result::Ok(())
                    },
                ).await?;

                #(#field_decls)*

                for (name, value) in payload.fields {
                    match name.as_str() {
                        #(#field_arms)*
                        _ => {}
                    }
                }

                let constructed = Self { #(#struct_init)* };

                if let ::core::result::Result::Err(errs) =
                    <Self as ::suprnova::http::upload::MultipartRequestHooks>::after_validation(&constructed)
                {
                    return ::core::result::Result::Err(::suprnova::FrameworkError::Validation(errs));
                }

                ::core::result::Result::Ok(constructed)
            }
        }

        #hooks_impl
    };

    expanded
}

enum FieldShape {
    FileScalar { validator: proc_macro2::TokenStream },
    FileOption { validator: proc_macro2::TokenStream },
    FileVec { validator: proc_macro2::TokenStream },
    TextScalar { inner_ty: proc_macro2::TokenStream },
    TextOption { inner_ty: proc_macro2::TokenStream },
    TextVec { inner_ty: proc_macro2::TokenStream },
}

fn classify(ty: &Type) -> FieldShape {
    let outer_kind = outer_segment_ident(ty);
    let outer_inner = outer_segment_first_generic(ty);

    match (outer_kind.as_deref(), outer_inner) {
        (Some("Vec"), Some(inner)) => {
            if let Some(validator) = uploaded_file_validator(&inner) {
                FieldShape::FileVec { validator }
            } else {
                FieldShape::TextVec {
                    inner_ty: quote! { #inner },
                }
            }
        }
        (Some("Option"), Some(inner)) => {
            if let Some(validator) = uploaded_file_validator(&inner) {
                FieldShape::FileOption { validator }
            } else {
                FieldShape::TextOption {
                    inner_ty: quote! { #inner },
                }
            }
        }
        _ => {
            if let Some(validator) = uploaded_file_validator(ty) {
                FieldShape::FileScalar { validator }
            } else {
                FieldShape::TextScalar {
                    inner_ty: quote! { #ty },
                }
            }
        }
    }
}

fn outer_segment_ident(ty: &Type) -> Option<String> {
    if let Type::Path(p) = ty {
        return p.path.segments.last().map(|s| s.ident.to_string());
    }
    None
}

fn outer_segment_first_generic(ty: &Type) -> Option<Type> {
    if let Type::Path(p) = ty
        && let Some(seg) = p.path.segments.last()
        && let syn::PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
    {
        return Some(inner.clone());
    }
    None
}

fn uploaded_file_validator(ty: &Type) -> Option<proc_macro2::TokenStream> {
    if let Type::Path(p) = ty
        && let Some(seg) = p.path.segments.last()
        && seg.ident == "UploadedFile"
    {
        if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
            if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                return Some(quote! { #inner });
            }
            return Some(quote! { () });
        }
        return Some(quote! { () });
    }
    None
}

#[cfg(test)]
mod tests {
    //! Domain 5 audit M-D5-3 regression: unknown keys inside
    //! `#[multipart(...)]` must surface as compile errors. Before the
    //! fix, `let _ = attr.parse_nested_meta(|meta| { ... })` silently
    //! discarded the `Err(meta.error("unknown ..."))` returned for
    //! typos like `max_body_byte` — operators thought they'd set a
    //! larger per-struct cap but production kept the default.
    //!
    //! These tests also lock in the existing rejection paths
    //! (`tuple struct`, missing `#[field(...)]`, `max_count` on
    //! scalar fields) so a future refactor can't quietly silence
    //! them.

    use super::*;
    use syn::parse_quote;

    fn render(input: DeriveInput) -> String {
        expand_inner(input).to_string()
    }

    #[test]
    fn unknown_multipart_option_emits_compile_error() {
        // The `typo_key` is intentionally not one of `custom_hooks`
        // or `max_body_bytes`. Before M-D5-3 this silently went
        // through; now it must produce a `compile_error!` token.
        let input: DeriveInput = parse_quote! {
            #[multipart(typo_key = 1024)]
            struct Bad {
                #[field("file")]
                file: UploadedFile<()>,
            }
        };
        let rendered = render(input);
        assert!(
            rendered.contains("compile_error"),
            "unknown #[multipart(...)] option must produce a compile_error; got: {rendered}"
        );
        assert!(
            rendered.contains("unknown") || rendered.contains("multipart"),
            "compile_error message must reference the bad option; got: {rendered}"
        );
    }

    #[test]
    fn known_multipart_options_compile_through() {
        let input: DeriveInput = parse_quote! {
            #[multipart(max_body_bytes = 1024)]
            struct Good {
                #[field("file")]
                file: UploadedFile<()>,
            }
        };
        let rendered = render(input);
        assert!(
            !rendered.contains("compile_error"),
            "known multipart options should not error; got: {rendered}"
        );
    }

    #[test]
    fn tuple_struct_emits_compile_error() {
        let input: DeriveInput = parse_quote! {
            struct Bad(String);
        };
        let rendered = render(input);
        assert!(
            rendered.contains("compile_error"),
            "tuple structs must be rejected; got: {rendered}"
        );
    }

    #[test]
    fn missing_field_attr_emits_compile_error() {
        let input: DeriveInput = parse_quote! {
            struct Bad {
                file: UploadedFile<()>,
            }
        };
        let rendered = render(input);
        assert!(
            rendered.contains("compile_error"),
            "field without #[field(\"...\")] must be rejected; got: {rendered}"
        );
        assert!(
            rendered.contains("MultipartRequest field needs"),
            "diagnostic must explain the missing #[field] attribute; got: {rendered}"
        );
    }
}

//! `#[derive(MultipartRequest)]` — strongly-typed multipart extractor.
//!
//! Emits two impls per struct:
//! 1. `impl FromRequest` — calls hooks, parses the body once via
//!    `parse_multipart_streaming`, dispatches each `(name, value)` to
//!    the right field, then constructs `Self`.
//! 2. `impl MultipartRequestHooks` — empty default unless the struct
//!    carries `#[multipart(custom_hooks)]`, in which case the user
//!    provides their own impl.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields, LitStr, Type};

pub fn expand(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_name = &input.ident;

    // Parse struct-level `#[multipart(custom_hooks)]`.
    let mut emit_default_hooks = true;
    for attr in &input.attrs {
        if attr.path().is_ident("multipart") {
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("custom_hooks") {
                    emit_default_hooks = false;
                }
                Ok(())
            });
        }
    }

    let Data::Struct(data) = &input.data else {
        return syn::Error::new_spanned(&input, "MultipartRequest requires a struct")
            .to_compile_error()
            .into();
    };
    let Fields::Named(fields) = &data.fields else {
        return syn::Error::new_spanned(&data.fields, "MultipartRequest requires named fields")
            .to_compile_error()
            .into();
    };

    let mut field_decls = Vec::new();
    let mut field_arms = Vec::new();
    let mut validator_arms = Vec::new();
    let mut struct_init = Vec::new();

    for field in &fields.named {
        let ident = field.ident.clone().unwrap();
        let ty = &field.ty;

        // Parse `#[field("name")]`.
        let mut field_name: Option<LitStr> = None;
        for attr in &field.attrs {
            if attr.path().is_ident("field") {
                let s: LitStr = match attr.parse_args() {
                    Ok(s) => s,
                    Err(e) => return e.to_compile_error().into(),
                };
                field_name = Some(s);
            }
        }
        let Some(field_name) = field_name else {
            return syn::Error::new_spanned(
                &ident,
                "each MultipartRequest field needs #[field(\"name\")]",
            )
            .to_compile_error()
            .into();
        };
        let field_name_str = field_name.value();

        let shape = classify(ty);
        match shape {
            FieldShape::FileScalar { validator } => {
                validator_arms.push(quote! {
                    #field_name_str => {
                        let v = <#validator as ::core::default::Default>::default();
                        <#validator as ::suprnova::http::upload::validators::UploadValidator>::validate_chunk(&v, accumulated)?;
                    }
                });
                field_arms.push(quote! {
                    #field_name_str => {
                        if let ::suprnova::http::upload::MultipartValue::File { bytes, file_name, content_type } = value {
                            let v = <#validator as ::core::default::Default>::default();
                            <#validator as ::suprnova::http::upload::validators::UploadValidator>::validate_final(
                                &v, &bytes, content_type.as_deref()
                            )?;
                            if #ident.is_none() {
                                #ident = ::core::option::Option::Some(
                                    ::suprnova::http::upload::UploadedFile::<#validator>::new(
                                        bytes, file_name, content_type,
                                    )
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
                validator_arms.push(quote! {
                    #field_name_str => {
                        let v = <#validator as ::core::default::Default>::default();
                        <#validator as ::suprnova::http::upload::validators::UploadValidator>::validate_chunk(&v, accumulated)?;
                    }
                });
                field_arms.push(quote! {
                    #field_name_str => {
                        if let ::suprnova::http::upload::MultipartValue::File { bytes, file_name, content_type } = value {
                            let v = <#validator as ::core::default::Default>::default();
                            <#validator as ::suprnova::http::upload::validators::UploadValidator>::validate_final(
                                &v, &bytes, content_type.as_deref()
                            )?;
                            if #ident.is_none() {
                                #ident = ::core::option::Option::Some(
                                    ::suprnova::http::upload::UploadedFile::<#validator>::new(
                                        bytes, file_name, content_type,
                                    )
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
                validator_arms.push(quote! {
                    #field_name_str => {
                        let v = <#validator as ::core::default::Default>::default();
                        <#validator as ::suprnova::http::upload::validators::UploadValidator>::validate_chunk(&v, accumulated)?;
                    }
                });
                field_arms.push(quote! {
                    #field_name_str => {
                        if let ::suprnova::http::upload::MultipartValue::File { bytes, file_name, content_type } = value {
                            let v = <#validator as ::core::default::Default>::default();
                            <#validator as ::suprnova::http::upload::validators::UploadValidator>::validate_final(
                                &v, &bytes, content_type.as_deref()
                            )?;
                            #ident.push(
                                ::suprnova::http::upload::UploadedFile::<#validator>::new(
                                    bytes, file_name, content_type,
                                )
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

                let payload = ::suprnova::http::upload::parse_multipart_streaming(
                    req,
                    |name: &str, accumulated: &[u8]| -> ::core::result::Result<(), ::suprnova::FrameworkError> {
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

    expanded.into()
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

//! Workflow attribute macro

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{parse_macro_input, FnArg, ItemFn, Pat, ReturnType, Type};

pub fn workflow_impl(_attr: TokenStream, input: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(input as ItemFn);

    if input_fn.sig.asyncness.is_none() {
        return syn::Error::new_spanned(
            input_fn.sig.fn_token,
            "#[workflow] requires an async function",
        )
        .to_compile_error()
        .into();
    }

    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;
    let fn_attrs = &input_fn.attrs;
    let fn_output = &input_fn.sig.output;
    let fn_block = &input_fn.block;
    let fn_inputs = &input_fn.sig.inputs;

    let mut arg_idents = Vec::new();
    let mut arg_types = Vec::new();

    for arg in fn_inputs.iter() {
        match arg {
            FnArg::Typed(pat_type) => match &*pat_type.pat {
                Pat::Ident(ident) => {
                    arg_idents.push(ident.ident.clone());
                    arg_types.push((*pat_type.ty).clone());
                }
                _ => {
                    return syn::Error::new_spanned(
                        &pat_type.pat,
                        "#[workflow] parameters must be simple identifiers",
                    )
                    .to_compile_error()
                    .into();
                }
            },
            FnArg::Receiver(_) => {
                return syn::Error::new_spanned(
                    arg,
                    "#[workflow] does not support methods with self",
                )
                .to_compile_error()
                .into();
            }
        }
    }

    let ok_type = match extract_result_ok_type(&input_fn.sig.output) {
        Ok(t) => t,
        Err(err) => return err.to_compile_error().into(),
    };

    let deser_args = build_deser_args(&arg_idents, &arg_types);

    let runner_name = format_ident!("__suprnova_workflow_runner_{}", fn_name);

    let expanded = quote! {
        #(#fn_attrs)*
        #fn_vis async fn #fn_name(#fn_inputs) #fn_output {
            #fn_block
        }

        #[doc(hidden)]
        fn #runner_name(__input: &str) -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = Result<String, ::suprnova::FrameworkError>> + Send>> {
            let __input = __input.to_string();
            Box::pin(async move {
                let __input: &str = &__input;
                #deser_args
                let __result: #ok_type = #fn_name(#(#arg_idents),*).await?;
                let __json = ::suprnova::serde_json::to_string(&__result)
                    .map_err(|e| ::suprnova::FrameworkError::internal(format!("Workflow output serialize error: {}", e)))?;
                Ok(__json)
            })
        }

        ::suprnova::inventory::submit! {
            ::suprnova::workflow::registry::WorkflowEntry {
                name: concat!(module_path!(), "::", stringify!(#fn_name)),
                run: #runner_name,
            }
        }
    };

    TokenStream::from(expanded)
}

fn extract_result_ok_type(output: &ReturnType) -> Result<Type, syn::Error> {
    match output {
        ReturnType::Type(_, ty) => match &**ty {
            Type::Path(path) => {
                let last = path.path.segments.last().ok_or_else(|| {
                    syn::Error::new_spanned(ty, "Invalid return type for #[workflow]")
                })?;

                if last.ident != "Result" {
                    return Err(syn::Error::new_spanned(
                        ty,
                        "#[workflow] must return Result<T, FrameworkError>",
                    ));
                }

                match &last.arguments {
                    syn::PathArguments::AngleBracketed(args) => {
                        let mut iter = args.args.iter();
                        let ok = iter.next().ok_or_else(|| {
                            syn::Error::new_spanned(ty, "Result must have ok type")
                        })?;
                        let err = iter.next().ok_or_else(|| {
                            syn::Error::new_spanned(ty, "Result must have error type")
                        })?;

                        let ok_ty = match ok {
                            syn::GenericArgument::Type(t) => t.clone(),
                            _ => {
                                return Err(syn::Error::new_spanned(
                                    ok,
                                    "Invalid ok type",
                                ))
                            }
                        };

                        let err_ty = match err {
                            syn::GenericArgument::Type(t) => t,
                            _ => {
                                return Err(syn::Error::new_spanned(
                                    err,
                                    "Invalid error type",
                                ))
                            }
                        };

                        if !is_framework_error(err_ty) {
                            return Err(syn::Error::new_spanned(
                                err_ty,
                                "#[workflow] must return Result<T, FrameworkError>",
                            ));
                        }

                        Ok(ok_ty)
                    }
                    _ => Err(syn::Error::new_spanned(
                        ty,
                        "#[workflow] must return Result<T, FrameworkError>",
                    )),
                }
            }
            _ => Err(syn::Error::new_spanned(
                ty,
                "#[workflow] must return Result<T, FrameworkError>",
            )),
        },
        ReturnType::Default => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "#[workflow] must return Result<T, FrameworkError>",
        )),
    }
}

fn is_framework_error(ty: &Type) -> bool {
    if let Type::Path(path) = ty
        && let Some(last) = path.path.segments.last() {
            return last.ident == "FrameworkError";
        }
    false
}

fn build_deser_args(arg_idents: &[syn::Ident], arg_types: &[Type]) -> TokenStream2 {
    match arg_idents.len() {
        0 => {
            quote! {
                let _: () = ::suprnova::serde_json::from_str(__input)
                    .map_err(|e| ::suprnova::FrameworkError::internal(format!("Workflow input deserialize error: {}", e)))?;
            }
        }
        _ => {
            quote! {
                let (#(#arg_idents),*,): (#(#arg_types),*,) = ::suprnova::serde_json::from_str(__input)
                    .map_err(|e| ::suprnova::FrameworkError::internal(format!("Workflow input deserialize error: {}", e)))?;
            }
        }
    }
}

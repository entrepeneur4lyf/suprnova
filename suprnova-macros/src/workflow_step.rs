//! Workflow step attribute macro

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{parse_macro_input, FnArg, ItemFn, Pat, ReturnType, Type};

pub fn workflow_step_impl(_attr: TokenStream, input: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(input as ItemFn);

    if input_fn.sig.asyncness.is_none() {
        return syn::Error::new_spanned(
            input_fn.sig.fn_token,
            "#[workflow_step] requires an async function",
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

    if let Err(err) = ensure_result_framework_error(fn_output) {
        return err.to_compile_error().into();
    }

    let mut arg_idents = Vec::new();
    for arg in fn_inputs.iter() {
        match arg {
            FnArg::Typed(pat_type) => match &*pat_type.pat {
                Pat::Ident(ident) => arg_idents.push(ident.ident.clone()),
                _ => {
                    return syn::Error::new_spanned(
                        &pat_type.pat,
                        "#[workflow_step] parameters must be simple identifiers",
                    )
                    .to_compile_error()
                    .into();
                }
            },
            FnArg::Receiver(_) => {
                return syn::Error::new_spanned(
                    arg,
                    "#[workflow_step] does not support methods with self",
                )
                .to_compile_error()
                .into();
            }
        }
    }

    let inner_name = format_ident!("__suprnova_workflow_step_inner_{}", fn_name);
    let input_json = build_input_json(&arg_idents);

    let expanded = quote! {
        #(#fn_attrs)*
        #fn_vis async fn #inner_name(#fn_inputs) #fn_output {
            #fn_block
        }

        #fn_vis async fn #fn_name(#fn_inputs) #fn_output {
            if let Some(ctx) = ::suprnova::workflow::WorkflowContext::current() {
                let __input_json = #input_json;
                ctx.run_step_with_input(
                    stringify!(#fn_name),
                    __input_json,
                    || async move { #inner_name(#(#arg_idents),*).await },
                )
                .await
            } else {
                #inner_name(#(#arg_idents),*).await
            }
        }
    };

    TokenStream::from(expanded)
}

fn ensure_result_framework_error(output: &ReturnType) -> Result<(), syn::Error> {
    match output {
        ReturnType::Type(_, ty) => match &**ty {
            Type::Path(path) => {
                let last = path.path.segments.last().ok_or_else(|| {
                    syn::Error::new_spanned(ty, "Invalid return type for #[workflow_step]")
                })?;

                if last.ident != "Result" {
                    return Err(syn::Error::new_spanned(
                        ty,
                        "#[workflow_step] must return Result<T, FrameworkError>",
                    ));
                }

                match &last.arguments {
                    syn::PathArguments::AngleBracketed(args) => {
                        let mut iter = args.args.iter();
                        iter.next().ok_or_else(|| {
                            syn::Error::new_spanned(ty, "Result must have ok type")
                        })?;
                        let err = iter.next().ok_or_else(|| {
                            syn::Error::new_spanned(ty, "Result must have error type")
                        })?;

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
                                "#[workflow_step] must return Result<T, FrameworkError>",
                            ));
                        }

                        Ok(())
                    }
                    _ => Err(syn::Error::new_spanned(
                        ty,
                        "#[workflow_step] must return Result<T, FrameworkError>",
                    )),
                }
            }
            _ => Err(syn::Error::new_spanned(
                ty,
                "#[workflow_step] must return Result<T, FrameworkError>",
            )),
        },
        ReturnType::Default => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "#[workflow_step] must return Result<T, FrameworkError>",
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

fn build_input_json(arg_idents: &[syn::Ident]) -> TokenStream2 {
    if arg_idents.is_empty() {
        quote! {
            ::suprnova::serde_json::to_string(&())
                .map_err(|e| ::suprnova::FrameworkError::internal(format!("Workflow step serialize error: {}", e)))?
        }
    } else {
        quote! {
            ::suprnova::serde_json::to_string(&(#(&#arg_idents),*,))
                .map_err(|e| ::suprnova::FrameworkError::internal(format!("Workflow step serialize error: {}", e)))?
        }
    }
}

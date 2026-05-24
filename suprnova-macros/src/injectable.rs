//! Injectable attribute macro for the Suprnova framework
//!
//! Provides the `#[injectable]` attribute macro that auto-registers
//! concrete types as singletons in the App container.
//!
//! Supports constructor injection via `#[inject]` field attribute.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, Fields, FieldsNamed};

/// Check if a field has the #[inject] attribute
fn has_inject_attr(field: &syn::Field) -> bool {
    field
        .attrs
        .iter()
        .any(|attr| attr.path().is_ident("inject"))
}

/// Implements the `#[injectable]` attribute macro
///
/// This macro automatically:
/// 1. Derives `Clone` for the type (and `Default` if no `#[inject]` fields)
/// 2. Registers the type as a singleton in the App container at startup
/// 3. For structs with `#[inject]` fields, resolves dependencies at registration time
///
/// # Example - Simple (no dependencies)
///
/// ```rust,ignore
/// use suprnova::injectable;
///
/// #[injectable]
/// pub struct AppState {
///     pub counter: u32,
/// }
///
/// // Automatically registered at startup with Default::default()
/// // Resolve via:
/// let state: AppState = App::get().unwrap();
/// ```
///
/// # Example - With Dependencies
///
/// ```rust,ignore
/// use suprnova::injectable;
///
/// #[injectable]
/// pub struct MyService {
///     #[inject]
///     config: AppConfig,
///     #[inject]
///     logger: LoggerService,
/// }
///
/// // Dependencies are resolved at startup
/// // Resolve via:
/// let service: MyService = App::get().unwrap();
/// ```
pub fn injectable_impl(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let name_str = name.to_string();
    let vis = &input.vis;
    let attrs = &input.attrs;
    let generics = &input.generics;

    let expanded = match &input.data {
        syn::Data::Struct(data_struct) => {
            match &data_struct.fields {
                Fields::Named(fields_named) => {
                    generate_for_named_struct(name, name_str, vis, attrs, generics, fields_named)
                }
                Fields::Unit => {
                    // Unit struct - use Default. No `#[inject]` fields so the
                    // closure never needs to resolve anything from the
                    // container and always returns Ok.
                    quote! {
                        #(#attrs)*
                        #[derive(Default, Clone)]
                        #vis struct #name #generics;

                        ::suprnova::inventory::submit! {
                            ::suprnova::container::provider::SingletonEntry {
                                register: || -> ::std::result::Result<(), ::std::string::String> {
                                    // singleton_if_absent — boot is idempotent
                                    // and does not clobber manual overrides or
                                    // stateful instances installed before boot.
                                    ::suprnova::App::singleton_if_absent(<#name as ::std::default::Default>::default());
                                    ::std::result::Result::Ok(())
                                },
                                name: #name_str,
                            }
                        }
                    }
                }
                Fields::Unnamed(_) => syn::Error::new_spanned(
                    &input,
                    "injectable does not support tuple structs. Use named fields instead.",
                )
                .to_compile_error(),
            }
        }
        _ => syn::Error::new_spanned(&input, "injectable can only be used on structs")
            .to_compile_error(),
    };

    TokenStream::from(expanded)
}

fn generate_for_named_struct(
    name: &syn::Ident,
    name_str: String,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    generics: &syn::Generics,
    fields_named: &FieldsNamed,
) -> proc_macro2::TokenStream {
    let fields = &fields_named.named;

    // Check if any fields have #[inject] attribute
    let has_injected_fields = fields.iter().any(has_inject_attr);

    if has_injected_fields {
        // Generate code for structs with injected dependencies
        generate_with_injection(name, name_str, vis, attrs, generics, fields_named)
    } else {
        // Generate code for simple structs (use Default)
        let fields_without_inject: Vec<_> = fields.iter().collect();

        quote! {
            #(#attrs)*
            #[derive(Default, Clone)]
            #vis struct #name #generics {
                #(#fields_without_inject),*
            }

            ::suprnova::inventory::submit! {
                ::suprnova::container::provider::SingletonEntry {
                    register: || -> ::std::result::Result<(), ::std::string::String> {
                        // singleton_if_absent — boot is idempotent and does
                        // not clobber manual overrides. No `#[inject]` fields,
                        // so we always return Ok.
                        ::suprnova::App::singleton_if_absent(<#name as ::std::default::Default>::default());
                        ::std::result::Result::Ok(())
                    },
                    name: #name_str,
                }
            }
        }
    }
}

fn generate_with_injection(
    name: &syn::Ident,
    name_str: String,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    generics: &syn::Generics,
    fields_named: &FieldsNamed,
) -> proc_macro2::TokenStream {
    let fields = &fields_named.named;

    // Separate injected and non-injected fields
    let mut field_definitions = Vec::new();
    let mut field_initializations = Vec::new();

    for field in fields {
        let field_name = field.ident.as_ref().unwrap();
        let field_ty = &field.ty;
        let field_vis = &field.vis;

        // Filter out #[inject] attribute from field definition
        let other_attrs: Vec<_> = field
            .attrs
            .iter()
            .filter(|attr| !attr.path().is_ident("inject"))
            .collect();

        field_definitions.push(quote! {
            #(#other_attrs)*
            #field_vis #field_name: #field_ty
        });

        if has_inject_attr(field) {
            // This field needs to be resolved from the container.
            //
            // We propagate via `?` so the bootstrap loop can distinguish
            // "still waiting on a dependency" from genuine missing/cyclic
            // deps. Inventory iteration order is implementation-defined,
            // so a producer #[injectable] may register after us on the
            // first pass — returning Err here makes the bootstrap loop
            // retry this entry on the next iteration, by which point the
            // producer is installed.
            field_initializations.push(quote! {
                #field_name: ::suprnova::App::resolve::<#field_ty>()
                    .map_err(|e| ::std::format!(
                        "failed to resolve dependency `{}` for `{}`: {}",
                        ::std::stringify!(#field_ty),
                        #name_str,
                        e,
                    ))?
            });
        } else {
            // Use Default for non-injected fields
            field_initializations.push(quote! {
                #field_name: ::std::default::Default::default()
            });
        }
    }

    quote! {
        #(#attrs)*
        #[derive(Clone)]
        #vis struct #name #generics {
            #(#field_definitions),*
        }

        impl #name {
            /// Resolve all dependencies and create an instance.
            ///
            /// Returns `Err` if any `#[inject]` field's dependency isn't
            /// installed yet. The bootstrap loop retries failed entries on
            /// the next iteration so inventory iteration order doesn't
            /// matter for inter-injectable resolution.
            fn __resolve_dependencies() -> ::std::result::Result<Self, ::std::string::String> {
                ::std::result::Result::Ok(Self {
                    #(#field_initializations),*
                })
            }
        }

        ::suprnova::inventory::submit! {
            ::suprnova::container::provider::SingletonEntry {
                register: || -> ::std::result::Result<(), ::std::string::String> {
                    // singleton_if_absent — boot is idempotent and does not
                    // clobber manual overrides. `?` defers to the bootstrap
                    // loop if a dependency isn't installed yet.
                    let instance = #name::__resolve_dependencies()?;
                    ::suprnova::App::singleton_if_absent(instance);
                    ::std::result::Result::Ok(())
                },
                name: #name_str,
            }
        }
    }
}

//! Domain error attribute macro for the Suprnova framework
//!
//! Provides the `#[domain_error]` attribute macro that generates
//! error types with automatic HTTP response conversion.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, Expr, Lit, Meta};

/// Parse the attributes from #[domain_error(status = 404, message = "...")]
struct DomainErrorAttrs {
    status: u16,
    message: Option<String>,
}

impl Default for DomainErrorAttrs {
    fn default() -> Self {
        Self {
            status: 500,
            message: None,
        }
    }
}

fn parse_attrs(attr: TokenStream) -> DomainErrorAttrs {
    let mut result = DomainErrorAttrs::default();

    // Parse as a comma-separated list of key=value pairs
    let parser = syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated;
    let metas = match syn::parse::Parser::parse(parser, attr) {
        Ok(metas) => metas,
        Err(_) => return result,
    };

    for meta in metas {
        if let Meta::NameValue(nv) = meta {
            let key = nv.path.get_ident().map(|i| i.to_string());

            match key.as_deref() {
                Some("status") => {
                    if let Expr::Lit(expr_lit) = &nv.value
                        && let Lit::Int(lit_int) = &expr_lit.lit
                            && let Ok(val) = lit_int.base10_parse::<u16>() {
                                result.status = val;
                            }
                }
                Some("message") => {
                    if let Expr::Lit(expr_lit) = &nv.value
                        && let Lit::Str(lit_str) = &expr_lit.lit {
                            result.message = Some(lit_str.value());
                        }
                }
                _ => {}
            }
        }
    }

    result
}

/// Implements the `#[domain_error]` attribute macro
///
/// This macro automatically:
/// 1. Derives `Debug` and `Clone` for the type
/// 2. Implements `Display`, `Error`, and `HttpError` traits
/// 3. Implements `From<T> for FrameworkError` for seamless `?` usage
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::domain_error;
///
/// #[domain_error(status = 404, message = "User not found")]
/// pub struct UserNotFoundError {
///     pub user_id: i32,
/// }
///
/// // Usage in controller - just use ? operator
/// pub async fn get_user(id: i32) -> Result<User, FrameworkError> {
///     users.find(id).ok_or(UserNotFoundError { user_id: id })?
/// }
/// ```
///
/// # Attributes
///
/// - `status`: HTTP status code (default: 500)
/// - `message`: Error message for Display (default: struct name converted to sentence)
pub fn domain_error_impl(attr: TokenStream, input: TokenStream) -> TokenStream {
    let attrs = parse_attrs(attr);
    let input = parse_macro_input!(input as DeriveInput);

    let name = &input.ident;
    let vis = &input.vis;
    let user_attrs = &input.attrs;
    let generics = &input.generics;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let status_code = attrs.status;

    // Generate default message from struct name if not provided
    // e.g., "UserNotFoundError" -> "User not found error"
    let message = attrs.message.unwrap_or_else(|| {
        let name_str = name.to_string();
        // Convert CamelCase to sentence case
        let mut result = String::new();
        for (i, c) in name_str.chars().enumerate() {
            if c.is_uppercase() && i > 0 {
                result.push(' ');
                result.push(c.to_lowercase().next().unwrap());
            } else {
                result.push(c);
            }
        }
        result
    });

    let expanded = match &input.data {
        syn::Data::Struct(data_struct) => {
            let fields = &data_struct.fields;

            quote! {
                #(#user_attrs)*
                #[derive(Debug, Clone)]
                #vis struct #name #generics #fields

                impl #impl_generics ::std::fmt::Display for #name #ty_generics #where_clause {
                    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                        write!(f, #message)
                    }
                }

                impl #impl_generics ::std::error::Error for #name #ty_generics #where_clause {}

                impl #impl_generics ::suprnova::HttpError for #name #ty_generics #where_clause {
                    fn status_code(&self) -> u16 {
                        #status_code
                    }

                    fn error_message(&self) -> String {
                        self.to_string()
                    }
                }

                impl #impl_generics ::std::convert::From<#name #ty_generics> for ::suprnova::FrameworkError #where_clause {
                    fn from(e: #name #ty_generics) -> Self {
                        ::suprnova::FrameworkError::Domain {
                            message: e.to_string(),
                            status_code: #status_code,
                        }
                    }
                }
            }
        }
        _ => syn::Error::new_spanned(&input, "domain_error can only be used on structs")
            .to_compile_error(),
    };

    TokenStream::from(expanded)
}

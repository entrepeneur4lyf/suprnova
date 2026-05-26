//! Domain error attribute macro for the Suprnova framework
//!
//! Provides the `#[domain_error]` attribute macro that generates
//! error types with automatic HTTP response conversion.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{DeriveInput, Expr, Lit, Meta, parse_macro_input};

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

/// Parse `#[domain_error(status = 404, message = "...")]`.
///
/// Domain 5 audit M-D5-4: this used to silently swallow EVERY error
/// — bad punctuation in the attribute body, overflowed status values
/// (`status = 100000`), wrong literal types (`message = 42`), and
/// unknown keys all collapsed to the defaults with no compile error.
/// A user setting `status = 70_000` to mark "rate-limited" expected
/// 429 behaviour and got 500 instead, with no signal anything was
/// wrong. Errors now propagate through `syn::Result` so the user
/// sees a span-pointed compile error at the offending key/value.
fn parse_attrs(attr: TokenStream2) -> syn::Result<DomainErrorAttrs> {
    let mut result = DomainErrorAttrs::default();

    let parser = syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated;
    let metas = syn::parse::Parser::parse2(parser, attr)?;

    for meta in metas {
        let nv = match &meta {
            Meta::NameValue(nv) => nv,
            _ => {
                return Err(syn::Error::new_spanned(
                    &meta,
                    "#[domain_error(...)] expects `key = value` pairs",
                ));
            }
        };

        let key = nv
            .path
            .get_ident()
            .ok_or_else(|| {
                syn::Error::new_spanned(
                    &nv.path,
                    "#[domain_error(...)] keys must be plain identifiers",
                )
            })?
            .to_string();

        match key.as_str() {
            "status" => {
                let Expr::Lit(expr_lit) = &nv.value else {
                    return Err(syn::Error::new_spanned(
                        &nv.value,
                        "`status` in #[domain_error(...)] expects an integer literal",
                    ));
                };
                let Lit::Int(lit_int) = &expr_lit.lit else {
                    return Err(syn::Error::new_spanned(
                        &expr_lit.lit,
                        "`status` in #[domain_error(...)] expects an integer literal",
                    ));
                };
                let val = lit_int.base10_parse::<u16>().map_err(|e| {
                    syn::Error::new_spanned(
                        lit_int,
                        format!(
                            "`status` in #[domain_error(...)] must fit in u16 \
                             (HTTP status codes are 100-599): {e}"
                        ),
                    )
                })?;
                result.status = val;
            }
            "message" => {
                let Expr::Lit(expr_lit) = &nv.value else {
                    return Err(syn::Error::new_spanned(
                        &nv.value,
                        "`message` in #[domain_error(...)] expects a string literal",
                    ));
                };
                let Lit::Str(lit_str) = &expr_lit.lit else {
                    return Err(syn::Error::new_spanned(
                        &expr_lit.lit,
                        "`message` in #[domain_error(...)] expects a string literal",
                    ));
                };
                result.message = Some(lit_str.value());
            }
            other => {
                return Err(syn::Error::new_spanned(
                    &nv.path,
                    format!(
                        "unknown key `{other}` in #[domain_error(...)] — \
                         supported keys: status, message"
                    ),
                ));
            }
        }
    }

    Ok(result)
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
    let attrs = match parse_attrs(attr.into()) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };
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

#[cfg(test)]
mod tests {
    //! Domain 5 audit M-D5-4 regression: `#[domain_error(...)]`
    //! attribute parsing must NOT silently swallow malformed input,
    //! overflowed status values, wrong literal types, or unknown
    //! keys. Each was previously a default-fallback; now each is a
    //! span-pointed compile error.

    use super::*;

    fn ok_attrs(src: &str) -> DomainErrorAttrs {
        let tokens: proc_macro2::TokenStream = src.parse().expect("test attr parses as tokens");
        // `parse_attrs` takes a `proc_macro2::TokenStream` (aliased
        // `TokenStream2`), which is exactly what the tests build — no
        // conversion needed. The real macro entry point is what bridges
        // `proc_macro::TokenStream` into `proc_macro2` before calling in.
        parse_attrs(tokens).expect("attrs parse")
    }

    fn err_attrs(src: &str) -> String {
        let tokens: proc_macro2::TokenStream = src.parse().expect("test attr parses as tokens");
        parse_attrs(tokens)
            .err()
            .expect("attrs must reject")
            .to_string()
    }

    #[test]
    fn happy_path_status_and_message() {
        let attrs = ok_attrs("status = 404, message = \"User not found\"");
        assert_eq!(attrs.status, 404);
        assert_eq!(attrs.message.as_deref(), Some("User not found"));
    }

    #[test]
    fn overflow_status_now_rejected() {
        // 70_000 doesn't fit in u16; old behaviour silently fell
        // through to the default 500.
        let msg = err_attrs("status = 70000, message = \"x\"");
        assert!(
            msg.contains("u16"),
            "overflow status must mention u16; got: {msg}"
        );
    }

    #[test]
    fn wrong_literal_type_now_rejected() {
        // `message = 42` is an integer where a string literal is
        // expected; old behaviour silently dropped it and built the
        // sentence-cased fallback from the struct name.
        let msg = err_attrs("status = 404, message = 42");
        assert!(
            msg.contains("string literal"),
            "wrong message type must reject; got: {msg}"
        );
    }

    #[test]
    fn unknown_key_now_rejected() {
        // `code` instead of `status` was previously silently ignored.
        let msg = err_attrs("code = 404");
        assert!(
            msg.contains("unknown key") && msg.contains("code"),
            "unknown key must name itself in the error; got: {msg}"
        );
    }
}

//! `#[derive(NotificationMailable)]` — auto-generate `to_mail` for a
//! Notification.
//!
//! The derive reads a `#[mail(...)]` outer attribute describing the
//! rendered mail's subject and body (inline or template file), plus
//! optional sender + cc/bcc/reply_to lists. The generated `to_mail`
//! serializes `self`, runs each template through Tera, and assembles a
//! [`MailRendering`] without the caller writing any boilerplate.
//!
//! Templates ride compile-time: inline strings end up in the binary
//! verbatim, and file paths embed via `include_str!` (relative to the
//! source file containing the derive). The empty-body invariant —
//! "at least one of `html` / `text` must be present" — is enforced at
//! macro-expansion time so a misconfigured notification fails to build
//! instead of failing at dispatch.
//!
//! See the rustdoc on `suprnova::NotificationMailable` (the derive
//! re-export) for the full attribute reference and examples.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, Expr, Lit};

/// Parsed `#[mail(...)]` attribute payload.
#[derive(Default)]
struct MailAttrs {
    subject: Option<String>,
    html: Option<String>,
    html_template: Option<String>,
    text: Option<String>,
    text_template: Option<String>,
    from: Option<String>,
    from_name: Option<String>,
    cc: Option<String>,
    bcc: Option<String>,
    reply_to: Option<String>,
}

/// Parse the `#[mail(key = "value", ...)]` outer attribute.
/// Returns a syn::Error pointing at the offending span on bad input.
fn parse_mail_attr(input: &DeriveInput) -> syn::Result<MailAttrs> {
    let mut attrs = MailAttrs::default();
    let mut saw_attr = false;

    for attribute in &input.attrs {
        if !attribute.path().is_ident("mail") {
            continue;
        }
        saw_attr = true;

        attribute.parse_nested_meta(|nested| {
            let key = nested
                .path
                .get_ident()
                .ok_or_else(|| nested.error("expected identifier key in #[mail(...)]"))?
                .to_string();

            // Every supported key is a `key = "string literal"` pair.
            let value: Expr = nested.value()?.parse()?;
            let Expr::Lit(expr_lit) = &value else {
                return Err(nested.error(format!(
                    "`{key}` in #[mail(...)] expects a string literal"
                )));
            };
            let Lit::Str(s) = &expr_lit.lit else {
                return Err(nested.error(format!(
                    "`{key}` in #[mail(...)] expects a string literal"
                )));
            };
            let v = s.value();

            match key.as_str() {
                "subject" => attrs.subject = Some(v),
                "html" => attrs.html = Some(v),
                "html_template" => attrs.html_template = Some(v),
                "text" => attrs.text = Some(v),
                "text_template" => attrs.text_template = Some(v),
                "from" => attrs.from = Some(v),
                "from_name" => attrs.from_name = Some(v),
                "cc" => attrs.cc = Some(v),
                "bcc" => attrs.bcc = Some(v),
                "reply_to" => attrs.reply_to = Some(v),
                other => {
                    return Err(nested.error(format!(
                        "unknown key `{other}` in #[mail(...)] — \
                         supported keys: subject, html, html_template, text, \
                         text_template, from, from_name, cc, bcc, reply_to"
                    )));
                }
            }
            Ok(())
        })?;
    }

    if !saw_attr {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[derive(NotificationMailable)] requires a `#[mail(...)]` outer attribute on the struct",
        ));
    }

    Ok(attrs)
}

/// Validate the attribute combination at expansion time so a
/// misconfigured Notification fails to compile rather than failing at
/// dispatch. Mirrors the runtime empty-body guard in
/// `MailChannel::deliver`.
fn validate(attrs: &MailAttrs, input: &DeriveInput) -> syn::Result<()> {
    let span = input.ident.span();

    if attrs.subject.is_none() {
        return Err(syn::Error::new(
            span,
            "#[mail(...)] requires a `subject = \"...\"` key",
        ));
    }

    if attrs.html.is_some() && attrs.html_template.is_some() {
        return Err(syn::Error::new(
            span,
            "#[mail(...)] — `html` and `html_template` are mutually exclusive",
        ));
    }
    if attrs.text.is_some() && attrs.text_template.is_some() {
        return Err(syn::Error::new(
            span,
            "#[mail(...)] — `text` and `text_template` are mutually exclusive",
        ));
    }

    let has_html = attrs.html.is_some() || attrs.html_template.is_some();
    let has_text = attrs.text.is_some() || attrs.text_template.is_some();
    if !has_html && !has_text {
        return Err(syn::Error::new(
            span,
            "#[mail(...)] — must specify at least one of `html`, `text`, \
             `html_template`, or `text_template` (an empty-body mail is \
             refused at dispatch; fail at compile time instead)",
        ));
    }

    if attrs.from_name.is_some() && attrs.from.is_none() {
        return Err(syn::Error::new(
            span,
            "#[mail(...)] — `from_name` requires `from`",
        ));
    }

    Ok(())
}

/// Build a token stream that constructs `Some(rendered_string)` for an
/// inline template, `Some(rendered_string)` for an `include_str!` file
/// template, or `None` if neither key was set.
///
/// `label` is used in the runtime error message ("Tera html for X:")
/// so a Tera parse failure tells the operator which template broke.
fn body_expr(
    inline: Option<&str>,
    template: Option<&str>,
    label: &str,
    struct_name: &str,
) -> TokenStream2 {
    if let Some(src) = inline {
        quote! {
            ::std::option::Option::Some(
                ::suprnova::__tera::Tera::one_off(#src, &__ctx, false).map_err(|e| {
                    ::suprnova::FrameworkError::internal(
                        format!("Tera {} for {}: {e}", #label, #struct_name)
                    )
                })?
            )
        }
    } else if let Some(path) = template {
        quote! {
            ::std::option::Option::Some(
                ::suprnova::__tera::Tera::one_off(
                    include_str!(#path),
                    &__ctx,
                    false,
                ).map_err(|e| {
                    ::suprnova::FrameworkError::internal(
                        format!("Tera {} for {}: {e}", #label, #struct_name)
                    )
                })?
            )
        }
    } else {
        quote! { ::std::option::Option::None }
    }
}

/// Build the `from: Option<Address>` initializer.
fn from_expr(attrs: &MailAttrs) -> TokenStream2 {
    let Some(email) = &attrs.from else {
        return quote! { ::std::option::Option::None };
    };
    if let Some(name) = &attrs.from_name {
        quote! {
            ::std::option::Option::Some(
                ::suprnova::mail::Address::new(#email).with_name(#name)
            )
        }
    } else {
        quote! {
            ::std::option::Option::Some(::suprnova::mail::Address::new(#email))
        }
    }
}

/// Build a `Vec<Address>` literal from a comma-separated email string.
/// Empty / whitespace-only entries are skipped so trailing commas are
/// forgiven.
fn address_list_expr(list: Option<&str>) -> TokenStream2 {
    let Some(raw) = list else {
        return quote! { ::std::vec::Vec::new() };
    };
    let items: Vec<TokenStream2> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|email| {
            quote! { ::suprnova::mail::Address::new(#email) }
        })
        .collect();
    if items.is_empty() {
        quote! { ::std::vec::Vec::new() }
    } else {
        quote! { ::std::vec![ #(#items),* ] }
    }
}

pub fn derive_notification_mailable_impl(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let attrs = match parse_mail_attr(&input) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };
    if let Err(e) = validate(&attrs, &input) {
        return e.to_compile_error().into();
    }

    let name = &input.ident;
    let name_str = name.to_string();
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let subject_lit = attrs.subject.as_deref().expect("validated above");

    let html_expr = body_expr(attrs.html.as_deref(), attrs.html_template.as_deref(), "html", &name_str);
    let text_expr = body_expr(attrs.text.as_deref(), attrs.text_template.as_deref(), "text", &name_str);
    let from_init = from_expr(&attrs);
    let cc_init = address_list_expr(attrs.cc.as_deref());
    let bcc_init = address_list_expr(attrs.bcc.as_deref());
    let reply_to_init = address_list_expr(attrs.reply_to.as_deref());

    let expanded = quote! {
        impl #impl_generics ::suprnova::notifications::channels::mail::NotificationMailable
            for #name #ty_generics #where_clause
        {
            fn to_mail(&self) -> ::std::result::Result<
                ::suprnova::notifications::channels::mail::MailRendering,
                ::suprnova::FrameworkError,
            > {
                let __ctx_value = ::suprnova::serde_json::to_value(self).map_err(|e| {
                    ::suprnova::FrameworkError::internal(
                        format!("encode {} for mail rendering: {e}", #name_str)
                    )
                })?;
                let __ctx = ::suprnova::__tera::Context::from_value(__ctx_value).map_err(|e| {
                    ::suprnova::FrameworkError::internal(
                        format!("Tera context for {}: {e}", #name_str)
                    )
                })?;

                let __subject = ::suprnova::__tera::Tera::one_off(#subject_lit, &__ctx, false)
                    .map_err(|e| {
                        ::suprnova::FrameworkError::internal(
                            format!("Tera subject for {}: {e}", #name_str)
                        )
                    })?;

                ::std::result::Result::Ok(
                    ::suprnova::notifications::channels::mail::MailRendering {
                        subject: __subject,
                        html: #html_expr,
                        text: #text_expr,
                        from: #from_init,
                        cc: #cc_init,
                        bcc: #bcc_init,
                        reply_to: #reply_to_init,
                        attachments: ::std::vec::Vec::new(),
                    }
                )
            }
        }
    };

    expanded.into()
}

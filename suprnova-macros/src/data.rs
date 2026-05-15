//! `#[derive(Data)]` — composite derive that produces:
//! - `Serialize` (skipping `#[data(input_only)]` fields)
//! - `Deserialize` (rejecting payloads containing `#[data(output_only)]`
//!   fields; using `T::default()` for output_only fields on deserialize)
//! - `FormRequest` (with `authorize: true` default; see below)
//!
//! Plus: a per-struct `inventory::submit!` block registering
//! `#[data(allow_include)]` fields into the runtime allowlist.
//!
//! Task 7 emits the skeleton for non-generic structs; Task 15 extends
//! the same builders to thread `impl_generics` / `ty_generics` /
//! `where_clause` through every generated impl.
//!
//! # Validation
//!
//! `#[derive(Data)]` generates the `FormRequest` impl (with
//! `authorize: true` default) but does NOT generate `Validate`. Add
//! `#[derive(Validate)]` separately so `#[validate(...)]` attributes
//! stay visible at the field call site:
//!
//! ```ignore
//! #[derive(Data, Validate)]
//! struct CreateUser {
//!     #[validate(email)]
//!     email: String,
//! }
//! ```
//!
//! # Custom authorization
//!
//! By default, `FormRequest::authorize` returns `true`. To override,
//! add `#[data(custom_authorize)]` at the struct level — the derive
//! will skip emitting the `FormRequest` impl, letting you write your
//! own:
//!
//! ```ignore
//! #[derive(Data, Validate)]
//! #[data(custom_authorize)]
//! struct ProtectedDto { /* ... */ }
//!
//! impl FormRequest for ProtectedDto {
//!     fn authorize(req: &Request) -> bool { /* your logic */ }
//! }
//! ```
//!
//! # Optional and tri-state fields
//!
//! Fields typed `Option<T>` and `Field<T>` are treated as absent-defaulting:
//! when the key is missing from the payload, they produce `None` /
//! `Field::Absent` respectively (via `unwrap_or_default()`). This matches
//! serde-derive's behaviour for `Option` and is the correct semantic for
//! `Field<T>` PATCH use-cases.
//!
//! # Unknown fields
//!
//! Payload keys not matching any struct field are silently dropped (serde
//! default permissive behavior). This mirrors `#[derive(Deserialize)]`.
//! Strict-mode (deny-unknown-fields) is not yet supported — add a
//! `#[data(deny_unknown_fields)]` opt-in if/when needed.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, Data, DataStruct, DeriveInput, Field, Fields, Ident, Meta};

#[derive(Default)]
struct FieldOptions {
    input_only: bool,
    output_only: bool,
    allow_include: bool,
}

#[derive(Default)]
struct StructOptions {
    custom_authorize: bool,
}

fn parse_struct_options(attrs: &[syn::Attribute]) -> Result<StructOptions, syn::Error> {
    let mut opts = StructOptions::default();
    for attr in attrs {
        if !attr.path().is_ident("data") {
            continue;
        }
        let list = match &attr.meta {
            Meta::List(list) => list,
            _ => continue,
        };
        list.parse_nested_meta(|meta| {
            if meta.path.is_ident("custom_authorize") {
                opts.custom_authorize = true;
            } else {
                return Err(meta.error("unknown struct-level #[data(...)] flag"));
            }
            Ok(())
        })?;
    }
    Ok(opts)
}

fn build_form_request(struct_name: &Ident, struct_opts: &StructOptions) -> TokenStream2 {
    if struct_opts.custom_authorize {
        // User opted to provide their own FormRequest impl — emit nothing.
        return quote! {};
    }
    quote! {
        impl ::suprnova::http::FormRequest for #struct_name {
            fn authorize(_req: &::suprnova::Request) -> bool {
                true
            }
        }
    }
}

fn parse_field_options(field: &Field) -> Result<FieldOptions, syn::Error> {
    let mut opts = FieldOptions::default();
    for attr in &field.attrs {
        if !attr.path().is_ident("data") {
            continue;
        }
        let list = match &attr.meta {
            Meta::List(list) => list,
            _ => {
                return Err(syn::Error::new_spanned(
                    attr,
                    "expected `#[data(...)]` with a parenthesised list",
                ))
            }
        };
        list.parse_nested_meta(|meta| {
            if meta.path.is_ident("input_only") {
                opts.input_only = true;
            } else if meta.path.is_ident("output_only") {
                opts.output_only = true;
            } else if meta.path.is_ident("allow_include") {
                opts.allow_include = true;
            } else {
                return Err(meta.error(
                    "unknown #[data(...)] flag — expected input_only, output_only, or allow_include",
                ));
            }
            Ok(())
        })?;
    }
    if opts.input_only && opts.output_only {
        return Err(syn::Error::new_spanned(
            field,
            "a field cannot be both input_only and output_only",
        ));
    }
    Ok(opts)
}

pub fn derive_data_impl(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    // Generic-param threading lands in Task 15. Until then, generic
    // structs will produce confusing errors from the generated code —
    // tests in Task 7 only exercise non-generic forms.
    let struct_name = &input.ident;
    let struct_name_str = struct_name.to_string();

    let fields = match &input.data {
        Data::Struct(DataStruct {
            fields: Fields::Named(named),
            ..
        }) => &named.named,
        _ => {
            return syn::Error::new_spanned(
                &input,
                "#[derive(Data)] requires a struct with named fields",
            )
            .to_compile_error()
            .into();
        }
    };

    let mut parsed: Vec<(&Field, FieldOptions)> = Vec::with_capacity(fields.len());
    for f in fields {
        match parse_field_options(f) {
            Ok(opts) => parsed.push((f, opts)),
            Err(e) => return e.to_compile_error().into(),
        }
    }

    let struct_opts = match parse_struct_options(&input.attrs) {
        Ok(o) => o,
        Err(e) => return e.to_compile_error().into(),
    };

    let serialize_impl = build_serialize(struct_name, &parsed);
    let deserialize_impl = build_deserialize(struct_name, &struct_name_str, &parsed);
    let allowlist_registration = build_allowlist_registration(&struct_name_str, &parsed);
    let form_request_impl = build_form_request(struct_name, &struct_opts);

    let expanded = quote! {
        #serialize_impl
        #deserialize_impl
        #allowlist_registration
        #form_request_impl
    };

    expanded.into()
}

/// Returns `true` when the field type's last path segment is `Option` or
/// `Field`. Both types impl `Default` meaningfully (`None` / `Field::Absent`),
/// so an absent payload key should produce the default rather than a
/// `missing_field` error.
///
/// Last-segment matching accepts fully-qualified paths
/// (`std::option::Option<T>`, `suprnova::data::Field<T>`) as well as the
/// short forms. False positives require a user type named exactly `Option` or
/// `Field` — an acceptable and easily documented limitation.
fn is_option_or_field(ty: &syn::Type) -> bool {
    if let syn::Type::Path(p) = ty {
        if let Some(seg) = p.path.segments.last() {
            return seg.ident == "Option" || seg.ident == "Field";
        }
    }
    false
}

fn build_serialize(struct_name: &Ident, parsed: &[(&Field, FieldOptions)]) -> TokenStream2 {
    let output_fields: Vec<TokenStream2> = parsed
        .iter()
        .filter(|(_, opts)| !opts.input_only)
        .map(|(f, _)| {
            let ident = f.ident.as_ref().unwrap();
            let name = ident.to_string();
            quote! {
                ::serde::ser::SerializeStruct::serialize_field(&mut state, #name, &self.#ident)?;
            }
        })
        .collect();
    let field_count = output_fields.len();
    let name_str = struct_name.to_string();

    quote! {
        impl ::serde::Serialize for #struct_name {
            fn serialize<S: ::serde::Serializer>(&self, ser: S) -> ::core::result::Result<S::Ok, S::Error> {
                use ::serde::ser::SerializeStruct;
                let mut state = ser.serialize_struct(#name_str, #field_count)?;
                #(#output_fields)*
                state.end()
            }
        }
    }
}

fn build_deserialize(
    struct_name: &Ident,
    struct_name_str: &str,
    parsed: &[(&Field, FieldOptions)],
) -> TokenStream2 {
    let output_only_names: Vec<String> = parsed
        .iter()
        .filter(|(_, o)| o.output_only)
        .map(|(f, _)| f.ident.as_ref().unwrap().to_string())
        .collect();

    // Split input fields into two groups based on whether an absent key
    // should produce a missing_field error (required) or a Default value
    // (Option<T> and Field<T> — both impl Default meaningfully).
    let input_fields: Vec<(&Ident, &str, &syn::Type)> = parsed
        .iter()
        .filter(|(_, o)| !o.output_only)
        .map(|(f, _)| {
            let ident = f.ident.as_ref().unwrap();
            let name: &str = Box::leak(ident.to_string().into_boxed_str());
            (ident, name, &f.ty)
        })
        .collect();

    // Required fields — missing key is an error.
    let req_idents: Vec<&Ident> = input_fields
        .iter()
        .filter(|(_, _, ty)| !is_option_or_field(ty))
        .map(|(id, _, _)| *id)
        .collect();
    let req_names: Vec<&str> = input_fields
        .iter()
        .filter(|(_, _, ty)| !is_option_or_field(ty))
        .map(|(_, name, _)| *name)
        .collect();
    let req_types: Vec<&syn::Type> = input_fields
        .iter()
        .filter(|(_, _, ty)| !is_option_or_field(ty))
        .map(|(_, _, ty)| *ty)
        .collect();

    // Defaultable fields — missing key yields Default::default().
    let def_idents: Vec<&Ident> = input_fields
        .iter()
        .filter(|(_, _, ty)| is_option_or_field(ty))
        .map(|(id, _, _)| *id)
        .collect();
    let def_names: Vec<&str> = input_fields
        .iter()
        .filter(|(_, _, ty)| is_option_or_field(ty))
        .map(|(_, name, _)| *name)
        .collect();
    let def_types: Vec<&syn::Type> = input_fields
        .iter()
        .filter(|(_, _, ty)| is_option_or_field(ty))
        .map(|(_, _, ty)| *ty)
        .collect();

    let output_only_idents: Vec<&Ident> = parsed
        .iter()
        .filter(|(_, o)| o.output_only)
        .map(|(f, _)| f.ident.as_ref().unwrap())
        .collect();

    let visitor_name = quote::format_ident!("__{}DataVisitor", struct_name);

    quote! {
        impl<'de> ::serde::Deserialize<'de> for #struct_name {
            fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> ::core::result::Result<Self, D::Error> {
                struct #visitor_name;

                impl<'de> ::serde::de::Visitor<'de> for #visitor_name {
                    type Value = #struct_name;

                    fn expecting(&self, f: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
                        f.write_str(concat!("struct ", #struct_name_str))
                    }

                    fn visit_map<A: ::serde::de::MapAccess<'de>>(self, mut map: A) -> ::core::result::Result<#struct_name, A::Error> {
                        // Required fields — slot is None until the key appears.
                        #(let mut #req_idents: ::core::option::Option<#req_types> = None;)*
                        // Defaultable fields (Option<T>, Field<T>) — slot is
                        // None until the key appears; absent yields Default.
                        #(let mut #def_idents: ::core::option::Option<#def_types> = None;)*

                        while let Some(key) = map.next_key::<String>()? {
                            match key.as_str() {
                                #(
                                    #output_only_names => {
                                        return Err(<A::Error as ::serde::de::Error>::custom(
                                            format!(
                                                "field `{}` is output_only on `{}` and cannot be set from input",
                                                #output_only_names,
                                                #struct_name_str,
                                            )
                                        ));
                                    }
                                )*
                                #(
                                    #req_names => {
                                        #req_idents = Some(map.next_value()?);
                                    }
                                )*
                                #(
                                    #def_names => {
                                        #def_idents = Some(map.next_value()?);
                                    }
                                )*
                                _ => {
                                    // Unknown keys are silently dropped —
                                    // permissive / serde-default behaviour.
                                    let _: ::serde::de::IgnoredAny = map.next_value()?;
                                }
                            }
                        }

                        Ok(#struct_name {
                            // Required fields: missing key is an error.
                            #(
                                #req_idents: #req_idents
                                    .ok_or_else(|| <A::Error as ::serde::de::Error>::missing_field(#req_names))?,
                            )*
                            // Defaultable fields: missing key yields Default::default().
                            #(
                                #def_idents: #def_idents
                                    .unwrap_or_default(),
                            )*
                            #(
                                #output_only_idents: ::core::default::Default::default(),
                            )*
                        })
                    }
                }

                d.deserialize_map(#visitor_name)
            }
        }
    }
}

fn build_allowlist_registration(
    struct_name_str: &str,
    parsed: &[(&Field, FieldOptions)],
) -> TokenStream2 {
    let allow_include_names: Vec<String> = parsed
        .iter()
        .filter(|(_, o)| o.allow_include)
        .map(|(f, _)| f.ident.as_ref().unwrap().to_string())
        .collect();

    if allow_include_names.is_empty() {
        return quote! {};
    }

    // Register via `inventory` — the same plugin-registration crate used
    // by typetag, sqlx, and clap-derive. Unlike `#[ctor::ctor]`, it
    // survives `cargo test` symbol stripping because the linker sees
    // these as live data, not unused init functions.
    quote! {
        ::suprnova::inventory::submit! {
            ::suprnova::data::registry::AllowedIncludes {
                struct_name: #struct_name_str,
                fields: &[#(#allow_include_names),*],
            }
        }
    }
}

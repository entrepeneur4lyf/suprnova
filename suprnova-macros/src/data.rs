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

fn build_form_request(
    struct_name: &Ident,
    struct_opts: &StructOptions,
    impl_generics: &syn::ImplGenerics,
    ty_generics: &syn::TypeGenerics,
    where_clause: Option<&syn::WhereClause>,
) -> TokenStream2 {
    if struct_opts.custom_authorize {
        // User opted to provide their own FormRequest impl — emit nothing.
        return quote! {};
    }
    quote! {
        impl #impl_generics ::suprnova::http::FormRequest for #struct_name #ty_generics #where_clause {
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

/// Clones the generics and prepends a `'__de` lifetime parameter at position 0.
/// This lifetime is used as the Deserialize lifetime in the generated impl.
fn add_de_lifetime(g: &syn::Generics) -> syn::Generics {
    let mut g = g.clone();
    g.params.insert(0, syn::parse_quote!('__de));
    g
}

pub fn derive_data_impl(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let struct_name = &input.ident;
    let struct_name_str = struct_name.to_string();

    // Split the generics into the three pieces needed for impl headers.
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    // Build a version of the generics with `'__de` prepended for Deserialize.
    let de_generics = add_de_lifetime(&input.generics);
    let (de_impl_generics, _, _) = de_generics.split_for_impl();

    // Track whether there are any type params (not lifetime params) so we can
    // decide whether to emit a turbofish on the visitor construction.
    let has_type_params = input.generics.type_params().count() > 0;

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

    // If any non-output-only field has a reference type (`&T` / `&'a T`),
    // we skip generating the Deserialize impl.  Reference types cannot be
    // produced by serde's deserialization machinery in the general case
    // (serde only supports `&str` / `&[u8]` via borrowing).  Structs with
    // reference fields are typically only ever serialized, not deserialized.
    let has_reference_fields = parsed
        .iter()
        .filter(|(_, o)| !o.output_only)
        .any(|(f, _)| is_reference_type(&f.ty));

    let serialize_impl =
        build_serialize(struct_name, &impl_generics, &ty_generics, where_clause, &parsed);
    let deserialize_impl = if has_reference_fields {
        proc_macro2::TokenStream::new()
    } else {
        build_deserialize(
            struct_name,
            &struct_name_str,
            &de_impl_generics,
            &impl_generics,
            &ty_generics,
            where_clause,
            &parsed,
            has_type_params,
        )
    };
    let allowlist_registration = build_allowlist_registration(&struct_name_str, &parsed);
    // Skip FormRequest for generic structs (any type or lifetime params).
    // FormRequest requires DeserializeOwned + Send + Validate; these bounds
    // cannot be generically propagated without knowing the concrete type params.
    // Users who need FormRequest on a generic struct must write the impl manually
    // with the appropriate where bounds (e.g., `where T: Send + Validate + ...`).
    let is_generic = !input.generics.params.is_empty();
    let form_request_impl = if is_generic || has_reference_fields {
        proc_macro2::TokenStream::new()
    } else {
        build_form_request(struct_name, &struct_opts, &impl_generics, &ty_generics, where_clause)
    };

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

fn build_serialize(
    struct_name: &Ident,
    impl_generics: &syn::ImplGenerics,
    ty_generics: &syn::TypeGenerics,
    where_clause: Option<&syn::WhereClause>,
    parsed: &[(&Field, FieldOptions)],
) -> TokenStream2 {
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
        impl #impl_generics ::serde::Serialize for #struct_name #ty_generics #where_clause {
            fn serialize<__S: ::serde::Serializer>(&self, ser: __S) -> ::core::result::Result<__S::Ok, __S::Error> {
                use ::serde::ser::SerializeStruct;
                let mut state = ser.serialize_struct(#name_str, #field_count)?;
                #(#output_fields)*
                state.end()
            }
        }
    }
}

/// Returns `true` when the field's type is a reference (`&T` or `&'a T`).
/// Used in `derive_data_impl` to detect structs for which generating a
/// `Deserialize` impl would be unsound: serde only provides `Deserialize` for
/// `&str` / `&[u8]` via input borrowing, not for arbitrary `&'a T`.  When any
/// non-output-only field is a reference, both the `Deserialize` and
/// `FormRequest` impls are suppressed.
fn is_reference_type(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Reference(_))
}

fn build_deserialize(
    struct_name: &Ident,
    struct_name_str: &str,
    de_impl_generics: &syn::ImplGenerics,
    impl_generics: &syn::ImplGenerics,
    ty_generics: &syn::TypeGenerics,
    where_clause: Option<&syn::WhereClause>,
    parsed: &[(&Field, FieldOptions)],
    has_type_params: bool,
) -> TokenStream2 {
    let output_only_names: Vec<String> = parsed
        .iter()
        .filter(|(_, o)| o.output_only)
        .map(|(f, _)| f.ident.as_ref().unwrap().to_string())
        .collect();

    // Split input fields into groups based on how an absent key is handled.
    // Reference-typed fields (e.g. `&'a T`) cannot be deserialized in general;
    // they are treated as reference-backed (skipped from the key-match, given
    // Default::default() in the constructor — caller must accept this semantic).
    let input_fields: Vec<(&Ident, &str, &syn::Type)> = parsed
        .iter()
        .filter(|(_, o)| !o.output_only)
        .map(|(f, _)| {
            let ident = f.ident.as_ref().unwrap();
            let name: &str = Box::leak(ident.to_string().into_boxed_str());
            (ident, name, &f.ty)
        })
        .collect();

    // Required fields — missing key is an error (non-Option, non-Field).
    // Note: reference-typed fields never appear here because build_deserialize
    // is only called when has_reference_fields is false (see derive_data_impl).
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

    // Visitor construction: when there are type params, we need the turbofish to
    // disambiguate; for non-generic structs (or lifetime-only generics with no
    // type params), just construct without turbofish.
    let visitor_construction = if has_type_params {
        quote! { #visitor_name::#ty_generics { _marker: ::core::marker::PhantomData } }
    } else {
        quote! { #visitor_name { _marker: ::core::marker::PhantomData } }
    };

    quote! {
        impl #de_impl_generics ::serde::Deserialize<'__de> for #struct_name #ty_generics #where_clause {
            fn deserialize<__D: ::serde::Deserializer<'__de>>(__d: __D) -> ::core::result::Result<Self, __D::Error> {
                struct #visitor_name #impl_generics #where_clause {
                    _marker: ::core::marker::PhantomData<fn() -> #struct_name #ty_generics>,
                }

                impl #de_impl_generics ::serde::de::Visitor<'__de> for #visitor_name #ty_generics #where_clause {
                    type Value = #struct_name #ty_generics;

                    fn expecting(&self, f: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
                        f.write_str(concat!("struct ", #struct_name_str))
                    }

                    fn visit_map<__A: ::serde::de::MapAccess<'__de>>(self, mut map: __A) -> ::core::result::Result<#struct_name #ty_generics, __A::Error> {
                        // Required fields — slot is None until the key appears.
                        #(let mut #req_idents: ::core::option::Option<#req_types> = None;)*
                        // Defaultable fields (Option<T>, Field<T>) — slot is
                        // None until the key appears; absent yields Default.
                        #(let mut #def_idents: ::core::option::Option<#def_types> = None;)*

                        while let Some(key) = map.next_key::<String>()? {
                            match key.as_str() {
                                #(
                                    #output_only_names => {
                                        return Err(<__A::Error as ::serde::de::Error>::custom(
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
                                    .ok_or_else(|| <__A::Error as ::serde::de::Error>::missing_field(#req_names))?,
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

                __d.deserialize_map(#visitor_construction)
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

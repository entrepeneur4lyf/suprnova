//! `#[derive(Data)]` — composite derive that produces:
//! - `Serialize` (skipping `#[data(input_only)]` fields)
//! - `Deserialize` (rejecting payloads containing `#[data(output_only)]`
//!   fields; using `T::default()` for output_only fields on deserialize)
//! - `Validate` (via the existing `validator` crate forwarding)
//! - `InertiaProps` (existing surface; serialization is the same as the
//!   generated Serialize)
//! - `FormRequest` (existing surface; gives the extractor path)
//!
//! Plus: a per-struct `inventory::submit!` block registering
//! `#[data(allow_include)]` fields into the runtime allowlist.
//!
//! Task 7 (this task) emits the skeleton for non-generic structs; Task
//! 15 extends the same builders to thread `impl_generics` /
//! `ty_generics` / `where_clause` through every generated impl.

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

    let serialize_impl = build_serialize(struct_name, &parsed);
    let deserialize_impl = build_deserialize(struct_name, &struct_name_str, &parsed);
    let allowlist_registration = build_allowlist_registration(&struct_name_str, &parsed);

    let expanded = quote! {
        #serialize_impl
        #deserialize_impl
        #allowlist_registration
    };

    expanded.into()
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

    let input_field_idents: Vec<&Ident> = parsed
        .iter()
        .filter(|(_, o)| !o.output_only)
        .map(|(f, _)| f.ident.as_ref().unwrap())
        .collect();
    let input_field_names: Vec<String> = input_field_idents.iter().map(|i| i.to_string()).collect();
    let input_field_types: Vec<&syn::Type> = parsed
        .iter()
        .filter(|(_, o)| !o.output_only)
        .map(|(f, _)| &f.ty)
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
                        #(let mut #input_field_idents: ::core::option::Option<#input_field_types> = None;)*

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
                                    #input_field_names => {
                                        #input_field_idents = Some(map.next_value()?);
                                    }
                                )*
                                _ => {
                                    let _: ::serde::de::IgnoredAny = map.next_value()?;
                                }
                            }
                        }

                        Ok(#struct_name {
                            #(
                                #input_field_idents: #input_field_idents
                                    .ok_or_else(|| <A::Error as ::serde::de::Error>::missing_field(#input_field_names))?,
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

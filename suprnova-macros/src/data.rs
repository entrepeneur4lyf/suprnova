#![allow(clippy::collapsible_if)]
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
//! point the derive at a free function via `#[data(authorize = "path")]`:
//!
//! ```ignore
//! fn admins_only(req: &Request) -> bool {
//!     req.user().is_some_and(|u| u.is_admin())
//! }
//!
//! #[derive(Data, Validate)]
//! #[data(authorize = "admins_only")]
//! struct ProtectedDto { /* ... */ }
//! ```
//!
//! The derive keeps emitting the full `FormRequest` impl (body parsing,
//! validation, Precognition, route-param injection, `after_validation`
//! hook) and only routes `authorize` to the named function. The function
//! must have the signature `fn(req: &::suprnova::Request) -> bool`.
//!
//! The earlier `#[data(custom_authorize)]` flag — which suppressed the
//! whole `FormRequest` impl and forced callers to reimplement extraction,
//! parsing, validation, and Precognition by hand — is removed.
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
//! By default, payload keys that don't match any struct field are
//! REJECTED with `serde::de::Error::unknown_field(..)`. Client typos
//! and schema drift surface immediately at the boundary instead of
//! silently disappearing into a deserialized value.
//!
//! Response DTOs that read forward-compatible payloads from external
//! services (paginated API envelopes, third-party webhooks, etc.) can
//! opt into permissive behaviour with `#[data(allow_unknown_fields)]`
//! at the struct level:
//!
//! ```ignore
//! // Strict by default — `{"email": "a@b", "typo": "x"}` fails fast.
//! #[derive(Data, Validate)]
//! struct CreateUser {
//!     #[validate(email)]
//!     email: String,
//! }
//!
//! // Permissive opt-in — for response DTOs that may carry extra keys.
//! #[derive(Data)]
//! #[data(allow_unknown_fields)]
//! struct WebhookEvent {
//!     id: String,
//!     kind: String,
//! }
//! ```
//!
//! Strict mode emits a `serde::de::Error::unknown_field` from the generated
//! visitor when an unrecognised key is encountered. The error includes the
//! offending key and the list of known fields, and surfaces through the
//! `FormRequest` path as `FrameworkError::Domain { status_code: 422 }`
//! (parse errors map to 422 in `framework/src/http/body.rs::parse_json`).

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Data, DataStruct, DeriveInput, Field, Fields, Ident, Meta, parse_macro_input};

/// Flavors of lazy resolution for a `#[data(lazy)]` field.
///
/// - `Plain` / `Inertia` — standard lazy prop, gated behind `?include=`.
///   Both emit `PropEntry::LazyOwned`.
/// - `Deferred` — Inertia deferred prop (follow-up XHR). Emits
///   `PropEntry::DeferredOwned`.
/// - `Closure` — closure-resolved prop (resolves eagerly on initial load
///   in a future release; same runtime as LazyOwned for v1). Emits
///   `PropEntry::ClosureOwned`.
/// - `WhenLoaded(relation)` — delegates to the `when_loaded!` macro at
///   runtime to produce `Prop::Lazy` or `Prop::EagerNone` based on whether
///   the named relation is preloaded on the source entity.
#[derive(Default, Clone, Debug)]
enum LazyFlavor {
    #[default]
    Plain,
    Inertia,
    Deferred,
    Closure,
    WhenLoaded,
}

#[derive(Default)]
struct FieldOptions {
    input_only: bool,
    output_only: bool,
    allow_include: bool,
    /// `Some(None)` means bare `#[data(from_route_param)]` — use the field name.
    /// `Some(Some(s))` means `#[data(from_route_param("s"))]` — use the explicit key.
    /// `None` means the attribute was not present on this field.
    from_route_param: Option<Option<String>>,
    /// Set by `#[data(lazy)]` or derived from `#[data(auto_lazy)]` at the
    /// struct level when the field's type is `Prop<T>`.
    lazy: Option<LazyFlavor>,
}

#[derive(Default)]
struct StructOptions {
    auto_lazy: bool,
    /// `Some(path)` when `#[data(authorize = "path::to::fn")]` is present.
    /// The path is the user-supplied function the derive routes
    /// `FormRequest::authorize` through. The rest of the `FormRequest`
    /// impl (body parsing, validation, Precognition, route-param
    /// injection, `after_validation`) is always emitted.
    authorize_fn: Option<syn::ExprPath>,
    /// `#[data(allow_unknown_fields)]` — when set, the generated
    /// `Deserialize` visitor silently drops payload keys that don't
    /// match any struct field (the serde-default permissive behaviour).
    /// The DEFAULT is strict: unknown keys produce
    /// `serde::de::Error::unknown_field(..)` so client typos and
    /// schema drift surface immediately at the boundary. Opt out only
    /// for response DTOs read back from external services that may
    /// carry forward-compatible extra keys.
    allow_unknown_fields: bool,
    /// `Some(N)` when `#[data(max_body_bytes = N)]` is present — overrides
    /// the `FormRequest::max_body_bytes` trait default for this DTO.
    /// Honored by both the simple (no route-param) and the inlined-lifecycle
    /// (with route-param) `FormRequest` impl arms — the override is part
    /// of the trait surface, not a route-param-only feature.
    max_body_bytes: Option<u64>,
    /// `Some(...)` when `#[json_resource("...")]` is present on the struct.
    json_resource: Option<JsonResourceOptions>,
}

struct JsonResourceOptions {
    /// JSON:API `type` member emitted in the resource envelope.
    resource_type: String,
    /// Field name to use as the `id` member (default: "id").
    id_field: String,
}

fn parse_struct_options(attrs: &[syn::Attribute]) -> Result<StructOptions, syn::Error> {
    let mut opts = StructOptions::default();
    for attr in attrs {
        if attr.path().is_ident("data") {
            let list = match &attr.meta {
                Meta::List(list) => list,
                _ => continue,
            };
            list.parse_nested_meta(|meta| {
                if meta.path.is_ident("auto_lazy") {
                    opts.auto_lazy = true;
                } else if meta.path.is_ident("authorize") {
                    // #[data(authorize = "path::to::fn")] — route the
                    // generated `FormRequest::authorize` to a user
                    // function. Quoted string + `LitStr::parse::<ExprPath>`
                    // mirrors serde's `#[serde(with = "...")]` convention
                    // and keeps paths with `::` separators unambiguous in
                    // attribute syntax.
                    let lit: syn::LitStr = meta.value()?.parse()?;
                    let path: syn::ExprPath = lit.parse()?;
                    opts.authorize_fn = Some(path);
                } else if meta.path.is_ident("allow_unknown_fields") {
                    opts.allow_unknown_fields = true;
                } else if meta.path.is_ident("max_body_bytes") {
                    // #[data(max_body_bytes = N)] — overrides
                    // FormRequest::max_body_bytes for this DTO. N must
                    // be a non-zero integer literal.
                    let lit: syn::LitInt = meta.value()?.parse()?;
                    let value: u64 = lit.base10_parse().map_err(|e| {
                        syn::Error::new(
                            lit.span(),
                            format!(
                                "#[data(max_body_bytes = N)] — N must parse as u64: {}",
                                e
                            ),
                        )
                    })?;
                    if value == 0 {
                        return Err(meta.error(
                            "#[data(max_body_bytes = 0)] is not allowed — a zero-byte cap would reject every request; use a positive value (the trait default is 64 MiB)",
                        ));
                    }
                    opts.max_body_bytes = Some(value);
                } else if meta.path.is_ident("deny_unknown_fields") {
                    // `deny_unknown_fields` was the opt-in flag when
                    // permissive was the default. Strict is now the
                    // default (codex review finding #10), so this flag
                    // is a no-op kept only to keep older call sites
                    // compiling. Prefer removing the attribute entirely.
                } else if meta.path.is_ident("custom_authorize") {
                    // Hard-cut diagnostic: the old "skip the whole
                    // FormRequest impl" flag is gone (codex review
                    // finding #11). Migrate to `authorize = "fn"` so
                    // body parsing, validation, and Precognition still
                    // get generated for you.
                    return Err(meta.error(
                        "#[data(custom_authorize)] was removed (codex review finding #11) — use #[data(authorize = \"path::to::fn\")] instead; the derive keeps emitting the FormRequest impl and only routes authorize to your function",
                    ));
                } else {
                    return Err(meta.error(
                        "unknown struct-level #[data(...)] flag — expected auto_lazy, authorize, allow_unknown_fields, or max_body_bytes",
                    ));
                }
                Ok(())
            })?;
        } else if attr.path().is_ident("json_resource") {
            // Positional first argument is the resource type string;
            // optional keyword args follow (`id_field = "..."`).
            let list = match &attr.meta {
                Meta::List(list) => list,
                _ => {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "#[json_resource(...)] requires arguments — at minimum a type string, e.g. #[json_resource(\"users\")]",
                    ));
                }
            };

            let mut resource_type: Option<String> = None;
            let mut id_field = "id".to_string();
            let mut first = true;

            list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated,
            )?
            .into_iter()
            .try_for_each(|expr| -> Result<(), syn::Error> {
                if first {
                    first = false;
                    if let syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    }) = expr
                    {
                        resource_type = Some(s.value());
                        return Ok(());
                    } else {
                        return Err(syn::Error::new_spanned(
                            expr,
                            "first argument to #[json_resource(...)] must be a string literal: the resource type",
                        ));
                    }
                }
                // Subsequent args: keyword = value
                if let syn::Expr::Assign(assign) = expr {
                    let key = match assign.left.as_ref() {
                        syn::Expr::Path(p) => p
                            .path
                            .get_ident()
                            .ok_or_else(|| {
                                syn::Error::new_spanned(
                                    &assign.left,
                                    "expected a bare identifier on the LHS of #[json_resource(...)] keyword arg",
                                )
                            })?
                            .to_string(),
                        _ => {
                            return Err(syn::Error::new_spanned(
                                &assign.left,
                                "expected a bare identifier on the LHS of #[json_resource(...)] keyword arg",
                            ));
                        }
                    };
                    let value = match assign.right.as_ref() {
                        syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s),
                            ..
                        }) => s.value(),
                        _ => {
                            return Err(syn::Error::new_spanned(
                                &assign.right,
                                "#[json_resource(...)] keyword args must be string literals",
                            ));
                        }
                    };
                    match key.as_str() {
                        "id_field" => {
                            // Domain 5 audit M-D5-7 — `id_field = "..."`
                            // is emitted as `syn::Ident::new(&opts.id_field,
                            // Span::call_site())` at `data.rs:1112` so the
                            // generated `IntoJsonResource` impl can reference
                            // the matching struct field via `self.#id_field`.
                            // A non-ident value (`id_field = "user-id"`,
                            // `id_field = "user id"`) panics the macro
                            // instead of producing a clean compile error.
                            if syn::parse_str::<syn::Ident>(&value).is_err() {
                                return Err(syn::Error::new_spanned(
                                    &assign.right,
                                    format!(
                                        "`id_field = \"{value}\"` — must parse as a Rust \
                                         identifier because it names a struct field on the \
                                         resource type. Use a snake_case name without \
                                         dashes / spaces (e.g. \"user_id\")."
                                    ),
                                ));
                            }
                            id_field = value;
                        }
                        other => {
                            return Err(syn::Error::new_spanned(
                                &assign.left,
                                format!("unknown #[json_resource(...)] keyword '{}'; expected 'id_field'", other),
                            ));
                        }
                    }
                    Ok(())
                } else {
                    Err(syn::Error::new_spanned(
                        expr,
                        "expected `keyword = \"value\"` after the resource type in #[json_resource(...)]",
                    ))
                }
            })?;

            let resource_type = resource_type.ok_or_else(|| {
                syn::Error::new_spanned(
                    attr,
                    "#[json_resource(...)] requires a resource type as the first argument, e.g. #[json_resource(\"users\")]",
                )
            })?;
            opts.json_resource = Some(JsonResourceOptions {
                resource_type,
                id_field,
            });
        }
    }
    Ok(opts)
}

/// Classification of a field's type for route-param coercion.
#[derive(Clone, Copy)]
enum RouteParamKind {
    I64,
    U64,
    I32,
    U32,
    I128,
    U128,
    F64,
    F32,
    Bool,
    /// Everything else (String, Uuid, &str, custom types) — pass as JSON string.
    Str,
}

/// Classify the last path segment of a type into a `RouteParamKind`.
/// Wraps the inner type when the field is `Option<T>` or `Field<T>`.
fn classify_route_param_type(ty: &syn::Type) -> RouteParamKind {
    // Unwrap Option<T> and Field<T> to their inner type for classification.
    let inner = unwrap_single_generic(ty);
    last_ident_kind(inner)
}

/// If `ty` is `Path<T>` where Path ends in `Option` or `Field`, return the
/// first generic arg; otherwise return `ty` unchanged.
fn unwrap_single_generic(ty: &syn::Type) -> &syn::Type {
    if let syn::Type::Path(p) = ty {
        if let Some(seg) = p.path.segments.last() {
            if seg.ident == "Option" || seg.ident == "Field" {
                if let syn::PathArguments::AngleBracketed(ab) = &seg.arguments {
                    if let Some(syn::GenericArgument::Type(inner)) = ab.args.first() {
                        return inner;
                    }
                }
            }
        }
    }
    ty
}

fn last_ident_kind(ty: &syn::Type) -> RouteParamKind {
    if let syn::Type::Path(p) = ty {
        if let Some(seg) = p.path.segments.last() {
            return match seg.ident.to_string().as_str() {
                "i64" => RouteParamKind::I64,
                "u64" => RouteParamKind::U64,
                "i32" => RouteParamKind::I32,
                "u32" => RouteParamKind::U32,
                "i128" => RouteParamKind::I128,
                "u128" => RouteParamKind::U128,
                "f64" => RouteParamKind::F64,
                "f32" => RouteParamKind::F32,
                "bool" => RouteParamKind::Bool,
                _ => RouteParamKind::Str,
            };
        }
    }
    RouteParamKind::Str
}

/// Map a `RouteParamKind` to the fully-qualified coercer function path in the
/// `suprnova::data::route_params` module.
fn route_param_parser_path(kind: RouteParamKind) -> TokenStream2 {
    match kind {
        RouteParamKind::I64 => quote! { ::suprnova::data::route_params::parse_i64 },
        RouteParamKind::U64 => quote! { ::suprnova::data::route_params::parse_u64 },
        RouteParamKind::I32 => quote! { ::suprnova::data::route_params::parse_i32 },
        RouteParamKind::U32 => quote! { ::suprnova::data::route_params::parse_u32 },
        RouteParamKind::I128 => quote! { ::suprnova::data::route_params::parse_i128 },
        RouteParamKind::U128 => quote! { ::suprnova::data::route_params::parse_u128 },
        RouteParamKind::F64 => quote! { ::suprnova::data::route_params::parse_f64 },
        RouteParamKind::F32 => quote! { ::suprnova::data::route_params::parse_f32 },
        RouteParamKind::Bool => quote! { ::suprnova::data::route_params::parse_bool },
        RouteParamKind::Str => quote! { ::suprnova::data::route_params::pass_string },
    }
}

/// Returns `true` when the outermost type is `Option<_>` or `Field<_>`.
fn is_option_type(ty: &syn::Type) -> bool {
    if let syn::Type::Path(p) = ty {
        if let Some(seg) = p.path.segments.last() {
            return seg.ident == "Option" || seg.ident == "Field";
        }
    }
    false
}

/// Returns `true` when the outermost type is `Prop<_>`.
/// Used by `auto_lazy` logic to infer which fields should be treated
/// as lazy props without an explicit `#[data(lazy)]` annotation.
fn is_prop_type(ty: &syn::Type) -> bool {
    if let syn::Type::Path(p) = ty {
        if let Some(seg) = p.path.segments.last() {
            return seg.ident == "Prop";
        }
    }
    false
}

fn build_form_request(
    struct_name: &Ident,
    struct_opts: &StructOptions,
    impl_generics: &syn::ImplGenerics,
    ty_generics: &syn::TypeGenerics,
    where_clause: Option<&syn::WhereClause>,
    parsed: &[(&Field, FieldOptions)],
) -> TokenStream2 {
    // Build the `fn authorize` body. With `#[data(authorize = "path::fn")]`
    // we route to the user's function; without it, we keep Laravel's
    // permissive default (true). Either way the rest of the `FormRequest`
    // impl is emitted intact (body parsing, validation, Precognition,
    // route-param injection, after_validation hook) — the prior
    // `custom_authorize`-skips-everything trap is gone.
    let authorize_body = match &struct_opts.authorize_fn {
        Some(path) => quote! { #path(req) },
        None => quote! { true },
    };

    // Collect route-param injection snippets for any field with `from_route_param`.
    let route_param_injections: Vec<TokenStream2> = parsed
        .iter()
        .filter_map(|(f, opts)| {
            let param_spec = opts.from_route_param.as_ref()?;
            let field_ident = f.ident.as_ref().unwrap();
            let field_key = field_ident.to_string();
            // Use explicit name if provided, otherwise use the field name.
            let resolved_name = param_spec.clone().unwrap_or_else(|| field_key.clone());

            let parser = route_param_parser_path(classify_route_param_type(&f.ty));
            let optional = is_option_type(&f.ty);

            if optional {
                Some(quote! {
                    if let Some(raw) = route_snapshot.get(#resolved_name) {
                        let coerced = #parser(#resolved_name, raw)?;
                        map.insert(#field_key.to_string(), coerced);
                    }
                })
            } else {
                Some(quote! {
                    {
                        let raw = route_snapshot.get(#resolved_name)
                            .ok_or_else(|| ::suprnova::FrameworkError::bad_request(
                                format!("missing route param `{}`", #resolved_name)
                            ))?;
                        let coerced = #parser(#resolved_name, raw)?;
                        map.insert(#field_key.to_string(), coerced);
                    }
                })
            }
        })
        .collect();

    // `#[data(max_body_bytes = N)]` — emit an override that shadows the
    // trait default. Both the simple (no-op extract) and the inlined-
    // lifecycle (with route-param) arms emit it; the inlined arm calls
    // `Self::max_body_bytes()` directly, so the override propagates
    // automatically once present on the impl.
    let max_body_bytes_override: TokenStream2 = match struct_opts.max_body_bytes {
        Some(n) => {
            let lit = syn::LitInt::new(&n.to_string(), proc_macro2::Span::call_site());
            quote! {
                fn max_body_bytes() -> usize { #lit as usize }
            }
        }
        None => quote! {},
    };

    // If no fields use from_route_param, emit the simple no-op FormRequest impl
    // (delegates to the trait's default `extract` which reads the body normally).
    if route_param_injections.is_empty() {
        return quote! {
            impl #impl_generics ::suprnova::http::FormRequest for #struct_name #ty_generics #where_clause {
                fn authorize(req: &::suprnova::Request) -> bool {
                    #authorize_body
                }

                #max_body_bytes_override
            }
        };
    }

    // At least one field is injected from a route param: generate a custom
    // `extract` that runs the FULL FormRequest lifecycle (Precognition
    // detection, content-type-aware body parsing honoring max_body_bytes,
    // after_validation cross-field hook) with one extra step — route-param
    // injection into the parsed body map before deserialization, with path
    // params winning (IDOR protection).
    //
    // Audit HIGH #336: prior to this, the route-param branch read
    // `body_bytes()` without honoring `max_body_bytes`, never parsed
    // form-urlencoded bodies, didn't handle Precognition, and skipped the
    // `after_validation` cross-field hook. Adding a single route-param
    // field silently changed the request semantics for the whole DTO.
    quote! {
        #[::suprnova::__async_trait::async_trait]
        impl #impl_generics ::suprnova::http::FormRequest for #struct_name #ty_generics #where_clause {
            fn authorize(req: &::suprnova::Request) -> bool {
                #authorize_body
            }

            #max_body_bytes_override

            async fn extract(req: ::suprnova::Request) -> ::core::result::Result<Self, ::suprnova::FrameworkError> {
                if !Self::authorize(&req) {
                    return Err(::suprnova::FrameworkError::Unauthorized);
                }

                // Snapshot route params BEFORE consuming the request body.
                let route_snapshot: ::std::collections::HashMap<String, String> =
                    req.all_route_params();

                // --- Precognition detection (mirrors default FormRequest::extract) ---
                let is_precognition = req
                    .header("Precognition")
                    .map(|v| v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
                let validate_only: Vec<String> = req
                    .header("Precognition-Validate-Only")
                    .map(|raw| {
                        raw.split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();

                // --- Content-type-aware body parsing with per-struct cap ---
                let content_type = req.content_type().map(|s| s.to_string());
                let (_, body_bytes) = req
                    .body_bytes_with_cap(Self::max_body_bytes())
                    .await?;

                let mut map: ::suprnova::serde_json::Map<String, ::suprnova::serde_json::Value> =
                    if body_bytes.is_empty() {
                        ::suprnova::serde_json::Map::new()
                    } else {
                        match content_type.as_deref() {
                            ::core::option::Option::Some(ct)
                                if ct.starts_with("application/x-www-form-urlencoded") =>
                            {
                                // Form-urlencoded: flatten pairs into a JSON object
                                // (last value wins on duplicate keys, matching Laravel).
                                let mut obj = ::suprnova::serde_json::Map::new();
                                for (k, v) in ::url::form_urlencoded::parse(&body_bytes) {
                                    obj.insert(
                                        k.into_owned(),
                                        ::suprnova::serde_json::Value::String(v.into_owned()),
                                    );
                                }
                                obj
                            }
                            _ => {
                                // JSON: must be an object. Reject non-object payloads
                                // explicitly rather than silently treating them as `{}`.
                                let parsed: ::suprnova::serde_json::Value =
                                    ::suprnova::serde_json::from_slice(&body_bytes)
                                        .map_err(|e| ::suprnova::FrameworkError::bad_request(
                                            ::std::format!("malformed JSON body: {e}"),
                                        ))?;
                                match parsed {
                                    ::suprnova::serde_json::Value::Object(m) => m,
                                    _ => {
                                        return ::core::result::Result::Err(
                                            ::suprnova::FrameworkError::bad_request(
                                                "request body must be a JSON object \
                                                 (DTOs with route-param fields cannot \
                                                 accept arrays / strings / null at the \
                                                 top level)",
                                            ),
                                        );
                                    }
                                }
                            }
                        }
                    };

                // Inject route params into the map (path params WIN — IDOR protection).
                #(#route_param_injections)*

                // Deserialize the merged map into Self.
                let dto: Self = ::suprnova::serde_json::from_value(
                    ::suprnova::serde_json::Value::Object(map),
                ).map_err(|e| ::suprnova::FrameworkError::bad_request(e.to_string()))?;

                // --- Validate + Precognition + after_validation (mirrors default) ---
                use ::validator::Validate;
                let validation_result = dto.validate();

                if is_precognition {
                    return match validation_result {
                        ::core::result::Result::Ok(()) => {
                            match dto.after_validation() {
                                ::core::result::Result::Ok(()) => ::core::result::Result::Err(
                                    ::suprnova::FrameworkError::PrecognitionSuccess,
                                ),
                                ::core::result::Result::Err(errs) => {
                                    let filtered = if validate_only.is_empty() {
                                        errs
                                    } else {
                                        errs.retain_fields(&validate_only)
                                    };
                                    if filtered.is_empty() {
                                        ::core::result::Result::Err(
                                            ::suprnova::FrameworkError::PrecognitionSuccess,
                                        )
                                    } else {
                                        ::core::result::Result::Err(
                                            ::suprnova::FrameworkError::PrecognitionFailure(filtered),
                                        )
                                    }
                                }
                            }
                        }
                        ::core::result::Result::Err(errors) => {
                            let errs = ::suprnova::ValidationErrors::from_validator(errors);
                            let filtered = if validate_only.is_empty() {
                                errs
                            } else {
                                errs.retain_fields(&validate_only)
                            };
                            if filtered.is_empty() {
                                ::core::result::Result::Err(
                                    ::suprnova::FrameworkError::PrecognitionSuccess,
                                )
                            } else {
                                ::core::result::Result::Err(
                                    ::suprnova::FrameworkError::PrecognitionFailure(filtered),
                                )
                            }
                        }
                    };
                }

                if let ::core::result::Result::Err(errors) = validation_result {
                    return ::core::result::Result::Err(::suprnova::FrameworkError::Validation(
                        ::suprnova::ValidationErrors::from_validator(errors),
                    ));
                }

                if let ::core::result::Result::Err(errs) = dto.after_validation() {
                    return ::core::result::Result::Err(::suprnova::FrameworkError::Validation(errs));
                }

                ::core::result::Result::Ok(dto)
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
                ));
            }
        };
        list.parse_nested_meta(|meta| {
            if meta.path.is_ident("input_only") {
                opts.input_only = true;
            } else if meta.path.is_ident("output_only") {
                opts.output_only = true;
            } else if meta.path.is_ident("allow_include") {
                opts.allow_include = true;
            } else if meta.path.is_ident("lazy") {
                // Accept both bare `lazy` and `lazy(<flavor>)`.
                if meta.input.peek(syn::token::Paren) {
                    let content;
                    syn::parenthesized!(content in meta.input);
                    let flavor_ident: syn::Ident = content.parse()?;
                    opts.lazy = Some(match flavor_ident.to_string().as_str() {
                        "inertia" => LazyFlavor::Inertia,
                        "deferred" => LazyFlavor::Deferred,
                        "closure" => LazyFlavor::Closure,
                        "when_loaded" => LazyFlavor::WhenLoaded,
                        other => {
                            return Err(syn::Error::new(
                                flavor_ident.span(),
                                format!("unknown lazy flavor `{other}` — expected inertia, deferred, closure, or when_loaded"),
                            ));
                        }
                    });
                } else {
                    opts.lazy = Some(LazyFlavor::Plain);
                }
            } else if meta.path.is_ident("from_route_param") {
                // `#[data(from_route_param("key"))]` — explicit param name
                // `#[data(from_route_param)]`        — use the field name
                if meta.input.peek(syn::token::Paren) {
                    let content;
                    syn::parenthesized!(content in meta.input);
                    let lit: syn::LitStr = content.parse()?;
                    opts.from_route_param = Some(Some(lit.value()));
                } else {
                    opts.from_route_param = Some(None);
                }
            } else {
                return Err(meta.error(
                    "unknown #[data(...)] flag — expected input_only, output_only, allow_include, lazy, or from_route_param",
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

/// Build the `out.push((name, PropEntry::...))` statement for one field of
/// a `#[derive(Data)]` struct.
///
/// Lazy flavors (`#[data(lazy(...))]`) never serialize eagerly, so they emit
/// the same token regardless of `fallible`. The eager (default) case
/// serializes the field to a `serde_json::Value` up front:
///
/// - `fallible = false` → panics on `Serialize` failure, naming
///   `Struct::field` + the source error (Domain 5 M-D5-9). Backs the
///   infallible `__into_inertia_props` escape hatch (`Inertia::data`).
/// - `fallible = true`  → propagates the failure as `FrameworkError::internal`
///   via `?`, with the same diagnostic shape. Backs `__try_into_inertia_props`
///   (`Inertia::try_data`), so a bad `Serialize` impl becomes a recoverable
///   error off the HTTP path (queue workers, the scheduler, CLI) where the
///   request-level panic to 500 net does not apply.
fn build_prop_entry(
    f: &Field,
    opts: &FieldOptions,
    struct_name_str: &str,
    qualified_name_expr: &TokenStream2,
    fallible: bool,
) -> TokenStream2 {
    let ident = f.ident.as_ref().unwrap();
    let name = ident.to_string();

    if let Some(flavor) = &opts.lazy {
        // Audit HIGH #336: `owner` is the FULLY-QUALIFIED type name so
        // include-allowlist lookups match the registry key (also fully
        // qualified). Bare struct names would collide across modules
        // and nondeterministically resolve the wrong allowlist.
        let entry_construction = match flavor {
            LazyFlavor::Plain | LazyFlavor::Inertia | LazyFlavor::WhenLoaded => quote! {
                ::suprnova::inertia::PropEntry::LazyOwned {
                    owner: #qualified_name_expr,
                    field: #name,
                    prop: self.#ident,
                }
            },
            LazyFlavor::Deferred => quote! {
                ::suprnova::inertia::PropEntry::DeferredOwned {
                    owner: #qualified_name_expr,
                    field: #name,
                    prop: self.#ident,
                }
            },
            LazyFlavor::Closure => quote! {
                ::suprnova::inertia::PropEntry::ClosureOwned {
                    owner: #qualified_name_expr,
                    field: #name,
                    prop: self.#ident,
                }
            },
        };
        quote! {
            out.push((
                #name.to_string(),
                #entry_construction,
            ));
        }
    } else if fallible {
        // __try_into_inertia_props — propagate the serialize failure as a
        // FrameworkError via `?` instead of panicking. Same diagnostic
        // shape (Struct::field + source error) as the infallible path's panic.
        //
        // Field<T>: skip the entry entirely when Absent so Inertia props mirror
        // the documented `skip_serializing_if = "Field::is_absent"` behaviour
        // (no key in the prop list rather than a `null` value).
        let skip_if_absent = is_field_type(&f.ty);
        let push_block = quote! {
            out.push((
                #name.to_string(),
                ::suprnova::inertia::PropEntry::Eager(
                    ::suprnova::serde_json::to_value(&self.#ident)
                        .map_err(|__suprnova_ser_err| ::suprnova::FrameworkError::internal(::std::format!(
                            "__try_into_inertia_props: serde_json::to_value failed for \
                             field `{}::{}`: {} (the field's Serialize impl returned \
                             Err - check for invalid float/NaN, broken custom \
                             serializer, or unsupported type)",
                            #struct_name_str,
                            #name,
                            __suprnova_ser_err,
                        )))?,
                ),
            ));
        };
        if skip_if_absent {
            quote! {
                if !::suprnova::data::Field::is_absent(&self.#ident) {
                    #push_block
                }
            }
        } else {
            push_block
        }
    } else {
        // Infallible escape hatch (`Inertia::data`); `__try_into_inertia_props`
        // / `Inertia::try_data` is the fallible sibling for callers off the
        // HTTP request lifecycle. The panic message names the struct + field
        // so operators can pinpoint which `serde_json::to_value` failed in
        // production logs.
        //
        // Field<T>: skip the entry entirely when Absent so Inertia props mirror
        // the documented `skip_serializing_if = "Field::is_absent"` behaviour.
        let skip_if_absent = is_field_type(&f.ty);
        let push_block = quote! {
            out.push((
                #name.to_string(),
                ::suprnova::inertia::PropEntry::Eager(
                    ::suprnova::serde_json::to_value(&self.#ident)
                        .unwrap_or_else(|__suprnova_ser_err| ::std::panic!(
                            "__into_inertia_props: serde_json::to_value failed for \
                             field `{}::{}`: {} (the field's Serialize impl returned \
                             Err - check for invalid float/NaN, broken custom \
                             serializer, or unsupported type)",
                            #struct_name_str,
                            #name,
                            __suprnova_ser_err,
                        )),
                ),
            ));
        };
        if skip_if_absent {
            quote! {
                if !::suprnova::data::Field::is_absent(&self.#ident) {
                    #push_block
                }
            }
        } else {
            push_block
        }
    }
}

fn build_into_inertia_props(
    struct_name: &Ident,
    struct_name_str: &str,
    qualified_name_expr: &TokenStream2,
    impl_generics: &syn::ImplGenerics,
    ty_generics: &syn::TypeGenerics,
    where_clause: Option<&syn::WhereClause>,
    parsed: &[(&Field, FieldOptions)],
) -> TokenStream2 {
    // Two parallel entry lists: the infallible (panicking) escape hatch and
    // the `?`-propagating fallible sibling. They differ only in the eager
    // (default) serialize arm; lazy flavors are identical. See
    // `build_prop_entry`.
    let entries: Vec<TokenStream2> = parsed
        .iter()
        .filter(|(_, o)| !o.input_only)
        .map(|(f, opts)| build_prop_entry(f, opts, struct_name_str, qualified_name_expr, false))
        .collect();
    let try_entries: Vec<TokenStream2> = parsed
        .iter()
        .filter(|(_, o)| !o.input_only)
        .map(|(f, opts)| build_prop_entry(f, opts, struct_name_str, qualified_name_expr, true))
        .collect();

    quote! {
        impl #impl_generics #struct_name #ty_generics #where_clause {
            #[doc(hidden)]
            pub fn __into_inertia_props(self) -> ::std::vec::Vec<(String, ::suprnova::inertia::PropEntry)> {
                let mut out = ::std::vec::Vec::new();
                #(#entries)*
                out
            }

            #[doc(hidden)]
            pub fn __try_into_inertia_props(
                self,
            ) -> ::core::result::Result<
                ::std::vec::Vec<(String, ::suprnova::inertia::PropEntry)>,
                ::suprnova::FrameworkError,
            > {
                let mut out = ::std::vec::Vec::new();
                #(#try_entries)*
                ::core::result::Result::Ok(out)
            }
        }

        impl #impl_generics ::suprnova::inertia::IntoInertiaData for #struct_name #ty_generics #where_clause {
            fn __into_inertia_props(self) -> ::std::vec::Vec<(String, ::suprnova::inertia::PropEntry)> {
                <Self>::__into_inertia_props(self)
            }

            fn __try_into_inertia_props(
                self,
            ) -> ::core::result::Result<
                ::std::vec::Vec<(String, ::suprnova::inertia::PropEntry)>,
                ::suprnova::FrameworkError,
            > {
                <Self>::__try_into_inertia_props(self)
            }
        }
    }
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

    // auto_lazy: when the struct-level flag is set, implicitly mark any
    // `Prop<T>`-typed field as lazy (equivalent to `#[data(lazy)]`).
    if struct_opts.auto_lazy {
        for (field, opts) in parsed.iter_mut() {
            if opts.lazy.is_none() && is_prop_type(&field.ty) {
                opts.lazy = Some(LazyFlavor::Plain);
            }
        }
    }

    // Lazy fields are always allow_include-eligible so they register in
    // the runtime allowlist.
    for (_, opts) in parsed.iter_mut() {
        if opts.lazy.is_some() {
            opts.allow_include = true;
        }
    }

    // If any non-output-only field has a reference type (`&T` / `&'a T`),
    // we skip generating the Deserialize impl.  Reference types cannot be
    // produced by serde's deserialization machinery in the general case
    // (serde only supports `&str` / `&[u8]` via borrowing).  Structs with
    // reference fields are typically only ever serialized, not deserialized.
    let has_reference_fields = parsed
        .iter()
        .filter(|(_, o)| !o.output_only && o.lazy.is_none())
        .any(|(f, _)| is_reference_type(&f.ty));

    // Structs with lazy fields (`Prop` type) also skip Deserialize and
    // FormRequest: `Prop` is a server-side type with no `Default` impl and
    // is never deserialized from client input. The DTO's public surface for
    // output is `__into_inertia_props`; for input, callers must write their
    // own extractor or use a separate input DTO without lazy fields.
    let has_lazy_fields = parsed.iter().any(|(_, o)| o.lazy.is_some());

    let serialize_impl = build_serialize(
        struct_name,
        &impl_generics,
        &ty_generics,
        where_clause,
        &parsed,
    );
    let deserialize_impl = if has_reference_fields || has_lazy_fields {
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
            struct_opts.allow_unknown_fields,
        )
    };
    // Fully-qualified type name expression — used as the registry key for
    // include allowlists and as the `owner` field on lazy PropEntry variants.
    // `concat!(module_path!(), "::", stringify!(StructName))` resolves to a
    // single `&'static str` literal at compile time, unique per module path
    // even when two crates define structs with the same bare identifier.
    let qualified_name_expr: TokenStream2 = quote! {
        ::std::concat!(::std::module_path!(), "::", ::std::stringify!(#struct_name))
    };
    let allowlist_registration = build_allowlist_registration(&qualified_name_expr, &parsed);
    // Skip FormRequest for generic structs (any type or lifetime params).
    // FormRequest requires DeserializeOwned + Send + Validate; these bounds
    // cannot be generically propagated without knowing the concrete type params.
    // Users who need FormRequest on a generic struct must write the impl manually
    // with the appropriate where bounds (e.g., `where T: Send + Validate + ...`).
    let is_generic = !input.generics.params.is_empty();
    let form_request_impl = if is_generic || has_reference_fields || has_lazy_fields {
        proc_macro2::TokenStream::new()
    } else {
        build_form_request(
            struct_name,
            &struct_opts,
            &impl_generics,
            &ty_generics,
            where_clause,
            &parsed,
        )
    };

    let into_inertia_props_impl = build_into_inertia_props(
        struct_name,
        &struct_name_str,
        &qualified_name_expr,
        &impl_generics,
        &ty_generics,
        where_clause,
        &parsed,
    );

    let into_json_resource_impl = if let Some(jr_opts) = &struct_opts.json_resource {
        build_into_json_resource(
            struct_name,
            &impl_generics,
            &ty_generics,
            where_clause,
            &parsed,
            jr_opts,
        )
    } else {
        proc_macro2::TokenStream::new()
    };

    let expanded = quote! {
        #serialize_impl
        #deserialize_impl
        #allowlist_registration
        #form_request_impl
        #into_inertia_props_impl
        #into_json_resource_impl
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

/// Returns `true` when the field type's last path segment is `Field`.
/// Distinguishes `Field<T>` (tri-state `Absent`/`Null`/`Value`) from the
/// plain `Option<T>` so we can wire the documented `Field::Absent` →
/// "omit the key entirely" behaviour from `framework/src/data/field.rs`
/// into the derive's custom Serialize.
fn is_field_type(ty: &syn::Type) -> bool {
    if let syn::Type::Path(p) = ty {
        if let Some(seg) = p.path.segments.last() {
            return seg.ident == "Field";
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
    // Build the output field list, tagging each entry as either always-emit
    // or `Field`-typed-conditional-emit. `Field<T>` honors its documented
    // contract: `Field::Absent` omits the key from the serialized output,
    // matching the `skip_serializing_if = "Field::is_absent"` guidance in
    // `framework/src/data/field.rs`. `Field::Null` and `Field::Value(_)`
    // still serialize (as `null` and the inner value, respectively).
    let output_fields: Vec<TokenStream2> = parsed
        .iter()
        // input_only and lazy fields are excluded from serde Serialize:
        // - input_only: never sent to the client in the response
        // - lazy: Prop<T> is not directly serializable; goes through
        //   __into_inertia_props -> PropEntry -> InertiaResponse resolution
        .filter(|(_, opts)| !opts.input_only && opts.lazy.is_none())
        .map(|(f, _)| {
            let ident = f.ident.as_ref().unwrap();
            let name = ident.to_string();
            if is_field_type(&f.ty) {
                quote! {
                    if !::suprnova::data::Field::is_absent(&self.#ident) {
                        ::serde::ser::SerializeStruct::serialize_field(&mut state, #name, &self.#ident)?;
                    } else {
                        ::serde::ser::SerializeStruct::skip_field(&mut state, #name)?;
                    }
                }
            } else {
                quote! {
                    ::serde::ser::SerializeStruct::serialize_field(&mut state, #name, &self.#ident)?;
                }
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

#[allow(clippy::too_many_arguments)]
fn build_deserialize(
    struct_name: &Ident,
    struct_name_str: &str,
    de_impl_generics: &syn::ImplGenerics,
    impl_generics: &syn::ImplGenerics,
    ty_generics: &syn::TypeGenerics,
    where_clause: Option<&syn::WhereClause>,
    parsed: &[(&Field, FieldOptions)],
    has_type_params: bool,
    allow_unknown_fields: bool,
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

    // Tokens for the `_ =>` arm of the visitor's key match. The default
    // is STRICT: unknown payload keys surface as
    // `serde::de::Error::unknown_field` so client typos and schema drift
    // fail at the boundary. `#[data(allow_unknown_fields)]` opts back
    // into the serde-default permissive behaviour for response DTOs
    // that read forward-compatible payloads from external services.
    //
    // The expected-fields slice is the union of required, defaultable,
    // and output_only names — output_only keys are rejected with a more
    // specific message earlier in the match, so listing them here only
    // affects the diagnostic output for keys that match nothing at all.
    let all_known_names: Vec<String> = req_names
        .iter()
        .map(|s| (*s).to_string())
        .chain(def_names.iter().map(|s| (*s).to_string()))
        .chain(output_only_names.iter().cloned())
        .collect();
    let unknown_field_arm = if allow_unknown_fields {
        quote! {
            _ => {
                // Unknown keys silently dropped — opt-in permissive
                // behaviour via `#[data(allow_unknown_fields)]`.
                let _: ::serde::de::IgnoredAny = map.next_value()?;
            }
        }
    } else {
        quote! {
            _ => {
                // Drop the value to keep the deserializer state machine
                // sane (some formats require consuming the value before
                // erroring), then emit the field-aware error.
                let _: ::serde::de::IgnoredAny = map.next_value()?;
                return Err(<__A::Error as ::serde::de::Error>::unknown_field(
                    key.as_str(),
                    &[#(#all_known_names),*],
                ));
            }
        }
    };

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
                                #unknown_field_arm
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

/// Emit the `IntoJsonResource` impl for a struct annotated with
/// `#[json_resource("type")]`.
///
/// - Attribute fields: non-input_only, non-id, non-allow_include, non-lazy fields.
/// - Relationship fields: `#[data(allow_include)]` fields that are NOT lazy
///   (lazy = Inertia/Prop fields; they can't satisfy `IntoJsonResource`).
/// - id field: the field named by `opts.id_field` (default "id").
/// - Default-deny: `resource_included` rejects unknown keys in the include tree.
fn build_into_json_resource(
    struct_name: &Ident,
    impl_generics: &syn::ImplGenerics,
    ty_generics: &syn::TypeGenerics,
    where_clause: Option<&syn::WhereClause>,
    parsed: &[(&Field, FieldOptions)],
    opts: &JsonResourceOptions,
) -> TokenStream2 {
    let resource_type = &opts.resource_type;
    let id_field = syn::Ident::new(&opts.id_field, proc_macro2::Span::call_site());

    // Attribute fields: non-input_only, not the id field, not allow_include, not lazy.
    // Carry whether the field is `Field<T>` so the attribute writer can honour
    // the `Field::Absent` → omit-key contract per `framework/src/data/field.rs`.
    let attr_fields: Vec<(&Ident, String, bool)> = parsed
        .iter()
        .filter(|(f, fo)| {
            let ident = f.ident.as_ref().unwrap();
            !fo.input_only && ident != &id_field && !fo.allow_include && fo.lazy.is_none()
        })
        .map(|(f, _)| {
            let ident = f.ident.as_ref().unwrap();
            let name = ident.to_string();
            let is_field = is_field_type(&f.ty);
            (ident, name, is_field)
        })
        .collect();

    // Relationship fields: allow_include=true AND not lazy (Prop fields excluded).
    let rel_fields: Vec<(&Ident, String)> = parsed
        .iter()
        .filter(|(_, fo)| fo.allow_include && fo.lazy.is_none())
        .map(|(f, _)| {
            let ident = f.ident.as_ref().unwrap();
            let name = ident.to_string();
            (ident, name)
        })
        .collect();

    let struct_name_str = struct_name.to_string();
    let attrs_entries = attr_fields.iter().map(|(ident, name, is_field)| {
        // Panic message names the struct + field so operators can pinpoint
        // which `serde_json::to_value` failed in production logs.
        //
        // Field<T>: omit the attribute when Absent so the resource matches
        // the documented `Field::Absent` → "key absent" contract.
        let insert_block = quote! {
            if fieldset_includes(#name) {
                map.insert(
                    #name.to_string(),
                    ::suprnova::serde_json::to_value(&self.#ident)
                        .unwrap_or_else(|__suprnova_ser_err| ::std::panic!(
                            "IntoJsonResource::resource_attributes: serde_json::to_value \
                             failed for field `{}::{}`: {} (the field's Serialize impl \
                             returned Err)",
                            #struct_name_str,
                            #name,
                            __suprnova_ser_err,
                        )),
                );
            }
        };
        if *is_field {
            quote! {
                if !::suprnova::data::Field::is_absent(&self.#ident) {
                    #insert_block
                }
            }
        } else {
            insert_block
        }
    });

    let rel_entries = rel_fields.iter().map(|(ident, name)| {
        quote! {
            if let Some(rel) = ::suprnova::resources::AsRelationshipValue::as_relationship_value(&self.#ident) {
                rels.push((#name.to_string(), rel));
            }
        }
    });

    let allowed_include_names: Vec<String> =
        rel_fields.iter().map(|(_, name)| name.clone()).collect();
    let resource_type_lit = resource_type.clone();

    let included_entries = rel_fields.iter().map(|(ident, name)| {
        quote! {
            if let ::std::option::Option::Some(subtree) = include_tree.subtree(#name) {
                ::suprnova::resources::PushIncluded::push_included(
                    &self.#ident,
                    subtree,
                    out,
                )?;
            }
        }
    });

    quote! {
        impl #impl_generics ::suprnova::resources::IntoJsonResource for #struct_name #ty_generics #where_clause {
            fn resource_type() -> &'static str {
                #resource_type
            }

            fn resource_id(&self) -> ::std::string::String {
                ::std::string::ToString::to_string(&self.#id_field)
            }

            fn resource_attributes(
                &self,
                fieldset: ::std::option::Option<&[&str]>,
            ) -> ::suprnova::serde_json::Value {
                let fieldset_includes = |name: &str| match fieldset {
                    ::std::option::Option::Some(allowed) => allowed.iter().any(|a| *a == name),
                    ::std::option::Option::None => true,
                };
                let mut map = ::suprnova::serde_json::Map::new();
                #(#attrs_entries)*
                ::suprnova::serde_json::Value::Object(map)
            }

            fn resource_relationships(
                &self,
            ) -> ::std::vec::Vec<(::std::string::String, ::suprnova::resources::RelationshipValue)> {
                let mut rels = ::std::vec::Vec::new();
                #(#rel_entries)*
                rels
            }

            fn resource_included(
                &self,
                include_tree: &::suprnova::resources::IncludeTree,
                out: &mut ::std::vec::Vec<::suprnova::serde_json::Value>,
            ) -> ::std::result::Result<(), ::suprnova::resources::IncludeResolutionError> {
                // Default-deny: reject any include key not in our allowlist.
                const ALLOWED: &[&str] = &[#(#allowed_include_names),*];
                for (key, _) in include_tree.iter() {
                    if !ALLOWED.contains(&key) {
                        return ::std::result::Result::Err(
                            ::suprnova::resources::IncludeResolutionError {
                                path: key.to_string(),
                                on_type: #resource_type_lit,
                            },
                        );
                    }
                }
                #(#included_entries)*
                ::std::result::Result::Ok(())
            }
        }
    }
}

fn build_allowlist_registration(
    qualified_name_expr: &TokenStream2,
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
    //
    // Audit HIGH #336: the registry key is the FULLY-QUALIFIED type name
    // (`my_crate::my_module::MyDto`) — not the bare struct name. This
    // prevents two DTOs with the same identifier in different modules
    // from overwriting each other's include allowlists nondeterministically.
    // `concat!(module_path!(), "::", stringify!(...))` resolves to a
    // single `&'static str` literal at expansion time.
    quote! {
        ::suprnova::inventory::submit! {
            ::suprnova::data::registry::AllowedIncludes {
                struct_name: #qualified_name_expr,
                fields: &[#(#allow_include_names),*],
            }
        }
    }
}

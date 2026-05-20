//! Parses `#[suprnova::model(table = "...", primary_key = "...", ...)]`
//! into a structured representation other modules emit code from.

use proc_macro2::{Ident, Span, TokenStream};
use syn::{parse2, parse::Parse, parse::ParseStream, ItemStruct, LitBool, LitStr, Result, Token, Type};

/// The parsed `#[model(...)]` attribute plus the struct definition.
pub struct ModelInput {
    pub item: ItemStruct,
    pub table: String,
    pub primary_key: String,
    #[allow(dead_code)] // T4+ — used when typed CRUD lifecycle wires through.
    pub key_type: Type,
    pub auto_increment: bool,
    #[allow(dead_code)] // T4+ — multi-connection routing in derive_eloquent.
    pub connection: String,
    // Slots filled by later tasks (Phase 10A T6/T7a/T9/T10):
    #[allow(dead_code)]
    pub fillable: Option<Vec<String>>,
    #[allow(dead_code)]
    pub guarded: Option<Vec<String>>,
    #[allow(dead_code)]
    pub casts: Vec<(Ident, Type)>,
    #[allow(dead_code)]
    pub timestamps: bool,
    #[allow(dead_code)]
    pub created_at: String,
    #[allow(dead_code)]
    pub updated_at: String,
    #[allow(dead_code)]
    pub soft_deletes: bool,
    #[allow(dead_code)]
    pub soft_deletes_column: String,
    #[allow(dead_code)]
    pub appends: Vec<String>,
    #[allow(dead_code)]
    pub hidden: Vec<String>,
    #[allow(dead_code)]
    pub visible: Option<Vec<String>>,
    #[allow(dead_code)]
    pub mutators: Vec<String>,
    #[allow(dead_code)]
    pub touches: Vec<String>,
}

impl ModelInput {
    pub fn parse(attr: TokenStream, item: TokenStream) -> Result<Self> {
        let item: ItemStruct = parse2(item)?;
        let attrs = parse2::<ModelAttrs>(attr)?;
        let struct_name = item.ident.to_string();

        let table = attrs.table.unwrap_or_else(|| pluralize_snake(&struct_name));
        let primary_key = attrs.primary_key.unwrap_or_else(|| "id".to_string());
        let key_type = attrs
            .key_type
            .unwrap_or_else(|| syn::parse_str("i64").expect("i64 parses"));
        let auto_increment = attrs.auto_increment.unwrap_or(true);
        let connection = attrs.connection.unwrap_or_else(|| "default".to_string());
        let timestamps_default = attrs.timestamps.unwrap_or(true);
        let created_at = attrs.created_at.unwrap_or_else(|| "created_at".to_string());
        let updated_at = attrs.updated_at.unwrap_or_else(|| "updated_at".to_string());
        let soft_deletes = attrs.soft_deletes.unwrap_or(false);
        let soft_deletes_column = attrs
            .soft_deletes_column
            .unwrap_or_else(|| "deleted_at".to_string());

        if attrs.fillable.is_some() && attrs.guarded.is_some() {
            return Err(syn::Error::new(
                Span::call_site(),
                "cannot specify both `fillable` and `guarded` on the same model",
            ));
        }

        // T8 — `hidden` is a denylist, `visible` an allowlist. Both at
        // once is incoherent (one says "drop these", the other "keep
        // only these") and Laravel rejects the same combination via
        // `$hidden` / `$visible` semantics.
        if !attrs.hidden.as_ref().is_none_or(Vec::is_empty)
            && attrs.visible.as_ref().is_some_and(|v| !v.is_empty())
        {
            return Err(syn::Error::new(
                Span::call_site(),
                "cannot specify both `hidden` and `visible` on the same model",
            ));
        }

        // T9 — auto-detect timestamp columns from the struct fields.
        //
        //   user attribute       | struct has BOTH      | exactly ONE     | NEITHER
        //   ---------------------+----------------------+-----------------+---------
        //   timestamps = false   | disabled             | disabled        | disabled
        //   timestamps (default) | enabled              | compile_error!  | disabled
        //
        // The default-enabled case used to fail at compile time if the
        // struct lacked `created_at` / `updated_at` (the macro emitted
        // `am.created_at = Set(now)` against a non-existent ActiveModel
        // field). Auto-detect lets users skip the `timestamps = false`
        // opt-out ceremony for join tables / pivots / no-history models.
        // The exactly-one case still errors loudly because it's almost
        // certainly a typo (e.g. `craeted_at`) — silently skipping
        // there would mask the bug.
        let field_names: std::collections::HashSet<String> = match &item.fields {
            syn::Fields::Named(named) => named
                .named
                .iter()
                .filter_map(|f| f.ident.as_ref().map(|i| i.to_string()))
                .collect(),
            _ => std::collections::HashSet::new(),
        };
        let has_created = field_names.contains(&created_at);
        let has_updated = field_names.contains(&updated_at);
        let timestamps = if !timestamps_default {
            false
        } else {
            match (has_created, has_updated) {
                (true, true) => true,
                (false, false) => false,
                _ => {
                    return Err(syn::Error::new_spanned(
                        &item.ident,
                        format!(
                            "model has only one of `{}` / `{}`. Eloquent timestamps need both \
                             columns (or neither). Add the missing field, or set \
                             `#[model(timestamps = false)]` to disable timestamp tracking \
                             entirely.",
                            created_at, updated_at,
                        ),
                    ));
                }
            }
        };

        // T9 — auto-inject `AsDateTime` casts for the timestamp columns
        // unless the user already declared a cast for them. The casts
        // do double duty: they unblock SeaORM's `DeriveEntityModel`
        // parser (which mis-parses bare `DateTime<Utc>` as
        // `NaiveDateTime`, since it can't see through generics) AND
        // they wire the same Runtime <-> Storage bridge the rest of
        // the cast machinery uses, so `.touch()` and the auto-set
        // injection don't need a parallel datetime-formatting path.
        let mut casts = attrs.casts.unwrap_or_default();
        if timestamps {
            for col_name in [&created_at, &updated_at] {
                if !casts.iter().any(|(i, _)| i == col_name) {
                    let ident: Ident = syn::parse_str(col_name).map_err(|e| {
                        syn::Error::new_spanned(
                            &item.ident,
                            format!(
                                "timestamp column name `{}` is not a valid Rust identifier: {}",
                                col_name, e,
                            ),
                        )
                    })?;
                    let ty: Type = syn::parse_str("::suprnova::AsDateTime").expect(
                        "::suprnova::AsDateTime parses — Suprnova lib re-exports this type",
                    );
                    casts.push((ident, ty));
                }
            }
        }

        // T10 — soft_deletes auto-injects `AsOptionalDateTime` on the
        // tombstone column. The user types the field as
        // `Option<DateTime<Utc>>` (Laravel-shape) but SeaORM's
        // `DeriveEntityModel` parses field types in a prelude scope
        // that shadows `chrono::DateTime` with `NaiveDateTime`; the
        // cast routes through `<AsOptionalDateTime as Cast>::Storage =
        // Option<String>` so the inner SeaORM Model never sees the
        // ambiguous type. User-declared cast overrides still win, same
        // pattern as the timestamps auto-inject above.
        if soft_deletes && field_names.contains(&soft_deletes_column)
            && !casts.iter().any(|(i, _)| i == &soft_deletes_column)
        {
            let ident: Ident = syn::parse_str(&soft_deletes_column).map_err(|e| {
                syn::Error::new_spanned(
                    &item.ident,
                    format!(
                        "soft_deletes column name `{}` is not a valid Rust identifier: {}",
                        soft_deletes_column, e,
                    ),
                )
            })?;
            let ty: Type = syn::parse_str("::suprnova::AsOptionalDateTime").expect(
                "::suprnova::AsOptionalDateTime parses — Suprnova lib re-exports this type",
            );
            casts.push((ident, ty));
        }

        Ok(Self {
            item,
            table,
            primary_key,
            key_type,
            auto_increment,
            connection,
            fillable: attrs.fillable,
            guarded: attrs.guarded,
            casts,
            timestamps,
            created_at,
            updated_at,
            soft_deletes,
            soft_deletes_column,
            appends: attrs.appends.unwrap_or_default(),
            hidden: attrs.hidden.unwrap_or_default(),
            visible: attrs.visible,
            mutators: attrs.mutators.unwrap_or_default(),
            touches: attrs.touches.unwrap_or_default(),
        })
    }

    pub fn module_name(&self) -> Ident {
        let s = to_snake(&self.item.ident.to_string());
        Ident::new(&s, self.item.ident.span())
    }

    pub fn struct_name_str(&self) -> String {
        self.item.ident.to_string()
    }

    pub fn struct_def(&self) -> &ItemStruct {
        &self.item
    }

    /// Look up the cast type declared for a field. Used by `casts::emit`
    /// once T7a wires the cast pipeline.
    #[allow(dead_code)]
    pub fn cast_for_field(&self, name: &str) -> Option<&Type> {
        self.casts.iter().find_map(|(ident, ty)| {
            if ident == name { Some(ty) } else { None }
        })
    }
}

/// Helper — convert `CamelCase` → `snake_case`.
pub fn to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.char_indices() {
        if ch.is_uppercase() && i != 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// Naive pluralizer — good enough for the common cases users will hit.
/// Override with explicit `table = "..."` for irregulars.
fn pluralize_snake(struct_name: &str) -> String {
    let snake = to_snake(struct_name);
    if snake.ends_with('s') || snake.ends_with("ch") || snake.ends_with("sh") || snake.ends_with('x') || snake.ends_with('z') {
        format!("{snake}es")
    } else if snake.ends_with('y') && snake.len() > 1 {
        let stem = &snake[..snake.len() - 1];
        let prev = snake.chars().nth(snake.len() - 2).unwrap();
        if matches!(prev, 'a' | 'e' | 'i' | 'o' | 'u') {
            format!("{snake}s")
        } else {
            format!("{stem}ies")
        }
    } else {
        format!("{snake}s")
    }
}

/// Internal parser for the comma-separated `name = value` attribute body.
#[derive(Default)]
struct ModelAttrs {
    table: Option<String>,
    primary_key: Option<String>,
    key_type: Option<Type>,
    auto_increment: Option<bool>,
    connection: Option<String>,
    fillable: Option<Vec<String>>,
    guarded: Option<Vec<String>>,
    casts: Option<Vec<(Ident, Type)>>,
    timestamps: Option<bool>,
    created_at: Option<String>,
    updated_at: Option<String>,
    soft_deletes: Option<bool>,
    soft_deletes_column: Option<String>,
    appends: Option<Vec<String>>,
    hidden: Option<Vec<String>>,
    visible: Option<Vec<String>>,
    mutators: Option<Vec<String>>,
    touches: Option<Vec<String>>,
}

impl Parse for ModelAttrs {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut out = ModelAttrs::default();
        if input.is_empty() {
            return Ok(out);
        }
        loop {
            let key: Ident = input.parse()?;
            // Flag-style attributes (no `=`):
            if matches!(key.to_string().as_str(), "soft_deletes" | "timestamps")
                && (input.is_empty() || input.peek(Token![,]))
            {
                match key.to_string().as_str() {
                    "soft_deletes" => out.soft_deletes = Some(true),
                    "timestamps" => out.timestamps = Some(true),
                    _ => unreachable!(),
                }
            } else {
                input.parse::<Token![=]>()?;
                match key.to_string().as_str() {
                    "table" => out.table = Some(input.parse::<LitStr>()?.value()),
                    "primary_key" => out.primary_key = Some(input.parse::<LitStr>()?.value()),
                    "key_type" => {
                        let lit = input.parse::<LitStr>()?;
                        out.key_type = Some(syn::parse_str::<Type>(&lit.value()).map_err(|e| {
                            syn::Error::new(lit.span(), format!("invalid `key_type` Rust type: {e}"))
                        })?);
                    }
                    "auto_increment" => out.auto_increment = Some(input.parse::<LitBool>()?.value),
                    "connection" => out.connection = Some(input.parse::<LitStr>()?.value()),
                    "fillable" => out.fillable = Some(parse_str_array(input)?),
                    "guarded" => out.guarded = Some(parse_str_array(input)?),
                    "casts" => out.casts = Some(parse_casts_map(input)?),
                    "timestamps" => out.timestamps = Some(input.parse::<LitBool>()?.value),
                    "created_at" => out.created_at = Some(input.parse::<LitStr>()?.value()),
                    "updated_at" => out.updated_at = Some(input.parse::<LitStr>()?.value()),
                    "soft_deletes" => out.soft_deletes = Some(input.parse::<LitBool>()?.value),
                    "soft_deletes_column" => out.soft_deletes_column = Some(input.parse::<LitStr>()?.value()),
                    "appends" => out.appends = Some(parse_str_array(input)?),
                    "hidden" => out.hidden = Some(parse_str_array(input)?),
                    "visible" => out.visible = Some(parse_str_array(input)?),
                    "mutators" => out.mutators = Some(parse_str_array(input)?),
                    "touches" => out.touches = Some(parse_str_array(input)?),
                    other => {
                        return Err(syn::Error::new(
                            key.span(),
                            format!("unknown `#[model]` attribute: `{other}`"),
                        ));
                    }
                }
            }
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
        }
        Ok(out)
    }
}

fn parse_str_array(input: ParseStream) -> Result<Vec<String>> {
    let content;
    syn::bracketed!(content in input);
    let mut out = Vec::new();
    while !content.is_empty() {
        out.push(content.parse::<LitStr>()?.value());
        if content.is_empty() {
            break;
        }
        content.parse::<Token![,]>()?;
    }
    Ok(out)
}

fn parse_casts_map(input: ParseStream) -> Result<Vec<(Ident, Type)>> {
    let content;
    syn::braced!(content in input);
    let mut entries = Vec::new();
    while !content.is_empty() {
        let field: Ident = content.parse()?;
        content.parse::<Token![=]>()?;
        let ty: Type = content.parse()?;
        entries.push((field, ty));
        if content.is_empty() { break; }
        content.parse::<Token![,]>()?;
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    #[test]
    fn parse_defaults_minimal() {
        let input = ModelInput::parse(
            quote! {},
            quote! { pub struct User { pub id: i64, pub name: String } },
        )
        .unwrap();
        assert_eq!(input.table, "users");
        assert_eq!(input.primary_key, "id");
        assert!(input.auto_increment);
        assert_eq!(input.connection, "default");
    }

    #[test]
    fn parse_explicit_table() {
        let input = ModelInput::parse(
            quote! { table = "people" },
            quote! { pub struct Person { pub id: i64 } },
        )
        .unwrap();
        assert_eq!(input.table, "people");
    }

    #[test]
    fn pluralize_handles_common_cases() {
        assert_eq!(pluralize_snake("User"), "users");
        assert_eq!(pluralize_snake("Post"), "posts");
        assert_eq!(pluralize_snake("Category"), "categories");
        assert_eq!(pluralize_snake("Holiday"), "holidays");
        assert_eq!(pluralize_snake("Box"), "boxes");
        assert_eq!(pluralize_snake("Match"), "matches");
        assert_eq!(pluralize_snake("Bus"), "buses");
    }

    #[test]
    fn fillable_and_guarded_conflict_errors() {
        let result = ModelInput::parse(
            quote! { fillable = ["a"], guarded = ["b"] },
            quote! { pub struct X { pub id: i64 } },
        );
        match result {
            Ok(_) => panic!("expected fillable + guarded conflict, got Ok"),
            Err(e) => assert!(
                e.to_string().contains("both `fillable` and `guarded`"),
                "unexpected error message: {e}",
            ),
        }
    }

    #[test]
    fn hidden_and_visible_conflict_errors() {
        // T8 — `hidden` is a denylist, `visible` an allowlist; the
        // pair is incoherent. Mirrors fillable + guarded mutual
        // exclusion.
        let result = ModelInput::parse(
            quote! { hidden = ["secret"], visible = ["name"] },
            quote! { pub struct X { pub id: i64, pub name: String, pub secret: String } },
        );
        match result {
            Ok(_) => panic!("expected hidden + visible conflict, got Ok"),
            Err(e) => assert!(
                e.to_string().contains("both `hidden` and `visible`"),
                "unexpected error message: {e}",
            ),
        }
    }

    #[test]
    fn parse_casts_attribute() {
        let input = ModelInput::parse(
            quote! { casts = { active = AsBool, tags = AsArray<String> } },
            quote! { pub struct X { pub id: i64, pub active: bool, pub tags: Vec<String> } },
        )
        .unwrap();
        assert_eq!(input.casts.len(), 2);
        assert_eq!(input.casts[0].0.to_string(), "active");
        assert_eq!(input.casts[1].0.to_string(), "tags");
    }

    #[test]
    fn timestamps_auto_detect_both_columns_enabled() {
        // Default `timestamps = true` + struct has both columns →
        // timestamps enabled + AsDateTime casts auto-injected.
        let input = ModelInput::parse(
            quote! {},
            quote! {
                pub struct User {
                    pub id: i64,
                    pub created_at: chrono::DateTime<chrono::Utc>,
                    pub updated_at: chrono::DateTime<chrono::Utc>,
                }
            },
        )
        .unwrap();
        assert!(input.timestamps);
        assert!(input.casts.iter().any(|(i, _)| i == "created_at"));
        assert!(input.casts.iter().any(|(i, _)| i == "updated_at"));
    }

    #[test]
    fn timestamps_auto_detect_neither_column_disabled() {
        // Default `timestamps = true` + struct has neither column →
        // auto-detect silently skips.
        let input = ModelInput::parse(
            quote! {},
            quote! { pub struct Pivot { pub id: i64, pub user_id: i64, pub role_id: i64 } },
        )
        .unwrap();
        assert!(!input.timestamps);
        assert!(input.casts.is_empty(), "no auto-injected casts on disabled timestamps");
    }

    #[test]
    fn timestamps_auto_detect_partial_errors() {
        // Default `timestamps = true` + struct has only updated_at →
        // compile error (typo guard).
        let result = ModelInput::parse(
            quote! {},
            quote! {
                pub struct Half {
                    pub id: i64,
                    pub updated_at: chrono::DateTime<chrono::Utc>,
                }
            },
        );
        match result {
            Ok(_) => panic!("expected partial-timestamps error, got Ok"),
            Err(e) => assert!(
                e.to_string().contains("only one of"),
                "unexpected error message: {e}",
            ),
        }
    }

    #[test]
    fn timestamps_false_disables_regardless_of_fields() {
        // Explicit opt-out wins even when the struct has both
        // columns. Used when a model has columns named created_at /
        // updated_at for unrelated reasons.
        let input = ModelInput::parse(
            quote! { timestamps = false },
            quote! {
                pub struct AuditLog {
                    pub id: i64,
                    pub created_at: chrono::DateTime<chrono::Utc>,
                    pub updated_at: chrono::DateTime<chrono::Utc>,
                }
            },
        )
        .unwrap();
        assert!(!input.timestamps);
        assert!(input.casts.is_empty(), "no auto-injected casts when timestamps = false");
    }

    #[test]
    fn timestamps_custom_column_names() {
        // `created_at = "..."` / `updated_at = "..."` overrides apply
        // to both the field detection and the auto-cast injection.
        let input = ModelInput::parse(
            quote! { created_at = "creado_en", updated_at = "actualizado_en" },
            quote! {
                pub struct Post {
                    pub id: i64,
                    pub creado_en: chrono::DateTime<chrono::Utc>,
                    pub actualizado_en: chrono::DateTime<chrono::Utc>,
                }
            },
        )
        .unwrap();
        assert!(input.timestamps);
        assert_eq!(input.created_at, "creado_en");
        assert_eq!(input.updated_at, "actualizado_en");
        assert!(input.casts.iter().any(|(i, _)| i == "creado_en"));
        assert!(input.casts.iter().any(|(i, _)| i == "actualizado_en"));
    }

    #[test]
    fn timestamps_user_cast_override_preserved() {
        // If the user explicitly declares a cast for created_at /
        // updated_at, the macro doesn't replace it. AsImmutableDateTime
        // here would survive the auto-injection.
        let input = ModelInput::parse(
            quote! { casts = { created_at = AsImmutableDateTime } },
            quote! {
                pub struct Post {
                    pub id: i64,
                    pub created_at: chrono::DateTime<chrono::Utc>,
                    pub updated_at: chrono::DateTime<chrono::Utc>,
                }
            },
        )
        .unwrap();
        assert!(input.timestamps);
        let created_cast = input.casts.iter().find(|(i, _)| i == "created_at").unwrap();
        let ty_token = &created_cast.1;
        let ty = quote!(#ty_token).to_string();
        // The user's `AsImmutableDateTime` wins; auto-injection didn't
        // overwrite. (We don't probe the type literally to keep the
        // assertion robust against whitespace.)
        assert!(
            ty.contains("AsImmutableDateTime"),
            "expected user-declared AsImmutableDateTime, got: {ty}",
        );
        // updated_at still got auto-injected since the user only
        // overrode created_at.
        assert!(input.casts.iter().any(|(i, _)| i == "updated_at"));
    }

    #[test]
    fn touches_attribute_parses() {
        let input = ModelInput::parse(
            quote! { touches = ["post", "author"] },
            quote! { pub struct Comment { pub id: i64 } },
        )
        .unwrap();
        assert_eq!(input.touches, vec!["post".to_string(), "author".to_string()]);
    }
}

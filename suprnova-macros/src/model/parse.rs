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
        let timestamps = attrs.timestamps.unwrap_or(true);
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

        Ok(Self {
            item,
            table,
            primary_key,
            key_type,
            auto_increment,
            connection,
            fillable: attrs.fillable,
            guarded: attrs.guarded,
            casts: attrs.casts.unwrap_or_default(),
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
}

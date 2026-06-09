//! Helpers for reading the consuming project's `Cargo.toml`.

use std::fs;
use std::path::Path;

/// Parse a Cargo.toml document into a `toml::Table`.
///
/// Cargo.toml is always a TOML *document* (with `[package]`, `[dependencies]`,
/// etc.), not a single TOML *value*. Parsing into `toml::Value` fails on the
/// first `[section]` header with "expected nothing"; `toml::Table` is the
/// correct shape.
pub fn parse_cargo_toml(content: &str) -> Result<toml::Table, toml::de::Error> {
    content.parse::<toml::Table>()
}

/// Extract `[package].name` from already-loaded Cargo.toml content.
pub fn package_name_from_content(content: &str) -> Option<String> {
    let table = parse_cargo_toml(content).ok()?;
    let name = table.get("package")?.get("name")?.as_str()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Read `Cargo.toml` from `path` and return `[package].name`.
pub fn package_name_from_path(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    package_name_from_content(&content)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCAFFOLD_CARGO_TOML: &str = r#"[package]
name = "nebula"
version = "0.1.0"
edition = "2024"
description = "A starter kit for Suprnova"
authors = ["entrepeneur4lyf <shawn.payments@gmail.com>"]

[[bin]]
name = "nebula"
path = "cmd/main.rs"

[[bin]]
name = "console"
path = "src/bin/console.rs"

[dependencies]
suprnova = { git = "https://github.com/entrepeneur4lyf/suprnova.git" }
tokio = { version = "1", features = ["full"] }
"#;

    #[test]
    fn parses_scaffold_shaped_cargo_toml_as_document_not_single_value() {
        let table = parse_cargo_toml(SCAFFOLD_CARGO_TOML).expect(
            "scaffold-shaped Cargo.toml must parse as a TOML document; \
             regression guard against re-introducing `toml::Value` here",
        );
        assert!(table.contains_key("package"));
        assert!(table.contains_key("dependencies"));
    }

    #[test]
    fn extracts_package_name_from_scaffold() {
        assert_eq!(
            package_name_from_content(SCAFFOLD_CARGO_TOML).as_deref(),
            Some("nebula")
        );
    }

    #[test]
    fn returns_none_for_invalid_toml() {
        assert_eq!(package_name_from_content("this is not toml ===="), None);
    }

    #[test]
    fn returns_none_when_package_table_absent() {
        assert_eq!(
            package_name_from_content("[workspace]\nmembers = []\n"),
            None
        );
    }

    #[test]
    fn returns_none_when_package_name_empty() {
        assert_eq!(
            package_name_from_content("[package]\nname = \"\"\n"),
            None
        );
    }
}

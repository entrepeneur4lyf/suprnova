//! TS extraction across Data derives:
//!   - Field<T>  → `field?: T | null`
//!   - Prop<T>   → `field?: T`         (lazy/deferred — may be absent)
//!   - input_only → excluded from generated output type
//!   - output_only → included in output type, excluded from input type
//!   - allow_include → no TS effect (runtime-only)

use suprnova_cli::commands::generate_types::{ScanInput, generate_types_string};

const SRC: &str = r#"
use suprnova::data::Field;
use suprnova::inertia::Prop;

#[derive(suprnova::Data, validator::Validate)]
pub struct UserDto {
    pub id: i64,
    pub name: String,

    #[data(input_only)]
    #[validate(length(min = 8))]
    pub password: String,

    #[data(output_only)]
    pub computed_handle: String,

    pub bio: Field<String>,

    #[data(lazy)]
    pub favorite_song: Prop<String>,
}
"#;

fn extract_block(ts: &str, name: &str) -> String {
    let start = ts
        .find(&format!("export interface {} {{", name))
        .or_else(|| ts.find(&format!("export interface {}<", name)))
        .expect("interface block not found");
    let after = &ts[start..];
    let end = after.find("}\n").expect("block close not found") + 1;
    after[..end].to_string()
}

#[test]
fn user_dto_emits_output_and_input_types() {
    let ts = generate_types_string(ScanInput::Source(SRC));

    // Output type — what the frontend RECEIVES
    let output = extract_block(&ts, "UserDto");
    assert!(output.contains("id: number"));
    assert!(output.contains("name: string"));
    assert!(!output.contains("password")); // input_only excluded
    assert!(output.contains("computed_handle: string"));
    assert!(output.contains("bio?: string | null")); // Field<T>
    assert!(output.contains("favorite_song?: string")); // Prop<T>
    assert!(!output.contains("favorite_song?: string | null"));
    assert!(!output.contains("Prop<")); // never leak Rust-only types

    // Input type — what the frontend SENDS
    let input = extract_block(&ts, "UserDtoInput");
    assert!(input.contains("password: string")); // input_only included
    assert!(!input.contains("computed_handle")); // output_only excluded
    assert!(!input.contains("favorite_song")); // lazy props are output-only
}

const GENERIC_SRC: &str = r#"
use suprnova::data::Field;

#[derive(suprnova::Data)]
pub struct Paginated<T>
where
    T: serde::Serialize + for<'de> serde::Deserialize<'de>,
{
    pub items: Vec<T>,
    pub total: usize,
    pub cursor: Field<String>,
}
"#;

#[test]
fn generic_struct_emits_typescript_generic() {
    let ts = generate_types_string(ScanInput::Source(GENERIC_SRC));
    assert!(ts.contains("export interface Paginated<T>"));
    assert!(ts.contains("items: Array<T>"));
    assert!(ts.contains("total: number"));
    assert!(ts.contains("cursor?: string | null"));
}

#[test]
fn multi_param_generic() {
    let src = r#"
        #[derive(suprnova::Data)]
        pub struct Pair<A, B>
        where
            A: serde::Serialize + for<'de> serde::Deserialize<'de>,
            B: serde::Serialize + for<'de> serde::Deserialize<'de>,
        {
            pub left: A,
            pub right: B,
        }
    "#;
    let ts = generate_types_string(ScanInput::Source(src));
    assert!(ts.contains("export interface Pair<A, B>"));
    assert!(ts.contains("left: A"));
    assert!(ts.contains("right: B"));
}

//! Compile-time + runtime tests of `#[derive(Data)]` macro expansion.

use suprnova::data::Field;
use suprnova::Data;
use validator::Validate;

#[derive(Data, Validate, Debug)]
struct UserDto {
    pub id: i64,
    pub name: String,

    #[data(input_only)]
    #[validate(length(min = 8))]
    pub password: String,

    #[data(output_only)]
    pub computed_handle: String,
}

#[test]
fn serialize_skips_input_only_fields() {
    let u = UserDto {
        id: 1,
        name: "ada".into(),
        password: "secretkey".into(),
        computed_handle: "@ada".into(),
    };
    let j = serde_json::to_value(&u).unwrap();
    assert_eq!(j["id"], 1);
    assert_eq!(j["name"], "ada");
    assert!(j.get("password").is_none());
    assert_eq!(j["computed_handle"], "@ada");
}

#[test]
fn deserialize_accepts_input_payload() {
    let j = serde_json::json!({
        "id": 1,
        "name": "ada",
        "password": "secretkey",
    });
    let u: UserDto = serde_json::from_value(j).unwrap();
    assert_eq!(u.id, 1);
    assert_eq!(u.password, "secretkey");
    // output_only fields take their type's Default value on deserialize.
    assert_eq!(u.computed_handle, "");
}

#[test]
fn deserialize_rejects_output_only_in_payload() {
    let j = serde_json::json!({
        "id": 1,
        "name": "ada",
        "password": "secretkey",
        "computed_handle": "@ada",
    });
    let err = serde_json::from_value::<UserDto>(j).unwrap_err();
    assert!(err.to_string().contains("computed_handle"));
}

// ── Option<T> and Field<T> absent-default regression ─────────────────────

#[derive(Debug, Data, Validate)]
struct PatchUserDto {
    pub id: i64,
    pub name: Option<String>,
    pub bio: Field<String>,
}

#[test]
fn deserialize_treats_option_as_absent_default_none() {
    // Neither `name` nor `bio` is present — both should default rather than
    // trigger a missing_field error.
    let j = serde_json::json!({"id": 1});
    let u: PatchUserDto = serde_json::from_value(j).unwrap();
    assert_eq!(u.id, 1);
    assert!(u.name.is_none());
    assert!(u.bio.is_absent());
}

#[test]
fn deserialize_option_passes_through_value() {
    let j = serde_json::json!({"id": 1, "name": "ada", "bio": "hi"});
    let u: PatchUserDto = serde_json::from_value(j).unwrap();
    assert_eq!(u.name.as_deref(), Some("ada"));
    assert!(matches!(u.bio, Field::Value(_)));
}

#[test]
fn deserialize_option_null_yields_none() {
    // Explicit JSON null on an Option<T> field must deserialize to None,
    // not error.
    let j = serde_json::json!({"id": 1, "name": null});
    let u: PatchUserDto = serde_json::from_value(j).unwrap();
    assert!(u.name.is_none());
}

// ── #[data(allow_include)] inventory→registry pipeline ───────────────────

#[allow(non_camel_case_types)]
#[derive(suprnova::Data, Validate)]
struct _test_AlbumDto_t8 {
    pub id: i64,
    pub title: String,

    #[data(allow_include)]
    pub songs: Option<Vec<String>>,

    #[data(allow_include)]
    pub artist: Option<String>,
}

#[test]
fn allow_include_fields_register_via_inventory() {
    // `inventory::submit!` registers the AllowedIncludes entries at
    // link time; the first call to `is_allowed`/`allowed_for` drains
    // them into the runtime registry via `ensure_initialized()`.
    use suprnova::data::registry;

    assert!(registry::is_allowed("_test_AlbumDto_t8", "songs"));
    assert!(registry::is_allowed("_test_AlbumDto_t8", "artist"));
    assert!(!registry::is_allowed("_test_AlbumDto_t8", "title"));
    assert_eq!(
        registry::allowed_for("_test_AlbumDto_t8"),
        vec!["songs", "artist"]
    );
}

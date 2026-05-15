//! Compile-time + runtime tests of `#[derive(Data)]` macro expansion.

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

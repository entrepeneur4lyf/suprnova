use serde::{Deserialize, Serialize};
use suprnova::data::Field;

#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct Patch {
    #[serde(default, skip_serializing_if = "Field::is_absent")]
    bio: Field<String>,
}

#[test]
fn absent_when_key_missing() {
    let p: Patch = serde_json::from_str("{}").unwrap();
    assert_eq!(p.bio, Field::Absent);
}

#[test]
fn null_when_explicit_null() {
    let p: Patch = serde_json::from_str(r#"{"bio": null}"#).unwrap();
    assert_eq!(p.bio, Field::Null);
}

#[test]
fn value_when_present() {
    let p: Patch = serde_json::from_str(r#"{"bio": "hi"}"#).unwrap();
    assert_eq!(p.bio, Field::Value("hi".into()));
}

#[test]
fn round_trip_absent_omits_key() {
    let p = Patch { bio: Field::Absent };
    assert_eq!(serde_json::to_string(&p).unwrap(), "{}");
}

#[test]
fn round_trip_null_emits_null() {
    let p = Patch { bio: Field::Null };
    assert_eq!(serde_json::to_string(&p).unwrap(), r#"{"bio":null}"#);
}

#[test]
fn round_trip_value() {
    let p = Patch { bio: Field::Value("hi".into()) };
    assert_eq!(serde_json::to_string(&p).unwrap(), r#"{"bio":"hi"}"#);
}

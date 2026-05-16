use std::sync::Arc;
use suprnova::data::{current_include_set, IncludeError, RequestIncludeSet, REQUEST_INCLUDE_SET};

#[test]
fn parses_include() {
    let s = RequestIncludeSet::from_query("include=foo,bar");
    assert_eq!(s.include, vec!["foo", "bar"]);
    assert!(s.exclude.is_empty());
    assert!(s.only.is_none());
    assert!(s.except.is_empty());
}

#[test]
fn parses_all_four_keys() {
    let s = RequestIncludeSet::from_query(
        "include=a&exclude=b&only=c,d&except=e",
    );
    assert_eq!(s.include, vec!["a"]);
    assert_eq!(s.exclude, vec!["b"]);
    assert_eq!(s.only, Some(vec!["c".into(), "d".into()]));
    assert_eq!(s.except, vec!["e"]);
}

#[test]
fn empty_query_yields_empty_set() {
    let s = RequestIncludeSet::from_query("");
    assert!(s.is_empty());
}

#[test]
fn trims_whitespace_and_drops_empty() {
    let s = RequestIncludeSet::from_query("include= foo , , bar ");
    assert_eq!(s.include, vec!["foo", "bar"]);
}

#[test]
fn array_form_include_brackets() {
    // include[]=foo&include[]=bar — Laravel-style array form.
    let s = RequestIncludeSet::from_query("include[]=foo&include[]=bar");
    assert_eq!(s.include, vec!["foo", "bar"]);
}

// HIGH 1 — includes() API
#[test]
fn includes_finds_field() {
    let s = RequestIncludeSet::from_query("include=foo,bar");
    assert!(s.includes("foo"));
    assert!(s.includes("bar"));
    assert!(!s.includes("baz"));
    assert!(!s.includes(""));
}

#[test]
fn includes_returns_false_on_empty_set() {
    let s = RequestIncludeSet::default();
    assert!(!s.includes("anything"));
}

// HIGH 2 — IncludeError::into_framework_error()
#[test]
fn include_error_into_framework_error_produces_400() {
    let err = IncludeError::UnknownInclude {
        field: "secret".into(),
        allowed: vec!["author".into(), "tags".into()],
    };
    let fw = err.into_framework_error();
    assert_eq!(fw.status_code(), 400);
    let msg = format!("{}", fw);
    assert!(msg.contains("secret"), "message should name the offending field, got: {msg}");
    assert!(msg.contains("author"), "message should list the allowed fields, got: {msg}");
    assert!(msg.contains("tags"));
}

// HIGH 3 — current_include_set() both paths
#[tokio::test]
async fn current_include_set_unbound_returns_empty() {
    let set = current_include_set();
    assert!(set.is_empty());
}

#[tokio::test]
async fn current_include_set_bound_returns_scoped_value() {
    let bound = Arc::new(RequestIncludeSet {
        include: vec!["author".into()],
        ..Default::default()
    });
    let observed = REQUEST_INCLUDE_SET
        .scope(bound.clone(), async { current_include_set() })
        .await;
    assert_eq!(observed.include, vec!["author"]);
    assert!(Arc::ptr_eq(&observed, &bound), "should hand back the same Arc");
}

// Codex review finding 9 — URL decoding ------------------------------

#[test]
fn url_decoded_comma_splits_into_separate_values() {
    // `%2C` is `,` — must be decoded before the `,` split.
    let s = RequestIncludeSet::from_query("include=author%2Ccomments");
    assert_eq!(s.include, vec!["author", "comments"]);
}

#[test]
fn url_decoded_brackets_recognized_as_array_form() {
    // `%5B%5D` is `[]` — array form must work after decoding.
    let s = RequestIncludeSet::from_query("include%5B%5D=foo&include%5B%5D=bar");
    assert_eq!(s.include, vec!["foo", "bar"]);
}

#[test]
fn url_decoded_plus_becomes_space_then_trimmed() {
    // `+` decodes to space; the value-trim step then drops the
    // leading/trailing whitespace. A bare-space value collapses to
    // an empty token which gets filtered out.
    let s = RequestIncludeSet::from_query("include=foo+&include=bar");
    assert_eq!(s.include, vec!["foo", "bar"]);
}

#[test]
fn url_decoded_repeated_keys_merge_across_encoding() {
    // Mix encoded and plain — both forms must merge into one list.
    let s = RequestIncludeSet::from_query("include=a%2Cb&include=c");
    assert_eq!(s.include, vec!["a", "b", "c"]);
}

#[test]
fn url_decoded_value_with_encoded_only_field() {
    let s = RequestIncludeSet::from_query("only=id%2Cname");
    assert_eq!(s.only, Some(vec!["id".into(), "name".into()]));
}

#[test]
fn url_decoded_malformed_percent_is_lossy_not_panic() {
    // `%ZZ` is not a valid percent escape. `form_urlencoded::parse`
    // tolerates it lossily; we just need to confirm the call
    // returns without panicking and still parses the surrounding
    // valid pairs.
    let s = RequestIncludeSet::from_query("include=foo%ZZbar&exclude=baz");
    // The decoded include value contains the raw `%ZZ` bytes (lossy
    // decode keeps them); the exclude value should still come through
    // cleanly.
    assert_eq!(s.exclude, vec!["baz"]);
    assert!(!s.include.is_empty(), "malformed percent must not drop the pair");
}

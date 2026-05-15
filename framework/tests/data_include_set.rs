use suprnova::data::RequestIncludeSet;

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

use suprnova::data::registry::{allowed_for, is_allowed, register};

#[test]
fn end_to_end_register_and_lookup() {
    register("_test_ItemDto", &["expansion"]);
    assert!(is_allowed("_test_ItemDto", "expansion"));
    assert_eq!(allowed_for("_test_ItemDto"), vec!["expansion"]);
    assert!(!is_allowed("_test_ItemDto", "nope"));
}

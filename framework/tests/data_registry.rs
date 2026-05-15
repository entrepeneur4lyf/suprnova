use suprnova::data::registry::{allowed_for, is_allowed, register};

#[test]
fn end_to_end_register_and_lookup() {
    register("ItemDto", &["expansion"]);
    assert!(is_allowed("ItemDto", "expansion"));
    assert_eq!(allowed_for("ItemDto"), vec!["expansion"]);
    assert!(!is_allowed("ItemDto", "nope"));
}

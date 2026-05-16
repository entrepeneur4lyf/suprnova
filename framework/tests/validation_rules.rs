//! Integration tests for the rule-object primitives in
//! `suprnova::validation::rule`.

use suprnova::validation::rule::{
    rules::{Email, Max, Min, Required},
    Rule,
};

#[test]
fn required_passes_on_present() {
    let r = Required;
    assert!(r.passes("not empty").is_ok());
    assert!(r.passes("").is_err());
    assert!(
        r.passes("   ").is_err(),
        "all-whitespace counts as empty"
    );
}

#[test]
fn email_accepts_well_formed_addresses() {
    let r = Email;
    assert!(r.passes("user@example.com").is_ok());
    assert!(r.passes("user+filter@sub.example.co.uk").is_ok());
}

#[test]
fn email_rejects_malformed_addresses() {
    let r = Email;
    // The `validator` crate rejects these:
    assert!(r.passes("not-an-email").is_err());
    assert!(r.passes("@nodomain").is_err());
    assert!(r.passes("noatsign.com").is_err());
    assert!(r.passes("trailing.dot@x.").is_err());
}

#[test]
fn min_max_check_length() {
    let r = Min(8);
    assert!(r.passes("longenough").is_ok());
    assert!(r.passes("short").is_err());

    let r = Max(5);
    assert!(r.passes("hi").is_ok());
    assert!(r.passes("toolong").is_err());
}

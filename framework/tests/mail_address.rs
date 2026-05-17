use suprnova::mail::address::{Address, Attachment};

#[test]
fn address_parses_from_email_only() {
    let a: Address = "alice@example.org".into();
    assert_eq!(a.email, "alice@example.org");
    assert_eq!(a.name, None);
}

#[test]
fn address_from_tuple_carries_name() {
    let a: Address = ("Alice".to_string(), "alice@example.org".to_string()).into();
    assert_eq!(a.email, "alice@example.org");
    assert_eq!(a.name.as_deref(), Some("Alice"));
}

#[test]
fn address_display_renders_rfc5322_when_name_present() {
    let a = Address { email: "a@b.c".into(), name: Some("Alice".into()) };
    assert_eq!(a.to_string(), "Alice <a@b.c>");

    let bare = Address { email: "a@b.c".into(), name: None };
    assert_eq!(bare.to_string(), "a@b.c");
}

#[test]
fn attachment_holds_filename_content_and_mime() {
    let a = Attachment {
        filename: "invoice.pdf".into(),
        content: b"%PDF-1.4".to_vec(),
        content_type: "application/pdf".into(),
    };
    assert_eq!(a.filename, "invoice.pdf");
    assert_eq!(a.content.len(), 8);
    assert_eq!(a.content_type, "application/pdf");
}

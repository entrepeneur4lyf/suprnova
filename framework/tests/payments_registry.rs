use std::sync::Arc;
use suprnova::payments::*;

#[test]
fn registry_bind_and_get() {
    let mock = Arc::new(MockPaymentProvider::new()) as Arc<dyn PaymentProvider>;
    PaymentProviderRegistry::bind("mock-bind-get", mock.clone());
    let got = PaymentProviderRegistry::get("mock-bind-get")
        .expect("provider not in registry after bind");
    assert_eq!(got.name(), "mock");
}

#[test]
fn registry_get_unknown_returns_none() {
    assert!(PaymentProviderRegistry::get("not-a-provider").is_none());
}

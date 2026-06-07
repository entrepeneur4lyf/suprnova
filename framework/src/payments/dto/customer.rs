use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Provider-side view of a billing customer.
///
/// `user_id` is `Option<String>` because the upstream provider does not
/// store the app's user identifier as a first-class field on its
/// customer object. Provider impls that mint a fresh record from a
/// [`CreateCustomerRequest`] propagate the caller-supplied `user_id`
/// (so the immediate response carries it), but
/// [`super::super::traits::CustomerStore::update_customer`] and
/// [`super::super::traits::CustomerStore::get_customer`] return `None`
/// because the provider only knows about `provider_customer_id` and
/// has no reverse lookup. Callers that need the app `user_id` for those
/// paths must read the DB mirror entity directly — the mirror row is
/// the authoritative source for app-side identifiers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomerRef {
    pub provider_customer_id: String,
    pub user_id: Option<String>,
    pub email: String,
    pub provider_metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateCustomerRequest {
    pub user_id: String,
    pub email: String,
    pub name: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCustomerRequest {
    pub provider_customer_id: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub metadata: Option<Value>,
}

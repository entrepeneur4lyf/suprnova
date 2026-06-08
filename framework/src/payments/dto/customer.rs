//! Customer DTOs — request and response shapes for the [`super::super::traits::CustomerStore`] trait.

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
    /// Provider-issued customer identifier (e.g. Stripe's `cus_…`).
    pub provider_customer_id: String,
    /// App-side user identifier; see the type docs for why this may be
    /// `None` on read paths.
    pub user_id: Option<String>,
    /// Customer billing email as known to the provider.
    pub email: String,
    /// JSON snapshot of the provider's customer object — preserved
    /// verbatim so callers can read fields the DTO does not flatten.
    pub provider_metadata: Value,
}

/// Request payload for [`super::super::traits::CustomerStore::create_customer`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateCustomerRequest {
    /// App-side user identifier — round-trips back on
    /// [`CustomerRef::user_id`] in the immediate response.
    pub user_id: String,
    /// Billing email; surfaced on receipts and Provider dashboards.
    pub email: String,
    /// Optional display name.
    pub name: Option<String>,
    /// Free-form provider metadata to attach to the new customer record.
    pub metadata: Option<Value>,
}

/// Request payload for [`super::super::traits::CustomerStore::update_customer`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCustomerRequest {
    /// Provider-issued customer identifier to update.
    pub provider_customer_id: String,
    /// New billing email, or `None` to leave unchanged.
    pub email: Option<String>,
    /// New display name, or `None` to leave unchanged.
    pub name: Option<String>,
    /// Provider metadata to merge in, or `None` to leave unchanged.
    pub metadata: Option<Value>,
}

//! Implementation of the `CustomerStore` trait for `StripeProvider`.
//!
//! Maps Suprnova's provider-neutral customer lifecycle onto Stripe's
//! `/v1/customers` API.

use crate::StripeProvider;
use async_trait::async_trait;
use serde::Serialize;
use std::collections::HashMap;
use stripe_client_core::{RequestBuilder, StripeMethod};
use stripe_shared::{Customer, DeletedCustomer};
use suprnova::payments::{
    CreateCustomerRequest, CustomerRef, CustomerStore, PaymentError, PaymentResult,
    UpdateCustomerRequest,
};

// ---------------------------------------------------------------------------
// Param structs
// ---------------------------------------------------------------------------
//
// Stripe accepts metadata as bracketed form pairs (`metadata[key]=value`).
// `stripe_client_core::RequestBuilder::form` serialises with `serde_qs`, which
// renders a `HashMap<String, String>` exactly in that shape — matching the
// `CreateCustomerBuilder` field type in async-stripe-core's own customer
// requests module.

#[derive(Serialize)]
struct CreateCustomerParams<'a> {
    email: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<HashMap<String, String>>,
}

#[derive(Serialize)]
struct UpdateCustomerParams<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<HashMap<String, String>>,
}

/// Flatten the public `Option<serde_json::Value>` metadata input into the
/// Stripe-friendly `Option<HashMap<String, String>>` shape.
///
/// Stripe metadata is a flat map of strings to strings. Top-level scalar
/// values are stringified (numbers, booleans, etc.); nested objects/arrays
/// are JSON-encoded so a value still round-trips, matching how Stripe's
/// own dashboard renders complex metadata pasted into the field.
///
/// `None` and `Some(serde_json::Value::Null)` both produce `None` so the
/// `#[serde(skip_serializing_if = "Option::is_none")]` attribute keeps the
/// outgoing form body empty when no metadata was supplied.
fn metadata_to_string_map(value: Option<&serde_json::Value>) -> Option<HashMap<String, String>> {
    let obj = value?.as_object()?;
    if obj.is_empty() {
        return None;
    }
    let mut map = HashMap::with_capacity(obj.len());
    for (k, v) in obj {
        let s = match v {
            serde_json::Value::Null => continue,
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        map.insert(k.clone(), s);
    }
    if map.is_empty() { None } else { Some(map) }
}

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

fn customer_to_ref(
    c: Customer,
    user_id: Option<String>,
    metadata: serde_json::Value,
) -> CustomerRef {
    CustomerRef {
        provider_customer_id: c.id.as_str().to_string(),
        user_id,
        email: c.email.unwrap_or_default(),
        provider_metadata: metadata,
    }
}

// ---------------------------------------------------------------------------
// Trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl CustomerStore for StripeProvider {
    async fn create_customer(&self, req: CreateCustomerRequest) -> PaymentResult<CustomerRef> {
        let params = CreateCustomerParams {
            email: &req.email,
            name: req.name.as_deref(),
            metadata: metadata_to_string_map(req.metadata.as_ref()),
        };

        let c: Customer = RequestBuilder::new(StripeMethod::Post, "/customers")
            .form(&params)
            .customize::<Customer>()
            .send(self.client())
            .await
            .map_err(|e| PaymentError::Provider(format!("stripe customers.create: {e}")))?;

        Ok(customer_to_ref(
            c,
            Some(req.user_id),
            req.metadata.unwrap_or(serde_json::json!({})),
        ))
    }

    async fn update_customer(&self, req: UpdateCustomerRequest) -> PaymentResult<CustomerRef> {
        let path = format!("/customers/{}", req.provider_customer_id);
        let params = UpdateCustomerParams {
            email: req.email.as_deref(),
            name: req.name.as_deref(),
            metadata: metadata_to_string_map(req.metadata.as_ref()),
        };

        let c: Customer = RequestBuilder::new(StripeMethod::Post, &path)
            .form(&params)
            .customize::<Customer>()
            .send(self.client())
            .await
            .map_err(|e| PaymentError::Provider(format!("stripe customers.update: {e}")))?;

        // update_customer returns user_id: None because Stripe's
        // Customer object doesn't carry the app's user identifier as a
        // first-class field — callers that need the app-side id should
        // read the DB mirror entity. See CustomerRef::user_id docs.
        Ok(customer_to_ref(
            c,
            None,
            req.metadata.unwrap_or(serde_json::json!({})),
        ))
    }

    async fn get_customer(&self, provider_customer_id: &str) -> PaymentResult<CustomerRef> {
        let path = format!("/customers/{provider_customer_id}");
        let c: Customer = RequestBuilder::new(StripeMethod::Get, &path)
            .customize::<Customer>()
            .send(self.client())
            .await
            .map_err(|e| PaymentError::Provider(format!("stripe customers.retrieve: {e}")))?;

        // See update_customer above for why user_id is None on the
        // get path.
        Ok(customer_to_ref(c, None, serde_json::json!({})))
    }

    async fn delete_customer(&self, provider_customer_id: &str) -> PaymentResult<()> {
        let path = format!("/customers/{provider_customer_id}");
        // Stripe customer deletion returns a DeletedCustomer object.
        // We only care that the call succeeded — discard the result.
        let _: DeletedCustomer = RequestBuilder::new(StripeMethod::Delete, &path)
            .customize::<DeletedCustomer>()
            .send(self.client())
            .await
            .map_err(|e| PaymentError::Provider(format!("stripe customers.delete: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn metadata_to_string_map_none_when_input_none() {
        assert!(metadata_to_string_map(None).is_none());
    }

    #[test]
    fn metadata_to_string_map_none_when_input_null() {
        let v = serde_json::Value::Null;
        assert!(metadata_to_string_map(Some(&v)).is_none());
    }

    #[test]
    fn metadata_to_string_map_none_when_input_empty_object() {
        let v = json!({});
        assert!(metadata_to_string_map(Some(&v)).is_none());
    }

    #[test]
    fn metadata_to_string_map_none_when_input_not_an_object() {
        // Non-object JSON (string, array, number) cannot be Stripe metadata
        // and should be skipped rather than silently mis-encoded.
        for v in [json!("just-a-string"), json!([1, 2, 3]), json!(42)] {
            assert!(metadata_to_string_map(Some(&v)).is_none(), "{v:?}");
        }
    }

    #[test]
    fn metadata_to_string_map_string_values_pass_through_unquoted() {
        let v = json!({ "plan": "pro", "tier": "gold" });
        let map = metadata_to_string_map(Some(&v)).expect("map present");
        assert_eq!(map.get("plan").map(String::as_str), Some("pro"));
        assert_eq!(map.get("tier").map(String::as_str), Some("gold"));
    }

    #[test]
    fn metadata_to_string_map_scalars_are_stringified() {
        let v = json!({ "seats": 5, "trial": true });
        let map = metadata_to_string_map(Some(&v)).expect("map present");
        assert_eq!(map.get("seats").map(String::as_str), Some("5"));
        assert_eq!(map.get("trial").map(String::as_str), Some("true"));
    }

    #[test]
    fn metadata_to_string_map_skips_null_values() {
        let v = json!({ "valid": "yes", "skipped": serde_json::Value::Null });
        let map = metadata_to_string_map(Some(&v)).expect("map present");
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("valid"));
        assert!(!map.contains_key("skipped"));
    }
}

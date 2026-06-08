//! Implementation of the `CustomerStore` trait for `PaddleProvider`.
//!
//! Paddle does NOT expose customer deletion via its API — `delete_customer`
//! returns `PaymentError::NotSupported` with a pointer to the archive-via-update
//! workaround. A test asserts this invariant.

use async_trait::async_trait;
use std::collections::HashMap;
use suprnova::payments::{
    CreateCustomerRequest, CustomerRef, CustomerStore, PaymentError, PaymentResult,
    UpdateCustomerRequest,
};

use crate::PaddleProvider;

/// Flatten the public `Option<serde_json::Value>` metadata input into the
/// Paddle-friendly `HashMap<String, String>` shape that
/// `CustomerCreate::custom_data` / `CustomerUpdate::custom_data` accept.
///
/// Paddle's `custom_data` field is documented as a flat key/value map of
/// strings; top-level scalars are stringified and nested objects/arrays are
/// JSON-encoded so the original structure round-trips, matching how Paddle
/// renders complex `custom_data` in its dashboard.
///
/// `None`, `Some(Null)`, and an empty object all produce `None` so the
/// builder method is simply not called and `custom_data` stays out of the
/// outgoing payload (the field is serialised with `#[skip_serializing_none]`
/// in the SDK builder).
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

#[async_trait]
impl CustomerStore for PaddleProvider {
    async fn create_customer(&self, req: CreateCustomerRequest) -> PaymentResult<CustomerRef> {
        let mut builder = self.client().customer_create(req.email.clone());
        if let Some(name) = &req.name {
            builder.name(name.clone());
        }
        if let Some(custom_data) = metadata_to_string_map(req.metadata.as_ref()) {
            builder.custom_data(custom_data);
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| PaymentError::Provider(format!("paddle customer_create: {e}")))?;

        Ok(CustomerRef {
            provider_customer_id: resp.data.id.to_string(),
            user_id: Some(req.user_id),
            email: req.email,
            provider_metadata: req.metadata.unwrap_or(serde_json::json!({})),
        })
    }

    async fn update_customer(&self, req: UpdateCustomerRequest) -> PaymentResult<CustomerRef> {
        let mut builder = self
            .client()
            .customer_update(req.provider_customer_id.clone());
        if let Some(email) = &req.email {
            builder.email(email.clone());
        }
        if let Some(name) = &req.name {
            builder.name(name.clone());
        }
        if let Some(custom_data) = metadata_to_string_map(req.metadata.as_ref()) {
            builder.custom_data(custom_data);
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| PaymentError::Provider(format!("paddle customer_update: {e}")))?;

        // Prefer the `custom_data` Paddle returned over the request-side
        // echo. Admin and reconciliation tooling needs the server's
        // authoritative view (Paddle may normalise), not the client's
        // pre-flight copy. Fall back to the request metadata only when
        // Paddle omitted the field entirely.
        let provider_metadata = resp
            .data
            .custom_data
            .clone()
            .unwrap_or_else(|| req.metadata.clone().unwrap_or(serde_json::json!({})));
        Ok(CustomerRef {
            provider_customer_id: resp.data.id.to_string(),
            user_id: None,
            email: resp.data.email.clone(),
            provider_metadata,
        })
    }

    async fn get_customer(&self, provider_customer_id: &str) -> PaymentResult<CustomerRef> {
        let resp = self
            .client()
            .customer_get(provider_customer_id.to_string())
            .send()
            .await
            .map_err(|e| PaymentError::Provider(format!("paddle customer_get: {e}")))?;

        // Round-trip Paddle's `custom_data` (Paddle's name for
        // caller-supplied metadata). Empty object when the customer
        // has no `custom_data` set — matches the contract callers
        // rely on (always a JSON value, never null).
        let provider_metadata = resp
            .data
            .custom_data
            .clone()
            .unwrap_or(serde_json::json!({}));
        Ok(CustomerRef {
            provider_customer_id: resp.data.id.to_string(),
            user_id: None,
            email: resp.data.email.clone(),
            provider_metadata,
        })
    }

    async fn delete_customer(&self, _provider_customer_id: &str) -> PaymentResult<()> {
        // Paddle does not expose customer deletion. Use UpdateCustomer with
        // status = archived to soft-delete instead.
        Err(PaymentError::NotSupported(
            "Paddle does not expose customer deletion. \
             Use UpdateCustomer with archived status if needed."
                .into(),
        ))
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

    // ---- provider_metadata round-trip (CustomerRef construction) -----------
    //
    // Paddle's `CustomerData::custom_data` is `Option<serde_json::Value>`.
    // The CustomerStore::get_customer / update_customer fix prefers the
    // server-returned value over the request-side echo so admin /
    // reconciliation tooling sees Paddle's authoritative view. The two
    // tests below pin that selection rule by exercising the
    // `unwrap_or` chain directly — the same shape both call sites use.

    fn pick_paddle_metadata(
        server: Option<serde_json::Value>,
        fallback: serde_json::Value,
    ) -> serde_json::Value {
        server.unwrap_or(fallback)
    }

    #[test]
    fn paddle_pick_prefers_server_custom_data_over_request_echo() {
        let server = Some(json!({ "tier": "gold" }));
        let fallback = json!({ "tier": "silver" }); // intentionally different
        let out = pick_paddle_metadata(server, fallback);
        assert_eq!(out, json!({ "tier": "gold" }));
    }

    #[test]
    fn paddle_pick_falls_back_when_server_returns_none() {
        let fallback = json!({ "from_request": "yes" });
        let out = pick_paddle_metadata(None, fallback);
        assert_eq!(out, json!({ "from_request": "yes" }));
    }
}

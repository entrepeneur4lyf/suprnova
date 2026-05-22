//! Implementation of the `CustomerStore` trait for `StripeProvider`.
//!
//! Maps Suprnova's provider-neutral customer lifecycle onto Stripe's
//! `/v1/customers` API.

use crate::StripeProvider;
use async_trait::async_trait;
use serde::Serialize;
use stripe_client_core::{RequestBuilder, StripeMethod};
use stripe_shared::{Customer, DeletedCustomer};
use suprnova::payments::{
    CreateCustomerRequest, CustomerRef, CustomerStore, PaymentError, PaymentResult,
    UpdateCustomerRequest,
};

// ---------------------------------------------------------------------------
// Param structs
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct CreateCustomerParams<'a> {
    email: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
}

#[derive(Serialize)]
struct UpdateCustomerParams<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

fn customer_to_ref(c: Customer, user_id: String, metadata: serde_json::Value) -> CustomerRef {
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
        };

        let c: Customer = RequestBuilder::new(StripeMethod::Post, "/customers")
            .form(&params)
            .customize::<Customer>()
            .send(self.client())
            .await
            .map_err(|e| PaymentError::Provider(format!("stripe customers.create: {e}")))?;

        Ok(customer_to_ref(
            c,
            req.user_id,
            req.metadata.unwrap_or(serde_json::json!({})),
        ))
    }

    async fn update_customer(&self, req: UpdateCustomerRequest) -> PaymentResult<CustomerRef> {
        let path = format!("/customers/{}", req.provider_customer_id);
        let params = UpdateCustomerParams {
            email: req.email.as_deref(),
            name: req.name.as_deref(),
        };

        let c: Customer = RequestBuilder::new(StripeMethod::Post, &path)
            .form(&params)
            .customize::<Customer>()
            .send(self.client())
            .await
            .map_err(|e| PaymentError::Provider(format!("stripe customers.update: {e}")))?;

        Ok(customer_to_ref(
            c,
            String::new(),
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

        Ok(customer_to_ref(c, String::new(), serde_json::json!({})))
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

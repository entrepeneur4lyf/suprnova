//! Implementation of the `CustomerStore` trait for `PaddleProvider`.
//!
//! Paddle does NOT expose customer deletion via its API — `delete_customer`
//! returns `PaymentError::NotSupported` with a pointer to the archive-via-update
//! workaround. A test asserts this invariant.

use async_trait::async_trait;
use suprnova::payments::{
    CreateCustomerRequest, CustomerRef, CustomerStore, PaymentError, PaymentResult,
    UpdateCustomerRequest,
};

use crate::PaddleProvider;

#[async_trait]
impl CustomerStore for PaddleProvider {
    async fn create_customer(&self, req: CreateCustomerRequest) -> PaymentResult<CustomerRef> {
        let mut builder = self.client().customer_create(req.email.clone());
        if let Some(name) = &req.name {
            builder.name(name.clone());
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| PaymentError::Provider(format!("paddle customer_create: {e}")))?;

        Ok(CustomerRef {
            provider_customer_id: resp.data.id.to_string(),
            user_id: req.user_id,
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

        let resp = builder
            .send()
            .await
            .map_err(|e| PaymentError::Provider(format!("paddle customer_update: {e}")))?;

        Ok(CustomerRef {
            provider_customer_id: resp.data.id.to_string(),
            user_id: String::new(),
            email: resp.data.email.clone(),
            provider_metadata: req.metadata.unwrap_or(serde_json::json!({})),
        })
    }

    async fn get_customer(&self, provider_customer_id: &str) -> PaymentResult<CustomerRef> {
        let resp = self
            .client()
            .customer_get(provider_customer_id.to_string())
            .send()
            .await
            .map_err(|e| PaymentError::Provider(format!("paddle customer_get: {e}")))?;

        Ok(CustomerRef {
            provider_customer_id: resp.data.id.to_string(),
            user_id: String::new(),
            email: resp.data.email.clone(),
            provider_metadata: serde_json::json!({}),
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

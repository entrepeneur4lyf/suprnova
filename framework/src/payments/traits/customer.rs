//! Customer-record CRUD trait for payment providers.

use crate::payments::{CreateCustomerRequest, CustomerRef, PaymentResult, UpdateCustomerRequest};
use async_trait::async_trait;

/// Provider-side customer record management.
///
/// Every provider implements this — the provider holds the canonical
/// billing record (email, name, default payment instrument), and the
/// framework's mirror table holds the `(provider, user_id)` join back
/// to the app's identity.
#[async_trait]
pub trait CustomerStore: Send + Sync {
    /// Create a fresh customer record on the provider.
    async fn create_customer(&self, req: CreateCustomerRequest) -> PaymentResult<CustomerRef>;
    /// Update an existing customer record. Providers that don't expose
    /// the requested field return [`super::super::PaymentError::NotSupported`].
    async fn update_customer(&self, req: UpdateCustomerRequest) -> PaymentResult<CustomerRef>;
    /// Fetch a customer record by its provider-side identifier.
    async fn get_customer(&self, provider_customer_id: &str) -> PaymentResult<CustomerRef>;
    /// Delete (or soft-delete, per provider semantics) a customer record.
    async fn delete_customer(&self, provider_customer_id: &str) -> PaymentResult<()>;
}

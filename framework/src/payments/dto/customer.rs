use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomerRef {
    pub provider_customer_id: String,
    pub user_id: String,
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

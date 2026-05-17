//! HTTP push client (reqwest 0.13). Fleshed out in Task 5.

use serde::{Deserialize, Serialize};

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubscriptionInfo {
    pub endpoint: String,
    pub keys: SubscriptionKeys,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubscriptionKeys {
    pub p256dh: String,
    pub auth: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct PushResponse {
    pub status: u16,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct WebPushClient;

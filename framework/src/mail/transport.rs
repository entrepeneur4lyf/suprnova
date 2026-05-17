//! MailTransport trait + rendered-message representation.

use crate::error::FrameworkError;
use crate::mail::address::{Address, Attachment};
use async_trait::async_trait;

/// A fully-rendered outgoing message — what transports receive.
#[derive(Debug, Clone)]
pub struct OutgoingMessage {
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub subject: String,
    pub html: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
}

#[async_trait]
pub trait MailTransport: Send + Sync {
    async fn send(&self, msg: &OutgoingMessage) -> Result<(), FrameworkError>;
    fn name(&self) -> &'static str { std::any::type_name::<Self>() }
}

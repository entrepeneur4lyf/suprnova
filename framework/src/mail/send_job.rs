//! `SendMailJob` — the framework-shipped Job that processes `Mail::queue`
//! and `Mail::later` dispatches via the Phase 5A FROZEN envelope.
//!
//! The job carries the routed recipients + the mailable's `(name, payload)`
//! pair. On `handle`, the worker rebuilds the mailable through the
//! [`mailable_registry`], renders via the same Tera-defaulted path as
//! `Mail::send`, and ships through the bound mail transport.

use crate::error::FrameworkError;
use crate::mail::mailable_registry;
use crate::mail::{dispatch_with_telemetry, Address, Mail};
use crate::queue::Job;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SendMailJob {
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub from_override: Option<Address>,
    pub mailable_name: String,
    pub mailable_payload: serde_json::Value,
}

#[async_trait]
impl Job for SendMailJob {
    fn job_name() -> &'static str {
        "Suprnova::SendMail"
    }

    async fn handle(self) -> Result<(), FrameworkError> {
        let any = mailable_registry::build(&self.mailable_name, self.mailable_payload)?;
        let msg = mailable_registry::render_outgoing(
            any.as_ref(),
            &self.mailable_name,
            self.to,
            self.cc,
            self.bcc,
            self.reply_to,
            self.from_override,
        )?;
        let transport = Mail::current_transport()?;
        dispatch_with_telemetry(transport.as_ref(), &msg).await
    }
}

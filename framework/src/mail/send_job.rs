//! `SendMailJob` — the framework-shipped Job that processes `Mail::queue`
//! and `Mail::later` dispatches via the Phase 5A FROZEN envelope.
//!
//! The job carries the routed recipients + the mailable's `(name, payload)`
//! pair, plus per-builder tags / metadata / priority / headers /
//! return-path that the caller layered on top. On `handle`, the worker
//! rebuilds the mailable through the [`mailable_registry`], renders via
//! the same Tera-defaulted path as `Mail::send`, and ships through the
//! bound mail transport.

use crate::error::FrameworkError;
use crate::mail::address::Attachment;
use crate::mail::mailable_registry;
use crate::mail::{Address, Mail, dispatch_with_telemetry};
use crate::queue::Job;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Worker-side payload that `Mail::queue` and `Mail::later` push onto the
/// queue. Carries the routed recipients plus the mailable's
/// `(name, payload)` pair so the worker can rebuild the typed mailable
/// via [`mailable_registry`] and render through the same path as
/// `Mail::send`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SendMailJob {
    /// Primary recipients (mirrors `MailBuilder::to`).
    pub to: Vec<Address>,
    /// CC recipients.
    pub cc: Vec<Address>,
    /// BCC recipients.
    pub bcc: Vec<Address>,
    /// Reply-To addresses.
    pub reply_to: Vec<Address>,
    /// Builder-side `from` override; defaults to the mailable's `from()`
    /// when `None`.
    pub from_override: Option<Address>,
    /// Stable `Mailable::mailable_name()` used to look up the registered
    /// factory on the worker side.
    pub mailable_name: String,
    /// Serialized mailable payload (deserialized back into `M` on dispatch).
    pub mailable_payload: serde_json::Value,
    /// Provider tags layered on by the builder.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Provider metadata layered on by the builder.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    /// Message priority (1 = highest, 5 = lowest).
    #[serde(default)]
    pub priority: Option<u8>,
    /// Custom MIME headers layered on by the builder.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Builder-side Return-Path override; falls back to the mailable.
    #[serde(default)]
    pub return_path: Option<Address>,
    /// Builder-side subject override. When `Some`, replaces the mailable's
    /// `render_subject()` output on the queue path — matches the send
    /// path's precedence (see `MailBuilder::send`).
    #[serde(default)]
    pub subject_override: Option<String>,
    /// Builder-side extra attachments. Appended after the mailable's own
    /// `attachments()` — matches the send path's order.
    #[serde(default)]
    pub attachments: Vec<Attachment>,
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
            self.tags,
            self.metadata,
            self.priority,
            self.headers,
            self.return_path,
            self.subject_override,
            self.attachments,
        )?;
        // Apply Mail::always_* defaults on the queue side too. Without
        // this the queue path would bypass `always_from` / `always_to`
        // / etc., creating an observable divergence from `Mail::send`.
        let msg = Mail::apply_always_defaults(msg);
        let transport = Mail::current_transport()?;
        dispatch_with_telemetry(transport.as_ref(), &msg).await
    }
}

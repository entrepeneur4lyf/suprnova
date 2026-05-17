//! Mail subsystem.

pub mod address;
pub(crate) mod http_provider;
pub mod log;
pub mod mailable;
pub mod mailgun;
pub mod memory;
pub mod postmark;
pub mod sendgrid;
pub mod ses;
pub mod smtp;
pub mod transport;

pub use address::{Address, Attachment};
pub use mailable::Mailable;
pub use transport::{MailTransport, OutgoingMessage};

use crate::error::FrameworkError;
use std::sync::{Arc, RwLock};

static TRANSPORT: RwLock<Option<Arc<dyn MailTransport>>> = RwLock::new(None);

pub struct Mail;

impl Mail {
    pub fn set_transport(transport: Arc<dyn MailTransport>) {
        *TRANSPORT.write().expect("mail transport lock poisoned") = Some(transport);
    }
    pub fn clear_transport() {
        *TRANSPORT.write().expect("mail transport lock poisoned") = None;
    }

    pub fn to(addr: impl Into<Address>) -> MailBuilder {
        MailBuilder::default().to(addr)
    }

    pub(crate) fn current_transport() -> Result<Arc<dyn MailTransport>, FrameworkError> {
        TRANSPORT
            .read()
            .expect("mail transport lock poisoned")
            .clone()
            .ok_or_else(|| FrameworkError::internal(
                "no mail transport configured; call Mail::set_transport(...) or run Mail::bootstrap_from_env()"
            ))
    }
}

#[derive(Default, Debug)]
pub struct MailBuilder {
    to: Vec<Address>,
    cc: Vec<Address>,
    bcc: Vec<Address>,
    reply_to: Vec<Address>,
    from_override: Option<Address>,
}

impl MailBuilder {
    pub fn to(mut self, addr: impl Into<Address>) -> Self { self.to.push(addr.into()); self }
    pub fn cc(mut self, addr: impl Into<Address>) -> Self { self.cc.push(addr.into()); self }
    pub fn bcc(mut self, addr: impl Into<Address>) -> Self { self.bcc.push(addr.into()); self }
    pub fn reply_to(mut self, addr: impl Into<Address>) -> Self { self.reply_to.push(addr.into()); self }
    pub fn from(mut self, addr: impl Into<Address>) -> Self { self.from_override = Some(addr.into()); self }

    /// Render `mailable` and dispatch to the bound transport.
    pub async fn send<M: Mailable>(self, mailable: M) -> Result<(), FrameworkError> {
        let transport = Mail::current_transport()?;
        let from = self.from_override
            .or_else(|| mailable.from())
            .unwrap_or_else(|| Address::new("noreply@localhost"));

        let html = mailable.render_html()?;
        let text = mailable.render_text()?;

        if html.is_none() && text.is_none() {
            return Err(FrameworkError::internal(format!(
                "mail: {} has no text or html body — define text_template_source or html_template_source on the Mailable",
                M::mailable_name()
            )));
        }

        let msg = OutgoingMessage {
            from,
            to: self.to,
            cc: self.cc,
            bcc: self.bcc,
            reply_to: self.reply_to,
            subject: mailable.subject(),
            html,
            text,
            attachments: mailable.attachments(),
        };

        transport.send(&msg).await
    }
}

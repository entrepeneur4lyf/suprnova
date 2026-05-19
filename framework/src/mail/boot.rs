//! Mail boot wiring — reads `MAIL_DRIVER` env and binds the matching
//! transport via [`Mail::set_transport`]. Defaults to the `log` driver when
//! `MAIL_DRIVER` is unset or names an unknown driver.

use crate::error::FrameworkError;
use crate::lock;
use crate::mail::log::LogMailTransport;
use crate::mail::mailgun::MailgunMailTransport;
use crate::mail::memory::InMemoryMailTransport;
use crate::mail::postmark::PostmarkMailTransport;
use crate::mail::resend::ResendMailTransport;
use crate::mail::sendgrid::SendGridMailTransport;
use crate::mail::ses::SesMailTransport;
use crate::mail::smtp::SmtpMailTransport;
use crate::mail::Mail;
use std::sync::{Arc, RwLock};

// `RwLock<Option<...>>` (not `OnceLock`) so successive bootstrap calls can
// install a fresh capture handle when the driver is toggled back to memory.
// `OnceLock::set` only succeeds once per process — that would silently leak
// the stale Arc from the FIRST memory bootstrap into every subsequent one,
// confusing tests that switch drivers between cases.
static MEMORY_CAPTURE: RwLock<Option<Arc<InMemoryMailTransport>>> = RwLock::new(None);

/// If the memory driver was selected via env on the most recent call to
/// [`bootstrap_from_env`], return the shared [`InMemoryMailTransport`] so
/// tests can inspect captured messages. Returns `None` after a switch to
/// any non-memory driver.
pub fn captured_in_memory() -> Option<Arc<InMemoryMailTransport>> {
    lock::read(&MEMORY_CAPTURE)
        .expect("memory capture lock poisoned")
        .clone()
}

fn set_memory_capture(t: Arc<InMemoryMailTransport>) {
    *lock::write(&MEMORY_CAPTURE)
        .expect("memory capture lock poisoned") = Some(t);
}

fn clear_memory_capture() {
    *lock::write(&MEMORY_CAPTURE)
        .expect("memory capture lock poisoned") = None;
}

/// Read `MAIL_DRIVER` and bind the matching transport globally. Defaults to
/// the `log` driver when the env var is unset.
///
/// Supported values: `log` | `memory` | `smtp` | `postmark` | `ses` |
/// `sendgrid` | `mailgun` | `resend`. Unknown values warn and fall back to
/// `log`.
///
/// HTTP-backed providers (postmark, ses, sendgrid, mailgun, resend) also
/// honor a corresponding `MAIL_<PROVIDER>_ENDPOINT` override for pointing
/// at a regional URL or a mock server.
///
/// Synchronous: every supported transport's constructor is sync today.
/// If a future transport adds async initialization (e.g. a connection
/// pre-warm), flip this back to `async` and update the call sites — only
/// `Server::serve` and the boot tests need to add `.await`.
pub fn bootstrap_from_env() -> Result<(), FrameworkError> {
    // Release any previous in-memory capture handle BEFORE matching, so
    // toggling `memory → postmark → memory` always exposes a fresh buffer
    // for the subsequent memory bootstrap.
    clear_memory_capture();

    let driver = std::env::var("MAIL_DRIVER").unwrap_or_else(|_| "log".into());
    match driver.as_str() {
        "log" => {
            Mail::set_transport(Arc::new(LogMailTransport::new()));
        }
        "memory" => {
            let t = Arc::new(InMemoryMailTransport::new());
            set_memory_capture(t.clone());
            Mail::set_transport(t);
        }
        "smtp" => {
            let host =
                std::env::var("MAIL_SMTP_HOST").unwrap_or_else(|_| "127.0.0.1".into());
            let port: u16 = std::env::var("MAIL_SMTP_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(587);
            let user = std::env::var("MAIL_SMTP_USER").ok();
            let pass = std::env::var("MAIL_SMTP_PASS").ok();
            // Note: partial creds (only USER, only PASS) intentionally fall
            // through to unencrypted local-dev mode. Set BOTH to authenticate.
            let transport = match (user, pass) {
                (Some(u), Some(p)) => SmtpMailTransport::starttls(&host, port, &u, &p)?,
                _ => SmtpMailTransport::unencrypted(&host, port)?,
            };
            Mail::set_transport(Arc::new(transport));
        }
        "postmark" => {
            let token = std::env::var("MAIL_POSTMARK_TOKEN").map_err(|_| {
                FrameworkError::internal(
                    "MAIL_POSTMARK_TOKEN is required for MAIL_DRIVER=postmark",
                )
            })?;
            let transport = match std::env::var("MAIL_POSTMARK_ENDPOINT") {
                Ok(ep) => PostmarkMailTransport::with_endpoint(token, ep),
                Err(_) => PostmarkMailTransport::new(token),
            };
            Mail::set_transport(Arc::new(transport));
        }
        "ses" => {
            let key = std::env::var("MAIL_SES_ACCESS_KEY").map_err(|_| {
                FrameworkError::internal(
                    "MAIL_SES_ACCESS_KEY is required for MAIL_DRIVER=ses",
                )
            })?;
            let secret = std::env::var("MAIL_SES_SECRET_KEY").map_err(|_| {
                FrameworkError::internal(
                    "MAIL_SES_SECRET_KEY is required for MAIL_DRIVER=ses",
                )
            })?;
            let region =
                std::env::var("MAIL_SES_REGION").unwrap_or_else(|_| "us-east-1".into());
            let transport = match std::env::var("MAIL_SES_ENDPOINT") {
                Ok(ep) => SesMailTransport::with_endpoint(key, secret, region, ep),
                Err(_) => SesMailTransport::new(key, secret, region),
            };
            Mail::set_transport(Arc::new(transport));
        }
        "sendgrid" => {
            let key = std::env::var("MAIL_SENDGRID_API_KEY").map_err(|_| {
                FrameworkError::internal(
                    "MAIL_SENDGRID_API_KEY is required for MAIL_DRIVER=sendgrid",
                )
            })?;
            let transport = match std::env::var("MAIL_SENDGRID_ENDPOINT") {
                Ok(ep) => SendGridMailTransport::with_endpoint(key, ep),
                Err(_) => SendGridMailTransport::new(key),
            };
            Mail::set_transport(Arc::new(transport));
        }
        "mailgun" => {
            let key = std::env::var("MAIL_MAILGUN_API_KEY").map_err(|_| {
                FrameworkError::internal(
                    "MAIL_MAILGUN_API_KEY is required for MAIL_DRIVER=mailgun",
                )
            })?;
            let domain = std::env::var("MAIL_MAILGUN_DOMAIN").map_err(|_| {
                FrameworkError::internal(
                    "MAIL_MAILGUN_DOMAIN is required for MAIL_DRIVER=mailgun",
                )
            })?;
            let transport = match std::env::var("MAIL_MAILGUN_ENDPOINT") {
                Ok(ep) => MailgunMailTransport::with_endpoint(key, domain, ep),
                Err(_) => MailgunMailTransport::new(key, domain),
            };
            Mail::set_transport(Arc::new(transport));
        }
        "resend" => {
            let key = std::env::var("MAIL_RESEND_API_KEY").map_err(|_| {
                FrameworkError::internal(
                    "MAIL_RESEND_API_KEY is required for MAIL_DRIVER=resend",
                )
            })?;
            let transport = match std::env::var("MAIL_RESEND_ENDPOINT") {
                Ok(ep) => ResendMailTransport::with_endpoint(key, ep),
                Err(_) => ResendMailTransport::new(key),
            };
            Mail::set_transport(Arc::new(transport));
        }
        other => {
            tracing::warn!(driver = %other, "unknown MAIL_DRIVER, falling back to log");
            Mail::set_transport(Arc::new(LogMailTransport::new()));
        }
    }
    Ok(())
}

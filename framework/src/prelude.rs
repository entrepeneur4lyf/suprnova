//! Curated re-exports for ergonomic consumer imports.
//!
//! `use suprnova::prelude::*;` brings the framework facades, the common
//! HTTP request/response types, the error type, and the trait surfaces
//! consumers implement most often (`Mailable`, `Notification`,
//! `Notifiable`, `NotificationMailable`, `Job`) into scope in one
//! statement.
//!
//! Specialized types — transports, paginators, workflow primitives,
//! validators, inertia plumbing, telemetry handles, etc. — stay
//! reachable at `suprnova::<Symbol>` and must be imported explicitly.
//! This keeps the prelude small enough to scan and avoids hiding the
//! intentional decision of pulling in a specialized type.
//!
//! Every prelude symbol is also reachable at the crate root, so this
//! module is purely additive — consumers who prefer explicit per-symbol
//! imports do not need to change a thing.
//!
//! ```ignore
//! use suprnova::prelude::*;
//!
//! #[derive(serde::Serialize, serde::Deserialize)]
//! struct Welcome { name: String }
//!
//! #[async_trait]
//! impl Mailable for Welcome {
//!     fn mailable_name() -> &'static str { "Welcome" }
//!     fn subject(&self) -> String { format!("Welcome, {}", self.name) }
//!     fn text_template_source(&self) -> Option<String> {
//!         Some("Hi {{ name }}".into())
//!     }
//! }
//!
//! async fn greet(name: String) -> Result<(), FrameworkError> {
//!     Mail::to("alice@example.org").send(Welcome { name }).await
//! }
//! ```

// async_trait is required to `impl` any of the async traits below
// (Mailable, Job, MailTransport, Channel, MultipartRequestHooks, ...).
pub use crate::async_trait;

// Error types — every `Result` in Suprnova-flavored code touches one of these.
pub use crate::error::{AppError, FrameworkError, HttpError};

// HTTP surface — the bread and butter of controllers.
pub use crate::http::{HttpResponse, Redirect, Request, Response};

// Container — App is the typical DI handle consumers reach for.
pub use crate::container::App;

// Facades — call sites for the framework's named services.
pub use crate::auth::Auth;
pub use crate::bus::Bus;
pub use crate::cache::Cache;
pub use crate::http_client::Http;
pub use crate::queue::{Job, Queue};

// Mail — facade, the trait consumers implement, the value types, and the
// test fake guard.
pub use crate::mail::{Address, Attachment, Mail, MailFake, Mailable};

// Notifications — facade plus the three traits consumers implement
// (`Notifiable`, `Notification`, `NotificationMailable`) and the
// rendering value used by the mail channel.
pub use crate::notifications::channels::mail::{
    MailRendering, NotificationMailable, register_mail_renderer,
};
pub use crate::notifications::{Notifiable, Notification, Notify};

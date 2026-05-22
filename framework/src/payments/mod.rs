//! Provider-neutral payments surface.
//!
//! See `docs/core/payments.md` for the user-facing guide.

pub mod dto;
pub mod error;
pub mod money;

pub use dto::*;
pub use error::{PaymentError, PaymentResult};
pub use money::{Currency, Money};

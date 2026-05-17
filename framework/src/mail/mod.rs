//! Mail subsystem.

pub mod address;
pub mod mailable;

pub use address::{Address, Attachment};
pub use mailable::Mailable;

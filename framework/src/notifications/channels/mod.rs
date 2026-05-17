//! Notification channels.
//!
//! Concrete channel implementations: [`mail::MailChannel`] dispatches
//! through the bound mail transport; [`database::DatabaseChannel`]
//! persists each delivery as a row in the `notifications` table.
//! WebPush lands in Task 18.

pub mod database;
pub mod mail;

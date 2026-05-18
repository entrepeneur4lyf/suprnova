//! Remember-me persistent cookies — re-export of the framework's
//! already-shipped implementation at [`crate::auth::remember`].
//!
//! # Why a re-export instead of a fresh implementation
//!
//! `framework/src/auth/remember.rs` shipped a stronger design than the
//! Phase 11 plan's original "encrypted cookie bytes" sketch:
//!
//! - **DB-row + bcrypt hash** — each issued token has a row in
//!   `remember_tokens` storing only the bcrypt hash, never the
//!   plaintext. A database dump cannot yield re-authenticating
//!   credentials. (Same standard as password hashes.)
//! - **Single-use rotation** — successful verification DELETEs the
//!   matched row and issues a fresh one. A captured cookie cannot be
//!   re-used; if attacker and victim race to use it, the loser sees
//!   the row gone and fails to authenticate.
//! - **Revocation** — `revoke_all_for_user` wipes every row for a
//!   user in one DELETE. `Auth::logout` chains this so a real logout
//!   actually clears persistent state.
//! - **Pruning** — `prune_expired` cleans up expired rows on a
//!   schedule.
//!
//! Listing the module here gives consumers a single, cohesive
//! `auth_flows::*` namespace for every auth feature — verification,
//! reset, brute-force, 2FA, remember-me — without two import paths.

pub use crate::auth::remember::*;

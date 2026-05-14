//! Application-level encryption — `Crypt` facade, `EncryptionKey`,
//! and the AES-256-GCM AEAD layer. The `Crypt` facade itself is added
//! in a follow-up commit; this stub exposes `EncryptionKey` first.

pub mod key;
pub(crate) mod aead;

pub use key::EncryptionKey;

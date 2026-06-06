//! Single-use ceremony tokens for OAuth + Passkey auth flows.
//!
//! Hardens OAuth state and Passkey challenge consumption against
//! concurrent replay.
//!
//! The previous design stored ceremony state in the session and
//! relied on session.get + session.forget being a single atomic
//! consume. Session driver implementations load the session before
//! the handler runs and write it after, with no compare-and-swap,
//! so two concurrent requests with the same session cookie can
//! both pass the get-and-forget and both consume the same ceremony.
//!
//! This module externalises the single-use authority to a dedicated
//! table indexed on a UNIQUE `selector`. Atomic consumption uses
//! conditional DELETE keyed on `(id, selector)`: only one of N
//! concurrent racers gets `rows_affected == 1`, the rest get 0 and
//! bail.

use chrono::Duration;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::database::DB;
use crate::error::FrameworkError;

/// Issue a ceremony token. `selector` MUST be globally unique
/// (UUID v4 is the canonical choice). `payload` is serialised to
/// JSON and stored as text — opaque to this table.
///
/// `kind` is a free-form discriminator used by [`consume`] to ensure
/// a passkey-registration selector can't be replayed against an OAuth
/// consumer (defence-in-depth against API misuse).
///
/// Returns `Ok(())` on success. A UNIQUE constraint conflict on
/// `selector` propagates as a database error rather than being
/// swallowed.
pub async fn issue<P: Serialize>(
    selector: &str,
    kind: &str,
    payload: &P,
    ttl_minutes: i64,
) -> Result<(), FrameworkError> {
    let payload_json = serde_json::to_string(payload)
        .map_err(|e| FrameworkError::internal(format!("ceremony: serialize payload: {e}")))?;
    let now = chrono::Utc::now();
    let expires_at = now + Duration::minutes(ttl_minutes);
    let conn = DB::connection()?;
    let model = entity::ActiveModel {
        selector: Set(selector.to_string()),
        kind: Set(kind.to_string()),
        payload: Set(payload_json),
        expires_at: Set(expires_at.naive_utc()),
        created_at: Set(now.naive_utc()),
        ..Default::default()
    };
    entity::Entity::insert(model)
        .exec(conn.inner())
        .await
        .map_err(|e| FrameworkError::database(format!("ceremony issue: {e}")))?;
    Ok(())
}

/// Atomically consume a ceremony token. Returns `Some(payload)` if:
/// - a row with this `selector` AND `kind` AND `expires_at > now` exists, AND
/// - the conditional DELETE affected exactly 1 row.
///
/// Returns `None` for any miss (no row, wrong kind, expired, or lost
/// a concurrency race). Callers MUST NOT retry on `None` — it
/// indicates "ceremony already consumed or invalid", not a transient
/// failure.
pub async fn consume<P: DeserializeOwned>(
    selector: &str,
    kind: &str,
) -> Result<Option<P>, FrameworkError> {
    let conn = DB::connection()?;
    let now = chrono::Utc::now().naive_utc();

    // O(1) indexed lookup on the UNIQUE selector.
    let row = entity::Entity::find()
        .filter(entity::Column::Selector.eq(selector))
        .filter(entity::Column::Kind.eq(kind))
        .filter(entity::Column::ExpiresAt.gt(now))
        .one(conn.inner())
        .await
        .map_err(|e| FrameworkError::database(format!("ceremony lookup: {e}")))?;

    let row = match row {
        Some(r) => r,
        None => return Ok(None),
    };

    // Atomic conditional DELETE: only one of N concurrent consumers
    // sees rows_affected == 1. Race-losers see 0 and bail without
    // returning the payload.
    let delete_result = entity::Entity::delete_many()
        .filter(entity::Column::Id.eq(row.id))
        .filter(entity::Column::Selector.eq(&row.selector))
        .exec(conn.inner())
        .await
        .map_err(|e| FrameworkError::database(format!("ceremony delete: {e}")))?;

    if delete_result.rows_affected != 1 {
        return Ok(None);
    }

    let payload = serde_json::from_str(&row.payload)
        .map_err(|e| FrameworkError::internal(format!("ceremony: deserialize payload: {e}")))?;
    Ok(Some(payload))
}

/// Delete all rows whose `expires_at` is in the past. Returns the
/// number of rows removed.
///
/// Wire to a scheduled task (see `framework/src/schedule/`) so the
/// table does not accumulate dead rows.
pub async fn prune_expired() -> Result<u64, FrameworkError> {
    let conn = DB::connection()?;
    let now = chrono::Utc::now().naive_utc();
    let result = entity::Entity::delete_many()
        .filter(entity::Column::ExpiresAt.lte(now))
        .exec(conn.inner())
        .await
        .map_err(|e| FrameworkError::database(format!("ceremony prune: {e}")))?;
    Ok(result.rows_affected)
}

/// Discriminators used by the framework's built-in ceremony flows.
///
/// Consumers SHOULD use these constants rather than free strings so
/// that a typo at the call site is a compile error.
pub mod kind {
    /// OAuth state+PKCE ceremony.
    pub const OAUTH: &str = "oauth";
    /// Passkey registration ceremony.
    pub const PASSKEY_REGISTER: &str = "passkey_register";
    /// Passkey authentication ceremony.
    pub const PASSKEY_AUTHENTICATE: &str = "passkey_authenticate";
}

/// SeaORM entity for the `auth_ceremony_tokens` table.
pub mod entity {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "auth_ceremony_tokens")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(unique)]
        pub selector: String,
        pub kind: String,
        #[sea_orm(column_type = "Text")]
        pub payload: String,
        pub expires_at: chrono::NaiveDateTime,
        pub created_at: chrono::NaiveDateTime,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

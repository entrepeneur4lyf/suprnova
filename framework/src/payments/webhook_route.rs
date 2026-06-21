//! Webhook ingress route for the payments subsystem.
//!
//! Registers `POST /webhooks/payments/{provider}` on a Suprnova `Router`.
//!
//! Production-grade contract:
//!
//! 1. **Idempotency, retry-aware.** A duplicate of an event that was
//!    *successfully* processed (`processed_at IS NOT NULL`) returns 200
//!    `duplicate` immediately. A duplicate of an event that previously
//!    *failed* (`processed_at IS NULL`, `process_error` set) re-attempts
//!    hydration — the provider's retry is the recovery mechanism.
//! 2. **Atomic, serialized hydration.** All mirror-table writes for one
//!    event happen inside a single DB transaction along with
//!    `mark_processed`. Partial state is not observable: either the mirror
//!    catches up AND `processed_at` is set, or both are rolled back
//!    together. Concurrent retries of the *same* unprocessed event are
//!    serialized by taking a `FOR UPDATE` lock on the audit row before
//!    re-checking `processed_at` — the second arrival blocks until the
//!    first commits, then sees `processed_at` set and short-circuits
//!    instead of double-applying. Mirror-table `UNIQUE` violations from a
//!    racing apply are treated as benign already-applied (200, not 503).
//! 3. **5xx on failure.** Hydration errors return 503 so the provider
//!    retries with backoff. The audit row's `process_error` is updated
//!    outside the rolled-back transaction so operators can see the
//!    failure across retries.
//! 4. **Auditability.** Every accepted event lands in
//!    `payments_webhook_events` before hydration begins — even failures
//!    leave an audit trail.

use crate::Request;
use crate::http::{HttpResponse, Response, text};
use crate::payments::entities::{
    customer, subscription, subscription_item, transaction, webhook_event,
};
use crate::payments::{
    CustomerSnapshot, NeutralEventKind, PaymentError, PaymentProvider, PaymentProviderRegistry,
    PaymentSnapshot, SubscriptionResult, SubscriptionStatus, WebhookContext, WebhookEvent,
};
use crate::routing::Router;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QuerySelect, Set, TransactionTrait,
};
use std::sync::Arc;

fn err_response(status: u16, body: &str) -> Response {
    Ok(HttpResponse::text(body).status(status))
}

/// A `UNIQUE` violation surfaces differently per backend (SQLite says
/// `UNIQUE constraint failed`, Postgres says `duplicate key value violates
/// unique constraint`, MySQL says `Duplicate entry`). This catches all
/// three by substring — the alternative is per-driver SQLSTATE matching,
/// which SeaORM abstracts away.
fn is_unique_violation(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("unique") || m.contains("duplicate")
}

async fn handle_webhook_inner(
    db: &DatabaseConnection,
    provider_name: &str,
    remote_addr_str: Option<String>,
    headers: http::HeaderMap,
    body: bytes::Bytes,
) -> Response {
    // 1. Resolve provider
    let provider = match PaymentProviderRegistry::get(provider_name) {
        Some(p) => p,
        None => {
            tracing::warn!(provider = %provider_name, "webhook received for unregistered provider");
            return err_response(404, "unknown provider");
        }
    };

    // 2. Parse remote_addr (best-effort; never blocks the request)
    let remote_addr = remote_addr_str
        .as_deref()
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse().ok());

    // 3. Verify signature
    let ctx = WebhookContext {
        body: &body,
        headers: &headers,
        remote_addr,
    };
    if let Err(e) = provider.verify(&ctx) {
        tracing::warn!(provider = %provider_name, error = %e, "webhook signature verification failed");
        return err_response(401, "signature");
    }

    // 4. Parse event
    let event: WebhookEvent = match provider.parse_event(&body) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(provider = %provider_name, error = %e, "webhook parse failed");
            return err_response(400, "malformed");
        }
    };

    // 5. Idempotency, retry-aware. Successfully-processed events short-circuit
    //    here; previously-failed events fall through to retry hydration.
    let existing = match webhook_event::Entity::find()
        .filter(webhook_event::Column::Provider.eq(&event.provider))
        .filter(webhook_event::Column::ProviderEventId.eq(&event.provider_event_id))
        .one(db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "db error checking webhook idempotency");
            return err_response(500, "db");
        }
    };
    if let Some(row) = &existing
        && row.processed_at.is_some()
    {
        tracing::debug!(
            provider = %provider_name,
            event_id = %event.provider_event_id,
            "duplicate webhook — already processed"
        );
        return err_response(200, "duplicate");
    }

    // 6. Ensure audit row exists. INSERT race on (provider, event_id) UNIQUE
    //    is treated as a concurrent duplicate — return 200 so the racing
    //    caller can retry against the now-existing row if it later fails.
    if existing.is_none() {
        let neutral_str = event.neutral.and_then(|k| {
            serde_json::to_value(k)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
        });
        let record = webhook_event::ActiveModel {
            provider: Set(event.provider.clone()),
            provider_event_id: Set(event.provider_event_id.clone()),
            provider_event_type: Set(event.provider_event_type.clone()),
            neutral_event_kind: Set(neutral_str),
            payload: Set(event.raw_payload.clone()),
            received_at: Set(Utc::now().to_rfc3339()),
            processed_at: Set(None),
            process_error: Set(None),
            ..Default::default()
        };
        if let Err(e) = record.insert(db).await {
            let msg = format!("{e}");
            if is_unique_violation(&msg) {
                return err_response(200, "duplicate-race");
            }
            tracing::error!(error = %e, "failed to persist webhook event");
            return err_response(500, "persist");
        }
    } else {
        // Retry: clear stale process_error from a previous failed attempt so
        // the audit row reflects the current attempt's outcome.
        if let Some(row) = existing {
            let mut am: webhook_event::ActiveModel = row.into();
            am.process_error = Set(None);
            if let Err(e) = am.update(db).await {
                tracing::error!(error = %e, "failed to clear stale process_error before retry");
                // Continue — the retry can still succeed and overwrite this.
            }
        }
    }

    // 7. Hydrate inside a transaction. On failure, audit row + process_error
    //    survive the rollback; the response is 503 so the provider retries.
    //
    //    Provider HTTP calls (currently: subscription `provider.get`) run
    //    BEFORE the transaction opens so the DB connection isn't pinned for
    //    the duration of an external round-trip — a webhook burst against a
    //    degraded provider would otherwise exhaust the pool. The pre-fetched
    //    snapshot is passed through to `process_webhook`, which then does
    //    only pure-DB work inside the transaction.
    //
    //    Concurrent retries of the same unprocessed event are serialized
    //    inside the transaction by a `FOR UPDATE` lock on the audit row: the
    //    loser blocks until the winner commits, then sees `processed_at` set
    //    and reports `AlreadyProcessed` instead of double-applying.
    match try_hydrate(db, provider.as_ref(), &event).await {
        Ok(HydrationOutcome::Processed) => text("ok"),
        Ok(HydrationOutcome::AlreadyProcessed) => {
            tracing::debug!(
                provider = %provider_name,
                event_id = %event.provider_event_id,
                "concurrent retry observed event already processed under row lock"
            );
            err_response(200, "duplicate")
        }
        Err(e) => {
            // A mirror-table `UNIQUE` violation means a racing apply already
            // landed the row this attempt was about to write. The state the
            // provider asked for exists; surfacing 503 would just provoke a
            // pointless retry. Treat it as already-applied: 200.
            if is_unique_violation(&format!("{e}")) {
                tracing::debug!(
                    provider = %provider_name,
                    event_id = %event.provider_event_id,
                    "mirror upsert hit a unique violation from a concurrent apply; treating as already-applied"
                );
                return err_response(200, "duplicate-applied");
            }
            tracing::error!(
                provider = %provider_name,
                event_id = %event.provider_event_id,
                error = %e,
                "webhook hydration failed"
            );
            let _ = mark_failed(db, &event, &format!("{e}")).await;
            err_response(503, "hydration-failed")
        }
    }
}

/// Result of a single hydration attempt that committed without error.
enum HydrationOutcome {
    /// This attempt performed the mirror writes and set `processed_at`.
    Processed,
    /// A concurrent attempt had already set `processed_at` by the time this
    /// one acquired the audit-row lock — no work was done and the caller
    /// should report a duplicate, not re-apply.
    AlreadyProcessed,
}

/// Pre-fetch any provider-side state the hydration path needs BEFORE opening
/// the DB transaction. Today the only network call is
/// `Subscription::get(sub_id)` for the subscription event family — payment
/// and customer events derive their state entirely from the webhook payload.
/// Returning `Err` here short-circuits to the 503/mark_failed path the same
/// way an in-transaction error would.
async fn prefetch_provider_state(
    provider: &dyn PaymentProvider,
    event: &WebhookEvent,
) -> Result<Option<SubscriptionResult>, PaymentError> {
    let Some(neutral) = event.neutral else {
        return Ok(None);
    };

    match neutral {
        NeutralEventKind::SubscriptionCreated
        | NeutralEventKind::SubscriptionUpdated
        | NeutralEventKind::SubscriptionCanceled => {
            let ids = provider.extract_payload_ids(event);
            let sub_id = ids.subscription_id.as_deref().ok_or_else(|| {
                PaymentError::Validation(format!(
                    "subscription webhook missing subscription_id (provider_event_type={})",
                    event.provider_event_type
                ))
            })?;
            let result = provider.get(sub_id).await?;
            Ok(Some(result))
        }
        _ => Ok(None),
    }
}

/// Run `process_webhook` and `mark_processed` inside one DB transaction.
/// On error, rolls back so partial mirror state isn't observable.
///
/// External provider HTTP calls are hoisted to `prefetch_provider_state`
/// above so the transaction scope contains only DB work.
///
/// Concurrent retries of the same event are serialized here: the first thing
/// the transaction does is take a `FOR UPDATE` lock on the audit row, then
/// re-read `processed_at`. A second retry that arrives while the first is
/// still committing blocks on that lock; once the first commits, the second
/// observes `processed_at` set and returns [`HydrationOutcome::AlreadyProcessed`]
/// without re-applying the mirror writes. On backends without row-level locks
/// (SQLite uses file-level transaction locking) `lock_for_update` is a no-op,
/// but the re-read under the open transaction plus the mirror-table `UNIQUE`
/// constraints still prevent a double-apply from being observable.
async fn try_hydrate(
    db: &DatabaseConnection,
    provider: &dyn PaymentProvider,
    event: &WebhookEvent,
) -> Result<HydrationOutcome, PaymentError> {
    let subscription_snapshot = prefetch_provider_state(provider, event).await?;

    let txn = db
        .begin()
        .await
        .map_err(|e| PaymentError::Internal(format!("begin tx: {e}")))?;

    // Serialize concurrent retries: lock the audit row, then re-check whether
    // a racing attempt already finished. `lock_exclusive` emits
    // `SELECT … FOR UPDATE` on Postgres/MySQL/MariaDB and is a documented
    // no-op on SQLite, which serializes writers at the file level instead.
    let locked = match webhook_event::Entity::find()
        .filter(webhook_event::Column::Provider.eq(&event.provider))
        .filter(webhook_event::Column::ProviderEventId.eq(&event.provider_event_id))
        .lock_exclusive()
        .one(&txn)
        .await
    {
        Ok(row) => row,
        Err(e) => {
            let _ = txn.rollback().await;
            return Err(PaymentError::Internal(format!("lock audit row: {e}")));
        }
    };
    if locked.as_ref().is_some_and(|r| r.processed_at.is_some()) {
        // A concurrent retry committed while we waited on the lock. Roll back
        // — we have nothing to add — and report the duplicate.
        let _ = txn.rollback().await;
        return Ok(HydrationOutcome::AlreadyProcessed);
    }

    match process_webhook(&txn, provider, event, subscription_snapshot.as_ref()).await {
        Ok(()) => match mark_processed(&txn, event).await {
            Ok(()) => txn
                .commit()
                .await
                .map(|()| HydrationOutcome::Processed)
                .map_err(|e| PaymentError::Internal(format!("commit: {e}"))),
            Err(e) => {
                let _ = txn.rollback().await;
                Err(e)
            }
        },
        Err(e) => {
            let _ = txn.rollback().await;
            Err(e)
        }
    }
}

/// Dispatch a parsed [`WebhookEvent`] to the mirror-table hydration paths.
///
/// Unmapped events (no `neutral`) are no-ops — the audit row is the only
/// effect. Mapped events extract IDs / snapshots from the provider, look up
/// or fetch the canonical entity state, then upsert the relevant mirror rows.
/// Missing IDs in events whose neutral kind requires one are treated as
/// validation errors — silent success would leave the mirror stale without
/// operator visibility.
///
/// `subscription_snapshot` is the pre-fetched [`SubscriptionResult`] from
/// `prefetch_provider_state`; the subscription branch consumes it directly
/// rather than re-issuing the network call from inside the transaction.
async fn process_webhook<C>(
    db: &C,
    provider: &dyn PaymentProvider,
    event: &WebhookEvent,
    subscription_snapshot: Option<&SubscriptionResult>,
) -> Result<(), PaymentError>
where
    C: ConnectionTrait + Send + Sync,
{
    let Some(neutral) = event.neutral else {
        return Ok(());
    };

    let ids = provider.extract_payload_ids(event);

    match neutral {
        NeutralEventKind::SubscriptionCreated
        | NeutralEventKind::SubscriptionUpdated
        | NeutralEventKind::SubscriptionCanceled => {
            let sub_id = ids.subscription_id.as_deref().ok_or_else(|| {
                PaymentError::Validation(format!(
                    "subscription webhook missing subscription_id (provider_event_type={})",
                    event.provider_event_type
                ))
            })?;
            let result = subscription_snapshot.ok_or_else(|| {
                PaymentError::Internal(
                    "subscription webhook reached process_webhook without a pre-fetched snapshot \
                     (prefetch_provider_state contract violated)"
                        .into(),
                )
            })?;
            upsert_subscription(db, &event.provider, result, neutral).await?;
            sync_subscription_items(db, &event.provider, sub_id, result).await?;
        }
        NeutralEventKind::CustomerCreated | NeutralEventKind::CustomerUpdated => {
            let cust_id = ids.customer_id.as_deref().ok_or_else(|| {
                PaymentError::Validation(format!(
                    "customer webhook missing customer_id (provider_event_type={})",
                    event.provider_event_type
                ))
            })?;
            let snapshot = provider.extract_customer_snapshot(event);
            update_customer_mirror(db, &event.provider, cust_id, snapshot.as_ref()).await?;
        }
        NeutralEventKind::PaymentSucceeded
        | NeutralEventKind::PaymentFailed
        | NeutralEventKind::PaymentRefunded
        | NeutralEventKind::PaymentDisputed
        | NeutralEventKind::InvoicePaid
        | NeutralEventKind::InvoiceFailed => {
            let Some(snapshot) = provider.extract_payment_snapshot(event) else {
                // Providers may legitimately omit snapshots for events they
                // can't translate (e.g., adjustment events with no charge id).
                // Audit-only is correct here.
                tracing::debug!(
                    event_id = %event.provider_event_id,
                    provider_event_type = %event.provider_event_type,
                    "payment webhook produced no snapshot; audit row only"
                );
                return Ok(());
            };
            upsert_transaction(db, &event.provider, &snapshot).await?;
        }
    }
    Ok(())
}

fn subscription_status_to_str(s: SubscriptionStatus) -> &'static str {
    match s {
        SubscriptionStatus::Trialing => "trialing",
        SubscriptionStatus::Active => "active",
        SubscriptionStatus::PastDue => "past_due",
        SubscriptionStatus::Canceled => "canceled",
        SubscriptionStatus::Incomplete => "incomplete",
        SubscriptionStatus::Paused => "paused",
    }
}

async fn upsert_subscription<C>(
    db: &C,
    provider: &str,
    result: &SubscriptionResult,
    neutral: NeutralEventKind,
) -> Result<(), PaymentError>
where
    C: ConnectionTrait + Send + Sync,
{
    let existing = subscription::Entity::find()
        .filter(subscription::Column::Provider.eq(provider))
        .filter(subscription::Column::ProviderSubscriptionId.eq(&result.provider_subscription_id))
        .one(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?;

    let status_str = subscription_status_to_str(result.status);
    let mark_canceled = matches!(neutral, NeutralEventKind::SubscriptionCanceled)
        || matches!(result.status, SubscriptionStatus::Canceled);

    let now = Utc::now().to_rfc3339();

    match existing {
        Some(model) => {
            let was_canceled = model.canceled_at.is_some();
            let mut am: subscription::ActiveModel = model.into();
            am.provider_customer_id = Set(result.provider_customer_id.clone());
            am.status = Set(status_str.to_string());
            am.current_period_start = Set(result.current_period_start.to_rfc3339());
            am.current_period_end = Set(result.current_period_end.to_rfc3339());
            am.cancel_at_period_end = Set(result.cancel_at_period_end);
            if mark_canceled && !was_canceled {
                am.canceled_at = Set(Some(now.clone()));
            }
            am.provider_metadata = Set(result.provider_metadata.clone());
            am.updated_at = Set(now);
            am.update(db)
                .await
                .map_err(|e| PaymentError::Internal(format!("{e}")))?;
        }
        None => {
            let canceled_at = if mark_canceled {
                Some(now.clone())
            } else {
                None
            };
            let am = subscription::ActiveModel {
                provider: Set(provider.to_string()),
                provider_subscription_id: Set(result.provider_subscription_id.clone()),
                provider_customer_id: Set(result.provider_customer_id.clone()),
                status: Set(status_str.to_string()),
                current_period_start: Set(result.current_period_start.to_rfc3339()),
                current_period_end: Set(result.current_period_end.to_rfc3339()),
                cancel_at_period_end: Set(result.cancel_at_period_end),
                canceled_at: Set(canceled_at),
                provider_metadata: Set(result.provider_metadata.clone()),
                created_at: Set(now.clone()),
                updated_at: Set(now),
                ..Default::default()
            };
            am.insert(db)
                .await
                .map_err(|e| PaymentError::Internal(format!("{e}")))?;
        }
    }
    Ok(())
}

async fn sync_subscription_items<C>(
    db: &C,
    provider: &str,
    provider_subscription_id: &str,
    result: &SubscriptionResult,
) -> Result<(), PaymentError>
where
    C: ConnectionTrait + Send + Sync,
{
    let parent = subscription::Entity::find()
        .filter(subscription::Column::Provider.eq(provider))
        .filter(subscription::Column::ProviderSubscriptionId.eq(provider_subscription_id))
        .one(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?
        .ok_or_else(|| {
            PaymentError::Internal(
                "parent subscription vanished between upsert and item sync".into(),
            )
        })?;
    let parent_id = parent.id;

    let existing_items = subscription_item::Entity::find()
        .filter(subscription_item::Column::SubscriptionId.eq(parent_id))
        .all(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?;

    let now = Utc::now().to_rfc3339();

    let mut keep: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in &result.items {
        keep.insert(item.provider_item_id.clone());
        let existing = existing_items
            .iter()
            .find(|row| row.provider_item_id == item.provider_item_id);
        let (unit_amount, unit_currency) = match item.unit_amount.as_ref() {
            Some(m) => (Some(m.minor_units()), Some(m.currency().code().to_string())),
            None => (None, None),
        };
        match existing {
            Some(model) => {
                let mut am: subscription_item::ActiveModel = model.clone().into();
                am.provider_price_id = Set(item.provider_price_id.clone());
                am.quantity = Set(item.quantity as i32);
                am.unit_amount_minor = Set(unit_amount);
                am.unit_currency = Set(unit_currency);
                am.updated_at = Set(now.clone());
                am.update(db)
                    .await
                    .map_err(|e| PaymentError::Internal(format!("{e}")))?;
            }
            None => {
                let am = subscription_item::ActiveModel {
                    subscription_id: Set(parent_id),
                    provider_item_id: Set(item.provider_item_id.clone()),
                    provider_price_id: Set(item.provider_price_id.clone()),
                    quantity: Set(item.quantity as i32),
                    unit_amount_minor: Set(unit_amount),
                    unit_currency: Set(unit_currency),
                    provider_metadata: Set(serde_json::Value::Null),
                    created_at: Set(now.clone()),
                    updated_at: Set(now.clone()),
                    ..Default::default()
                };
                am.insert(db)
                    .await
                    .map_err(|e| PaymentError::Internal(format!("{e}")))?;
            }
        }
    }

    // Remove items that no longer exist on the provider side. The
    // subscription is the source of truth — once an item is removed by the
    // upstream subscription update, the mirror should reflect that.
    for stale in existing_items
        .iter()
        .filter(|row| !keep.contains(&row.provider_item_id))
    {
        let am: subscription_item::ActiveModel = stale.clone().into();
        am.delete(db)
            .await
            .map_err(|e| PaymentError::Internal(format!("{e}")))?;
    }

    Ok(())
}

async fn upsert_transaction<C>(
    db: &C,
    provider: &str,
    snapshot: &PaymentSnapshot,
) -> Result<(), PaymentError>
where
    C: ConnectionTrait + Send + Sync,
{
    let existing = transaction::Entity::find()
        .filter(transaction::Column::Provider.eq(provider))
        .filter(transaction::Column::ProviderTransactionId.eq(&snapshot.provider_transaction_id))
        .one(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?;

    let now = Utc::now().to_rfc3339();

    match existing {
        Some(model) => {
            let mut am: transaction::ActiveModel = model.into();
            am.provider_customer_id = Set(snapshot.provider_customer_id.clone());
            am.provider_subscription_id = Set(snapshot.provider_subscription_id.clone());
            am.amount_total_minor = Set(snapshot.amount_total_minor);
            am.amount_tax_minor = Set(snapshot.amount_tax_minor);
            am.currency = Set(snapshot.currency.clone());
            am.status = Set(snapshot.status.clone());
            // Preserve original paid_at across refund/dispute events — the
            // original payment time is the canonical reference.
            if snapshot.paid_at.is_some() {
                am.paid_at = Set(snapshot.paid_at.map(|t| t.to_rfc3339()));
            }
            am.provider_metadata = Set(snapshot.provider_metadata.clone());
            am.updated_at = Set(now);
            am.update(db)
                .await
                .map_err(|e| PaymentError::Internal(format!("{e}")))?;
        }
        None => {
            let am = transaction::ActiveModel {
                provider: Set(provider.to_string()),
                provider_transaction_id: Set(snapshot.provider_transaction_id.clone()),
                provider_customer_id: Set(snapshot.provider_customer_id.clone()),
                provider_subscription_id: Set(snapshot.provider_subscription_id.clone()),
                amount_total_minor: Set(snapshot.amount_total_minor),
                amount_tax_minor: Set(snapshot.amount_tax_minor),
                currency: Set(snapshot.currency.clone()),
                status: Set(snapshot.status.clone()),
                paid_at: Set(snapshot.paid_at.map(|t| t.to_rfc3339())),
                provider_metadata: Set(snapshot.provider_metadata.clone()),
                created_at: Set(now.clone()),
                updated_at: Set(now),
                ..Default::default()
            };
            am.insert(db)
                .await
                .map_err(|e| PaymentError::Internal(format!("{e}")))?;
        }
    }
    Ok(())
}

/// Update an existing `payments_customers` mirror row from a provider-supplied
/// snapshot.
///
/// We deliberately do NOT insert when no row exists: `user_id` is `NOT NULL`
/// and only the app knows which user a provider-side customer maps to
/// (creation goes through [`crate::payments::CustomerStore::create_customer`]
/// plus an app-controlled DB write). Out-of-band customers (created in the
/// Stripe dashboard, say) are logged but not synthesized.
async fn update_customer_mirror<C>(
    db: &C,
    provider: &str,
    provider_customer_id: &str,
    snapshot: Option<&CustomerSnapshot>,
) -> Result<(), PaymentError>
where
    C: ConnectionTrait + Send + Sync,
{
    let existing = customer::Entity::find()
        .filter(customer::Column::Provider.eq(provider))
        .filter(customer::Column::ProviderCustomerId.eq(provider_customer_id))
        .one(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?;

    let Some(model) = existing else {
        tracing::info!(
            provider = %provider,
            provider_customer_id = %provider_customer_id,
            "customer webhook for unknown customer — no mirror row to update"
        );
        return Ok(());
    };

    let mut am: customer::ActiveModel = model.into();
    if let Some(snap) = snapshot {
        if let Some(email) = snap.email.as_ref() {
            am.email = Set(email.clone());
        }
        am.provider_metadata = Set(snap.provider_metadata.clone());
    }
    am.updated_at = Set(Utc::now().to_rfc3339());
    am.update(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?;
    Ok(())
}

async fn mark_processed<C>(db: &C, event: &WebhookEvent) -> Result<(), PaymentError>
where
    C: ConnectionTrait + Send + Sync,
{
    let model = webhook_event::Entity::find()
        .filter(webhook_event::Column::Provider.eq(&event.provider))
        .filter(webhook_event::Column::ProviderEventId.eq(&event.provider_event_id))
        .one(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?
        .ok_or_else(|| PaymentError::Internal("webhook event vanished after insert".into()))?;
    let mut am: webhook_event::ActiveModel = model.into();
    am.processed_at = Set(Some(Utc::now().to_rfc3339()));
    am.process_error = Set(None);
    am.update(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?;
    Ok(())
}

async fn mark_failed(
    db: &DatabaseConnection,
    event: &WebhookEvent,
    err_str: &str,
) -> Result<(), PaymentError> {
    let model = webhook_event::Entity::find()
        .filter(webhook_event::Column::Provider.eq(&event.provider))
        .filter(webhook_event::Column::ProviderEventId.eq(&event.provider_event_id))
        .one(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?
        .ok_or_else(|| PaymentError::Internal("webhook event vanished after insert".into()))?;
    let mut am: webhook_event::ActiveModel = model.into();
    am.process_error = Set(Some(err_str.into()));
    am.update(db)
        .await
        .map_err(|e| PaymentError::Internal(format!("{e}")))?;
    Ok(())
}

/// Mount the webhook ingress route onto an Axum-compatible Router.
///
/// ```ignore
/// use std::sync::Arc;
/// use suprnova::payments::webhook_routes;
///
/// let router = webhook_routes(db.clone());
/// // Merge into your app router.
/// ```
pub fn webhook_routes(db: Arc<DatabaseConnection>) -> Router {
    // The handler is a `Fn(Request) -> Future` closure that clones Arc per call.
    let db_for_handler = db;
    Router::new()
        .post("/webhooks/payments/{provider}", move |req: Request| {
            let db = db_for_handler.clone();
            async move {
                // Extract header map and client IP before consuming the request body.
                let headers = req.headers().clone();
                // Resolve the client IP through the trusted-proxy allowlist:
                // `X-Forwarded-For` / `X-Real-IP` are honoured only when the TCP
                // peer is a configured trusted proxy, otherwise this is the socket
                // peer addr. Reading the raw `X-Forwarded-For` header directly here
                // would let any client spoof `remote_addr` and defeat an adapter
                // that IP-allowlists via `WebhookContext::remote_addr`.
                let remote_addr_str = req.ip();
                let provider_name = match req.param("provider") {
                    Ok(s) => s.to_string(),
                    Err(_) => return err_response(404, "unknown provider"),
                };
                // Consume the request to read the body.
                let (_, body) = match req.body_bytes().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::error!(error = %e, "failed to read webhook body");
                        return err_response(400, "body");
                    }
                };
                handle_webhook_inner(&db, &provider_name, remote_addr_str, headers, body).await
            }
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payments::{MockPaymentProvider, PaymentProvider, PaymentProviderRegistry};
    use crate::testing::TestDatabase;
    use sea_orm_migration::MigratorTrait;

    struct PaymentsTestMigrator;

    #[async_trait::async_trait]
    impl MigratorTrait for PaymentsTestMigrator {
        fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
            crate::payments::migrations::migrations()
        }
    }

    fn register_mock(name: &'static str) -> Arc<MockPaymentProvider> {
        let mock = Arc::new(MockPaymentProvider::new());
        let as_trait: Arc<dyn PaymentProvider> = mock.clone();
        PaymentProviderRegistry::bind(name, as_trait);
        mock
    }

    /// Status of a `Response` regardless of which arm carries it.
    fn status_of(resp: &Response) -> u16 {
        match resp {
            Ok(r) | Err(r) => r.status_code(),
        }
    }

    fn payment_succeeded_body(event_id: &str, txn_id: &str) -> bytes::Bytes {
        bytes::Bytes::from(
            serde_json::json!({
                "id": event_id,
                "type": "payment.succeeded",
                "data": { "object": {
                    "id": txn_id,
                    "customer": "cus_concurrent",
                    "amount": 4242,
                    "currency": "USD"
                }}
            })
            .to_string(),
        )
    }

    /// `is_unique_violation` recognises the per-backend phrasings so a benign
    /// concurrent-apply collision is mapped to 200 rather than a spurious 503.
    #[test]
    fn unique_violation_matcher_covers_each_backend_phrasing() {
        assert!(is_unique_violation("UNIQUE constraint failed: t.col"));
        assert!(is_unique_violation(
            "duplicate key value violates unique constraint \"uniq_x\""
        ));
        assert!(is_unique_violation(
            "Duplicate entry 'a-b' for key 'uniq_x'"
        ));
        assert!(!is_unique_violation("connection refused"));
    }

    /// Two concurrent RETRY deliveries of the same unprocessed event must
    /// result in a single effective processing: both callers get 200, exactly
    /// one mirror transaction row exists, and the audit row ends up processed
    /// exactly once. The `FOR UPDATE` lock + `processed_at` re-check inside the
    /// transaction serializes the apply; the loser observes the winner's commit
    /// and reports a duplicate instead of double-applying.
    #[tokio::test]
    async fn concurrent_retries_of_unprocessed_event_process_once() {
        let provider_name: &'static str = "mock-webhook-concurrent-retry";
        let _mock = register_mock(provider_name);

        let db = TestDatabase::fresh::<PaymentsTestMigrator>()
            .await
            .expect("TestDatabase::fresh");
        let conn = Arc::new(db.conn().clone());

        let event_id = "evt_concurrent_retry";
        let txn_id = "txn_concurrent_retry";

        // Seed an existing, UNPROCESSED audit row — this is the retry precondition:
        // a prior delivery landed the audit row but hydration has not completed.
        let seed = webhook_event::ActiveModel {
            provider: Set("mock".into()),
            provider_event_id: Set(event_id.into()),
            provider_event_type: Set("payment.succeeded".into()),
            neutral_event_kind: Set(Some("payment_succeeded".into())),
            payload: Set(serde_json::json!({})),
            received_at: Set(Utc::now().to_rfc3339()),
            processed_at: Set(None),
            process_error: Set(Some("transient failure on first attempt".into())),
            ..Default::default()
        };
        seed.insert(&*conn).await.expect("seed audit row");

        // Fire two retry deliveries of the same event concurrently.
        let body = payment_succeeded_body(event_id, txn_id);
        let (db1, db2) = (conn.clone(), conn.clone());
        let (b1, b2) = (body.clone(), body);
        let (r1, r2) = tokio::join!(
            handle_webhook_inner(&db1, provider_name, None, http::HeaderMap::new(), b1),
            handle_webhook_inner(&db2, provider_name, None, http::HeaderMap::new(), b2),
        );

        // Both deliveries succeed (one processes, one observes the duplicate).
        assert_eq!(status_of(&r1), 200, "first retry must return 200");
        assert_eq!(status_of(&r2), 200, "second retry must return 200");

        // Exactly one mirror transaction row — no double-insert.
        let txns = transaction::Entity::find()
            .filter(transaction::Column::ProviderTransactionId.eq(txn_id))
            .all(&*conn)
            .await
            .expect("db ok");
        assert_eq!(
            txns.len(),
            1,
            "concurrent retries must produce exactly one transaction mirror row, got {}",
            txns.len()
        );
        assert_eq!(txns[0].amount_total_minor, 4242);
        assert_eq!(txns[0].status, "succeeded");

        // Exactly one audit row, now processed and with the stale error cleared.
        let audit = webhook_event::Entity::find()
            .filter(webhook_event::Column::ProviderEventId.eq(event_id))
            .all(&*conn)
            .await
            .expect("db ok");
        assert_eq!(audit.len(), 1, "retry must reuse the single audit row");
        assert!(
            audit[0].processed_at.is_some(),
            "audit row must be marked processed after the winning apply"
        );
        assert!(
            audit[0].process_error.is_none(),
            "the winning apply clears the stale process_error"
        );
    }
}

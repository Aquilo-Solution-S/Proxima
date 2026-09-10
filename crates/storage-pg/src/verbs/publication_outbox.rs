//! The publication outbox: capture inside the Fact's transaction, and the
//! host-only drain over what was captured (issue #305).
//!
//! Two halves that must not be confused. [`capture_publication_in_tx`] runs
//! inside the caller's Fact transaction and is the only writer of a
//! captured event; the [`PublicationOutboxPort`] impl below runs in a
//! publisher process and only ever moves a record through its delivery
//! lifecycle. Neither half deletes a record — that is compliance erasure's
//! job alone, so an expired lease can make a record deliverable again but
//! never destroy it.

use std::num::NonZeroU32;
use std::time::Duration;

use proxima_core::publication::{PublicationError, PublicationLimits, PublicationPlan};
use proxima_core::storage_ports::publication::{
    AckOutcome, BrokerReceipt, ClaimToken, ClaimedPublication, PublicationOutboxPort, PublisherId,
    ReleaseOutcome,
};
use proxima_core::{OwnerRefKind, SealedPublication, StorageError};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::PgStorage;
use crate::error::{internal, map_err};

/// Capture one listenable Fact's event in the transaction that admits it.
///
/// Ordering inside `ingest_core` is deliberate: the capacity refusal comes
/// FIRST, so an exhausted outbox costs a bounded index probe rather than a
/// serialization of the whole payload; the seal comes second, because it
/// needs the `t` the admission just minted; the insert is last. Every one
/// of the three is an `Err` the caller propagates, and the caller's
/// transaction is dropped — which is the whole point: a Fact that could not
/// capture its event was never written.
///
/// # Errors
///
/// [`StorageError::PublicationRefused`] for an exhausted outbox, an
/// unexportable payload or an oversized envelope; storage errors from the
/// probe and the insert.
///
/// Both bounds come off `plan.limits`, which the engine put there from its
/// own [`PublicationConfig`]. This backend holds no limits of its own: a
/// second copy could only ever disagree with the configured one.
///
/// [`PublicationConfig`]: proxima_core::publication::PublicationConfig
pub(crate) async fn capture_publication_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    t: Uuid,
    plan: &PublicationPlan,
) -> Result<(), StorageError> {
    refuse_when_full(tx, &plan.limits).await?;
    let sealed = SealedPublication::seal(plan, t).map_err(StorageError::from)?;
    let schema_version = i32::try_from(sealed.schema_version.into_inner()).map_err(|_| {
        StorageError::ConstraintViolation("schema version does not fit an integer column".into())
    })?;
    sqlx::query(
        "INSERT INTO proxima_core.publication_outbox
             (t, owner_id, schema_id, schema_version, event_type, event_id,
              envelope, envelope_digest)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(sealed.id)
    .bind(owner_id)
    .bind(sealed.schema_id.as_str())
    .bind(schema_version)
    .bind(&sealed.event_type)
    .bind(&sealed.event_id)
    .bind(&sealed.bytes)
    .bind(sealed.digest.as_slice())
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Refuse a capture that would push the undelivered backlog past its bound.
///
/// The probe is `OFFSET max_pending - 1 LIMIT 1` over the partial index, so
/// a healthy deployment pays for one index tuple rather than a full count.
/// The exact count is only taken on the refusal path, where an operator is
/// about to be told how far behind the publisher is.
async fn refuse_when_full(
    tx: &mut Transaction<'_, Postgres>,
    limits: &PublicationLimits,
) -> Result<(), StorageError> {
    let Some(offset) = limits.max_pending.checked_sub(1) else {
        // `max_pending = 0` admits no record at all; say so without a query.
        return Err(StorageError::from(PublicationError::CapacityExhausted {
            pending: 0,
            max: 0,
        }));
    };
    let offset = i64::try_from(offset).map_err(|_| {
        StorageError::ConstraintViolation("max_pending does not fit a bigint offset".into())
    })?;
    let over: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM proxima_core.publication_outbox
          WHERE state <> 'published'
         OFFSET $1 LIMIT 1",
    )
    .bind(offset)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    if over.is_none() {
        return Ok(());
    }
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.publication_outbox WHERE state <> 'published'",
    )
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;
    Err(StorageError::from(PublicationError::CapacityExhausted {
        pending: pending.try_into().unwrap_or(u64::MAX),
        max: limits.max_pending,
    }))
}

/// Claim deliverable records under a lease, oldest `t` first.
///
/// Deliverable is `pending`, or `claimed` with an expired lease. There is
/// no cursor and no watermark on purpose: a transaction that commits after
/// a later one is simply `pending` on the next call, where an advancing
/// high-water mark would have stepped over it forever.
const CLAIM_SQL: &str = "WITH claimed AS (
             SELECT t
               FROM proxima_core.publication_outbox
              WHERE state = 'pending'
                 OR (state = 'claimed' AND lease_expires_at < now())
              ORDER BY t ASC
              FOR UPDATE SKIP LOCKED
              LIMIT $1
         )
         UPDATE proxima_core.publication_outbox p
            SET state = 'claimed',
                claim_token = uuidv7(),
                claimed_by = $2,
                lease_expires_at = now() + make_interval(secs => $3),
                attempts = p.attempts + 1
           FROM claimed, proxima_core.owners o
          WHERE p.t = claimed.t
            AND o.owner_id = p.owner_id
        RETURNING p.t,
                  p.event_id,
                  p.owner_id,
                  o.kind::text AS owner_kind,
                  p.schema_id,
                  p.schema_version,
                  p.event_type,
                  p.envelope,
                  p.envelope_digest,
                  p.attempts,
                  p.claim_token,
                  p.lease_expires_at";

#[async_trait::async_trait]
impl PublicationOutboxPort for PgStorage {
    async fn claim(
        &self,
        publisher: &PublisherId,
        limit: NonZeroU32,
        lease: Duration,
    ) -> Result<Vec<ClaimedPublication>, StorageError> {
        let rows = sqlx::query(CLAIM_SQL)
            .bind(i64::from(limit.get()))
            .bind(publisher.as_str())
            .bind(lease.as_secs_f64())
            .fetch_all(&self.pool)
            .await
            .map_err(map_err)?;
        rows.iter().map(claimed_from_row).collect()
    }

    async fn mark_published(
        &self,
        id: Uuid,
        claim: ClaimToken,
        receipt: &BrokerReceipt,
    ) -> Result<AckOutcome, StorageError> {
        let sequence = i64::try_from(receipt.sequence).map_err(|_| {
            StorageError::ConstraintViolation("broker sequence does not fit a bigint".into())
        })?;
        let result = sqlx::query(
            "UPDATE proxima_core.publication_outbox
                SET state = 'published',
                    published_at = now(),
                    published_stream = $3,
                    published_seq = $4,
                    claim_token = NULL,
                    claimed_by = NULL,
                    lease_expires_at = NULL
              WHERE t = $1
                AND state = 'claimed'
                AND claim_token = $2",
        )
        .bind(id)
        .bind(claim.into_inner())
        .bind(&receipt.stream)
        .bind(sequence)
        .execute(&self.pool)
        .await
        .map_err(map_err)?;
        if result.rows_affected() > 0 {
            return Ok(AckOutcome::Published);
        }
        Ok(match current_state(&self.pool, id).await? {
            Some(state) if state == "published" => AckOutcome::AlreadyPublished,
            _ => AckOutcome::StaleClaim,
        })
    }

    async fn release(&self, id: Uuid, claim: ClaimToken) -> Result<ReleaseOutcome, StorageError> {
        let result = sqlx::query(
            "UPDATE proxima_core.publication_outbox
                SET state = 'pending',
                    claim_token = NULL,
                    claimed_by = NULL,
                    lease_expires_at = NULL
              WHERE t = $1
                AND state = 'claimed'
                AND claim_token = $2",
        )
        .bind(id)
        .bind(claim.into_inner())
        .execute(&self.pool)
        .await
        .map_err(map_err)?;
        if result.rows_affected() > 0 {
            return Ok(ReleaseOutcome::Released);
        }
        Ok(match current_state(&self.pool, id).await? {
            Some(state) if state == "published" => ReleaseOutcome::AlreadyPublished,
            _ => ReleaseOutcome::StaleClaim,
        })
    }

    async fn pending_count(&self) -> Result<u64, StorageError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.publication_outbox WHERE state <> 'published'",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(map_err)?;
        Ok(count.try_into().unwrap_or(0))
    }
}

/// The record's delivery state, or `None` when no record with that `t`
/// exists (compliance erasure destroyed it, or it never existed).
async fn current_state(pool: &sqlx::PgPool, id: Uuid) -> Result<Option<String>, StorageError> {
    sqlx::query_scalar("SELECT state::text FROM proxima_core.publication_outbox WHERE t = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(map_err)
}

fn claimed_from_row(row: &sqlx::postgres::PgRow) -> Result<ClaimedPublication, StorageError> {
    let owner_kind: String = row.try_get("owner_kind").map_err(internal)?;
    let digest: Vec<u8> = row.try_get("envelope_digest").map_err(internal)?;
    let digest: [u8; 32] = digest.try_into().map_err(|_| {
        StorageError::Internal("captured envelope digest is not 32 bytes".to_string())
    })?;
    let schema_version: i32 = row.try_get("schema_version").map_err(internal)?;
    let attempts: i32 = row.try_get("attempts").map_err(internal)?;
    Ok(ClaimedPublication {
        id: row.try_get("t").map_err(internal)?,
        event_id: row.try_get("event_id").map_err(internal)?,
        owner_id: row.try_get("owner_id").map_err(internal)?,
        owner_kind: if owner_kind == "group" {
            OwnerRefKind::Group
        } else {
            OwnerRefKind::Personal
        },
        schema_id: row.try_get("schema_id").map_err(internal)?,
        schema_version: schema_version.try_into().unwrap_or(0),
        event_type: row.try_get("event_type").map_err(internal)?,
        envelope: row.try_get("envelope").map_err(internal)?,
        digest,
        attempts: attempts.try_into().unwrap_or(0),
        claim: ClaimToken::new(row.try_get("claim_token").map_err(internal)?),
        lease_expires_at: row.try_get("lease_expires_at").map_err(internal)?,
    })
}

#[cfg(test)]
#[path = "publication_outbox_pg_tests.rs"]
mod publication_outbox_pg_tests;

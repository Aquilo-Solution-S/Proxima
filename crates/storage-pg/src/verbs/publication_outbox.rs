//! The publication outbox: capture inside the Fact's transaction, and the
//! host-only drain over what was captured (issue #305).
//!
//! Three parts that must not be confused. [`capture_publication_in_tx`]
//! runs inside the caller's Fact transaction and is the only writer of a
//! captured event; the [`PublicationOutboxPort`] impl below runs in a
//! publisher process and only ever moves a record through its delivery
//! lifecycle; the [`PublicationRetentionPort`] impl reclaims records that
//! were already DELIVERED. Neither of the first two deletes anything, and
//! the third cannot touch an undelivered record: an expired lease makes a
//! record deliverable again, never destroys it, and an event that has not
//! reached the broker leaves this table only through compliance erasure.
//!
//! [`PublicationRetentionPort`]: proxima_core::storage_ports::publication::PublicationRetentionPort

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
/// The probe is `OFFSET max_pending - 1 LIMIT 1` over the partial index,
/// which costs `min(unpublished, max_pending)` index tuples: `OFFSET N`
/// DISCARDS N rows, it does not skip to them. So a healthy deployment pays
/// for the backlog it actually has — a handful of tuples while the
/// publisher keeps up — and a stalled one pays for a full scan of the
/// partial index once the backlog reaches the bound. That is the shape this
/// probe is chosen for: the cost is proportional to how far behind the
/// deployment already is, and it is bounded above by `max_pending` however
/// long the outage lasts. A counter table would be O(1) and would also be a
/// second writer on one row in the path of every listenable write, which
/// trades a bounded scan for a serialization point.
///
/// The exact count is only taken on the refusal path, where an operator is
/// about to be told how far behind the publisher is.
///
/// **The bound is SOFT under concurrent writers.** The probe takes no lock
/// and runs at READ COMMITTED, so N transactions can each see
/// `max_pending - 1` and each insert: the table can settle at
/// `max_pending + (N - 1)` rows, where N is the number of listenable writes
/// in flight at that instant. The overshoot is therefore bounded by the
/// concurrency, not by the traffic, and it is accepted deliberately —
/// making the bound exact costs an advisory lock or a `FOR SHARE` counter
/// row on every listenable write, which is the serialization point the
/// previous paragraph declines.
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

/// Claim deliverable records: fewest attempts first, then oldest `t`.
///
/// Deliverable is `pending`, or `claimed` with an expired lease. There is
/// no cursor and no watermark on purpose: a transaction that commits after
/// a later one is simply `pending` on the next call, where an advancing
/// high-water mark would have stepped over it forever.
///
/// `attempts ASC` before `t ASC` is head-of-line-blocking insurance. Under
/// `t` alone, one record the broker will never accept is re-claimed at the
/// front of every batch forever, and a `batch`-sized window of the backlog
/// behind it never ships. Ordering by attempt count demotes a repeatedly
/// failing record behind every fresher one after its first failure, so a
/// poison record costs one slot per pass instead of the whole queue. Within
/// one attempt count the order is still `t`, so the ordinary case — every
/// record on attempt 1 — is exactly the old oldest-first order.
///
/// The partial index `publication_outbox_pending_idx` still serves this
/// statement's PREDICATE (`state <> 'published'` is implied by the two
/// disjuncts). What it no longer supplies is the sort: `attempts` is not in
/// it, so the plan is an index scan feeding a sort. That sort is over the
/// undelivered backlog only, which `max_pending` bounds — the same set the
/// capacity probe already bounds — and a covering index for it would be a
/// migration this slice does not ship.
const CLAIM_SQL: &str = "WITH claimed AS (
             SELECT t
               FROM proxima_core.publication_outbox
              WHERE state = 'pending'
                 OR (state = 'claimed' AND lease_expires_at < now())
              ORDER BY attempts ASC, t ASC
              FOR UPDATE SKIP LOCKED
              LIMIT $1
         ),
         leased AS (
         UPDATE proxima_core.publication_outbox p
            SET state = 'claimed',
                claim_token = uuidv7(),
                claimed_by = $2,
                lease_expires_at = now() + make_interval(secs => $3),
                attempts = least(p.attempts + 1, 2147483647)
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
                  p.lease_expires_at
         )
         -- The batch is handed back in the same order it was chosen. An
         -- UPDATE ... RETURNING emits rows in whatever order the executor
         -- produced them, so without this the publisher's send order would
         -- be an accident of the plan rather than the queue discipline.
         SELECT * FROM leased ORDER BY attempts ASC, t ASC";

/// Shortest lease a claim may take.
///
/// A zero (or sub-second) lease sets `lease_expires_at = now()`, so the
/// record is deliverable again before the claiming publisher has finished
/// its first `publish`. The floor is a refusal rather than a clamp: a
/// caller that asked for 0 s asked for something that cannot work, and
/// silently substituting 1 s would hide the misconfiguration.
const MIN_CLAIM_LEASE: Duration = Duration::from_secs(1);

#[async_trait::async_trait]
impl PublicationOutboxPort for PgStorage {
    async fn claim(
        &self,
        publisher: &PublisherId,
        limit: NonZeroU32,
        lease: Duration,
    ) -> Result<Vec<ClaimedPublication>, StorageError> {
        if lease < MIN_CLAIM_LEASE {
            return Err(StorageError::ConstraintViolation(format!(
                "publication claim lease is {} ms, under the {} ms floor; a lease shorter \
                 than one broker round trip expires while the first publisher is still \
                 sending, so every record would be re-claimed under it",
                lease.as_millis(),
                MIN_CLAIM_LEASE.as_millis(),
            )));
        }
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

/// Operator-only retention of DELIVERED records (issue #305, review R7).
///
/// The predicate is written twice over — `state = 'published'` and
/// `published_at < now() - interval` — and the second is not redundant:
/// `publication_outbox_published_chk` makes `published_at` NOT NULL exactly
/// when the state is `published`, so a row in any other state has a NULL
/// `published_at` and the comparison is NULL, not true. A future edit that
/// dropped the state predicate would therefore still not reach a pending
/// record. The bound is `ctid IN (... LIMIT n)` rather than a bare `LIMIT`,
/// because `DELETE` takes no `LIMIT` of its own.
#[async_trait::async_trait]
impl proxima_core::storage_ports::publication::PublicationRetentionPort for PgStorage {
    async fn prune_published(
        &self,
        older_than: Duration,
        limit: NonZeroU32,
    ) -> Result<u64, StorageError> {
        let deleted = sqlx::query(
            "DELETE FROM proxima_core.publication_outbox
              WHERE ctid IN (
                    SELECT p.ctid
                      FROM proxima_core.publication_outbox p
                     WHERE p.state = 'published'
                       AND p.published_at < now() - make_interval(secs => $1)
                     ORDER BY p.published_at ASC
                     LIMIT $2
              )",
        )
        .bind(older_than.as_secs_f64())
        .bind(i64::from(limit.get()))
        .execute(&self.pool)
        .await
        .map_err(map_err)?
        .rows_affected();
        Ok(deleted)
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

/// Decode one claimed row, with NO repair of a value the database should
/// not have been able to store.
///
/// Every fallback this could take would be a lie the publisher then ships:
/// an unrecognised `owner_kind` read as `personal` routes a group's event
/// onto a personal subject, and a negative `schema_version` or `attempts`
/// read as `0` reports a contract version and a retry count that were never
/// written. None of these is reachable through the write path — the enum is
/// a SQL enum, the two integers are written from `u32`s — so reaching one
/// means the row was altered outside it, and the honest answer is to refuse
/// the record rather than to publish a plausible reconstruction of it.
fn claimed_from_row(row: &sqlx::postgres::PgRow) -> Result<ClaimedPublication, StorageError> {
    let id: Uuid = row.try_get("t").map_err(internal)?;
    let owner_kind: String = row.try_get("owner_kind").map_err(internal)?;
    let owner_kind = match owner_kind.as_str() {
        "personal" => OwnerRefKind::Personal,
        "group" => OwnerRefKind::Group,
        other => {
            return Err(StorageError::Internal(format!(
                "publication {id} names owner kind {other:?}, which is not a known owner kind"
            )));
        }
    };
    let digest: Vec<u8> = row.try_get("envelope_digest").map_err(internal)?;
    let digest: [u8; 32] = digest.try_into().map_err(|_| {
        StorageError::Internal("captured envelope digest is not 32 bytes".to_string())
    })?;
    let schema_version: i32 = row.try_get("schema_version").map_err(internal)?;
    let schema_version = u32::try_from(schema_version).map_err(|_| {
        StorageError::Internal(format!(
            "publication {id} carries schema version {schema_version}, which is not a version"
        ))
    })?;
    let attempts: i32 = row.try_get("attempts").map_err(internal)?;
    let attempts = u32::try_from(attempts).map_err(|_| {
        StorageError::Internal(format!(
            "publication {id} carries {attempts} attempts, which is not a count"
        ))
    })?;
    Ok(ClaimedPublication {
        id,
        event_id: row.try_get("event_id").map_err(internal)?,
        owner_id: row.try_get("owner_id").map_err(internal)?,
        owner_kind,
        schema_id: row.try_get("schema_id").map_err(internal)?,
        schema_version,
        event_type: row.try_get("event_type").map_err(internal)?,
        envelope: row.try_get("envelope").map_err(internal)?,
        digest,
        attempts,
        claim: ClaimToken::new(row.try_get("claim_token").map_err(internal)?),
        lease_expires_at: row.try_get("lease_expires_at").map_err(internal)?,
    })
}

#[cfg(test)]
#[path = "publication_outbox_pg_tests.rs"]
mod publication_outbox_pg_tests;

//! The publication outbox: capture inside the Fact's transaction, and the
//! host-only drain over what was captured (issue #305).
//!
//! Four parts that must not be confused. [`capture_publication_in_tx`]
//! runs inside the caller's Fact transaction and is the only writer of a
//! captured event and of its delivery lanes, one per registered consumer;
//! the [`PublicationOutboxPort`] impl below runs in a publisher process and
//! only ever moves one consumer's lane through its delivery lifecycle; the
//! [`PublicationRetentionPort`] impl reclaims records that were already
//! DELIVERED to every consumer; the [`PublicationConsumerPort`] impl
//! registers consumers and retires them. None of the first two deletes
//! anything, the third cannot touch an undelivered record, and only a retire
//! cancels a lane that was not delivered: an expired lease makes a lane
//! deliverable again, never destroys it.
//!
//! A record's own `state` is the aggregate of its lanes: `published` exactly
//! when no lane is undelivered. Every statement that can make the last lane
//! delivered or go away (a mark, a retire) settles the aggregate in the same
//! transaction, under the record's row lock.
//!
//! [`PublicationRetentionPort`]: proxima_core::storage_ports::publication::PublicationRetentionPort

use std::num::NonZeroU32;
use std::time::Duration;

use proxima_core::publication::{
    PublicationError, PublicationLimits, PublicationPlan, SubjectPosition,
};
use proxima_core::storage_ports::publication::{
    AckOutcome, ClaimToken, ClaimedPublication, ConsumerName, OriginScope, PublicationConsumerPort,
    PublicationOriginEligibility, PublicationOriginEligibilityPort, PublicationOutboxPort,
    PublisherId, ReleaseOutcome, RetireOutcome,
};
use proxima_core::{SealedPublication, StorageError};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::PgStorage;
use crate::error::{internal, map_err};

/// The next position in the series `handle`: its publication sequence,
/// incremented. The caller holds the series lock (every Fact admission takes
/// it before materializing), so two writes to one series never read the same
/// value, and the column's trigger refuses to let it go back.
///
/// # Errors
///
/// Storage errors from the update; a missing head is a constraint violation,
/// since the admission that calls this just wrote it.
pub(crate) async fn next_series_position(
    tx: &mut Transaction<'_, Postgres>,
    handle: Uuid,
) -> Result<SubjectPosition, StorageError> {
    let sequence: i64 = sqlx::query_scalar(
        "UPDATE proxima_core.memory_head
            SET publication_sequence = publication_sequence + 1
          WHERE handle = $1
      RETURNING publication_sequence",
    )
    .bind(handle)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    .ok_or_else(|| {
        StorageError::ConstraintViolation(
            "publication capture requires the series head its admission just wrote".into(),
        )
    })?;
    let sequence = u64::try_from(sequence)
        .ok()
        .and_then(std::num::NonZeroU64::new)
        .ok_or_else(|| internal("publication_sequence is not positive after an increment"))?;
    Ok(SubjectPosition::new(handle, sequence))
}

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
/// The record gets one delivery lane per consumer registered as of this
/// statement, read and written in ONE statement so the set it fans out to is
/// the set it read. With no consumer registered there is nobody to owe the
/// event to, and the record is published at once rather than held against
/// the pending ceiling forever.
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
/// `occurred_at` is the value the admission bound into `memory.occurred_at`
/// (`PreparedMemoryAdmission::occurred_at`), so the envelope's `time` and a
/// read of the Fact agree. Only a new Fact admission captures, and every new
/// Fact resolves one; `None` is a caller fault.
///
/// [`PublicationConfig`]: proxima_core::publication::PublicationConfig
pub(crate) async fn capture_publication_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    t: Uuid,
    occurred_at: Option<time::OffsetDateTime>,
    plan: &PublicationPlan,
    position: SubjectPosition,
) -> Result<(), StorageError> {
    let occurred_at = occurred_at.ok_or_else(|| {
        internal("publication capture requires the occurrence time of a new Fact admission")
    })?;
    refuse_when_full(tx, &plan.limits).await?;
    let sealed =
        SealedPublication::seal(plan, t, occurred_at, position).map_err(StorageError::from)?;
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
    let lanes = sqlx::query(
        "INSERT INTO proxima_core.publication_delivery (t, consumer)
         SELECT $1, name FROM proxima_core.publication_consumer",
    )
    .bind(t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?
    .rows_affected();
    if lanes == 0 {
        sqlx::query(
            "UPDATE proxima_core.publication_outbox
                SET state = 'published', published_at = now()
              WHERE t = $1",
        )
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    }
    let captured = sqlx::query_scalar::<_, Option<String>>(
        "SELECT memory.source_id
           FROM proxima_core.memory memory
           JOIN proxima_core.owners owner ON owner.owner_id = memory.owner_id
          WHERE memory.t = $1
            AND memory.owner_id = $2
            AND memory.kind = 'fact'",
    )
    .bind(t)
    .bind(owner_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    .ok_or_else(|| {
        StorageError::ConstraintViolation(
            "publication capture requires its freshly inserted retained Fact".into(),
        )
    })?;
    sqlx::query(
        "INSERT INTO proxima_core.publication_origin
             (t, original_owner_id, source_id)
         VALUES ($1, $2, $3)",
    )
    .bind(t)
    .bind(owner_id)
    .bind(captured)
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

/// Claim one consumer's deliverable lanes: fewest attempts first, then
/// oldest `t`.
///
/// Deliverable is a lane of THIS consumer that is `pending`, or `claimed`
/// with an expired lease. Another consumer's lanes are not read, so its
/// failures and its lag cannot hold this one. There is no cursor and no
/// watermark on purpose: a transaction that commits after a later one is
/// simply `pending` on the next call, where an advancing high-water mark
/// would have stepped over it forever.
///
/// `attempts ASC` before `t ASC` is head-of-line-blocking insurance. Under
/// `t` alone, one record the intake will never accept is re-claimed at the
/// front of every batch forever, and a `batch`-sized window of the backlog
/// behind it never ships. Ordering by attempt count demotes a repeatedly
/// failing record behind every fresher one after its first failure, so a
/// poison record costs one slot per pass instead of the whole queue. Within
/// one attempt count the order is still `t`, so the ordinary case — every
/// record on attempt 1 — is oldest first. That demotion is also why
/// delivery order is not commit order (docs/host-integration.md).
///
/// `publication_delivery_pending_idx` (`consumer, attempts, t` over the
/// undelivered lanes) serves both the predicate and the sort. `FOR UPDATE OF
/// lane` locks the lane only: a record row stays free for the other
/// consumers' marks.
const CLAIM_SQL: &str = "WITH claimed AS (
             SELECT lane.t
               FROM proxima_core.publication_delivery lane
               JOIN proxima_core.publication_outbox record ON record.t = lane.t
              WHERE lane.consumer = $4
                AND (lane.state = 'pending'
                  OR (lane.state = 'claimed' AND lane.lease_expires_at < now()))
                AND EXISTS (
                    SELECT 1 FROM proxima_core.publication_origin origin
                    JOIN proxima_core.owners original_owner
                      ON original_owner.owner_id = origin.original_owner_id
                   WHERE origin.t = record.t
                     AND origin.original_owner_id = record.owner_id
                )
                AND NOT EXISTS (
                    SELECT 1 FROM proxima_core.erased_pin_target erased
                     WHERE erased.t = record.t
                )
              ORDER BY lane.attempts ASC, lane.t ASC
              FOR UPDATE OF lane SKIP LOCKED
              LIMIT $1
         ),
         leased AS (
         UPDATE proxima_core.publication_delivery lane
            SET state = 'claimed',
                claim_token = uuidv7(),
                claimed_by = $2,
                lease_expires_at = now() + make_interval(secs => $3),
                attempts = least(lane.attempts + 1, 2147483647)
           FROM claimed
          WHERE lane.t = claimed.t
            AND lane.consumer = $4
        RETURNING lane.t, lane.attempts, lane.claim_token, lane.lease_expires_at
         )
         -- The batch is handed back in the same order it was chosen. An
         -- UPDATE ... RETURNING emits rows in whatever order the executor
         -- produced them, so without this the publisher's send order would
         -- be an accident of the plan rather than the queue discipline.
         SELECT p.t,
                p.event_id,
                p.owner_id,
                p.schema_id,
                p.schema_version,
                p.event_type,
                p.envelope,
                p.envelope_digest,
                leased.attempts,
                leased.claim_token,
                leased.lease_expires_at
           FROM leased
           JOIN proxima_core.publication_outbox p ON p.t = leased.t
           JOIN proxima_core.owners o ON o.owner_id = p.owner_id
          ORDER BY leased.attempts ASC, leased.t ASC";

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
        consumer: &ConsumerName,
        publisher: &PublisherId,
        limit: NonZeroU32,
        lease: Duration,
    ) -> Result<Vec<ClaimedPublication>, StorageError> {
        if lease < MIN_CLAIM_LEASE {
            return Err(StorageError::ConstraintViolation(format!(
                "publication claim lease is {} ms, under the {} ms floor; a lease shorter \
                 than one delivery expires while the first publisher is still \
                 delivering, so every record would be re-claimed under it",
                lease.as_millis(),
                MIN_CLAIM_LEASE.as_millis(),
            )));
        }
        let mut tx = self.platform_transaction().await?;
        let rows = sqlx::query(CLAIM_SQL)
            .bind(i64::from(limit.get()))
            .bind(publisher.as_str())
            .bind(lease.as_secs_f64())
            .bind(consumer.as_str())
            .fetch_all(tx.as_mut())
            .await
            .map_err(map_err)?;
        let result = rows.iter().map(claimed_from_row).collect();
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn mark_published(
        &self,
        consumer: &ConsumerName,
        id: Uuid,
        claim: ClaimToken,
    ) -> Result<AckOutcome, StorageError> {
        let mut tx = self.platform_transaction().await?;
        let result = sqlx::query(
            "UPDATE proxima_core.publication_delivery
                SET state = 'published',
                    published_at = now(),
                    claim_token = NULL,
                    claimed_by = NULL,
                    lease_expires_at = NULL
              WHERE t = $1
                AND consumer = $2
                AND state = 'claimed'
                AND claim_token = $3",
        )
        .bind(id)
        .bind(consumer.as_str())
        .bind(claim.into_inner())
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
        let outcome = if result.rows_affected() > 0 {
            settle_records(&mut tx, &[id]).await?;
            AckOutcome::Published
        } else {
            match current_lane_state(tx.as_mut(), id, consumer).await? {
                Some(state) if state == "published" => AckOutcome::AlreadyPublished,
                _ => AckOutcome::StaleClaim,
            }
        };
        crate::owner_scope::finish_transaction(tx, Ok(outcome)).await
    }

    async fn release(
        &self,
        consumer: &ConsumerName,
        id: Uuid,
        claim: ClaimToken,
    ) -> Result<ReleaseOutcome, StorageError> {
        let mut tx = self.platform_transaction().await?;
        let result = sqlx::query(
            "UPDATE proxima_core.publication_delivery
                SET state = 'pending',
                    claim_token = NULL,
                    claimed_by = NULL,
                    lease_expires_at = NULL
              WHERE t = $1
                AND consumer = $2
                AND state = 'claimed'
                AND claim_token = $3",
        )
        .bind(id)
        .bind(consumer.as_str())
        .bind(claim.into_inner())
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
        let outcome = if result.rows_affected() > 0 {
            ReleaseOutcome::Released
        } else {
            match current_lane_state(tx.as_mut(), id, consumer).await? {
                Some(state) if state == "published" => ReleaseOutcome::AlreadyPublished,
                _ => ReleaseOutcome::StaleClaim,
            }
        };
        crate::owner_scope::finish_transaction(tx, Ok(outcome)).await
    }

    async fn pending_count(&self) -> Result<u64, StorageError> {
        let mut tx = self.platform_transaction().await?;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.publication_outbox WHERE state <> 'published'",
        )
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?;
        crate::owner_scope::finish_transaction(tx, Ok(count.try_into().unwrap_or(0))).await
    }
}

/// Mark published each of `records` that has no undelivered lane left.
///
/// Locks the records FIRST, in a statement of its own, and decides in the
/// next one. Under READ COMMITTED that next statement takes a fresh snapshot
/// after the lock is held, so two consumers marking the last two lanes of one
/// record serialize on it and the second sees the first's lane: one of them
/// settles the record, never neither.
async fn settle_records(
    tx: &mut Transaction<'_, Postgres>,
    records: &[Uuid],
) -> Result<(), StorageError> {
    sqlx::query(
        "SELECT 1 FROM proxima_core.publication_outbox
          WHERE t = ANY($1) AND state <> 'published'
          ORDER BY t
            FOR UPDATE",
    )
    .bind(records)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    sqlx::query(
        "UPDATE proxima_core.publication_outbox record
            SET state = 'published', published_at = now()
          WHERE record.t = ANY($1)
            AND record.state <> 'published'
            AND NOT EXISTS (
                SELECT 1 FROM proxima_core.publication_delivery lane
                 WHERE lane.t = record.t AND lane.state <> 'published'
            )",
    )
    .bind(records)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(())
}

#[async_trait::async_trait]
impl PublicationConsumerPort for PgStorage {
    async fn register(&self, consumer: &ConsumerName) -> Result<(), StorageError> {
        let mut tx = self.platform_transaction().await?;
        let result = sqlx::query(
            "INSERT INTO proxima_core.publication_consumer (name) VALUES ($1)
             ON CONFLICT (name) DO NOTHING",
        )
        .bind(consumer.as_str())
        .execute(tx.as_mut())
        .await
        .map_err(map_err)
        .map(|_| ());
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn registered(&self) -> Result<Vec<ConsumerName>, StorageError> {
        let mut tx = self.platform_transaction().await?;
        let names: Vec<String> =
            sqlx::query_scalar("SELECT name FROM proxima_core.publication_consumer ORDER BY name")
                .fetch_all(tx.as_mut())
                .await
                .map_err(map_err)?;
        // The CHECK constraint spells the same rule as `ConsumerName::new`,
        // so a refusal here means a row was written around both.
        let result = names
            .into_iter()
            .map(|name| ConsumerName::new(name).map_err(internal))
            .collect();
        crate::owner_scope::finish_transaction(tx, result).await
    }

    /// One transaction: block every lane write, drop the consumer (its lanes
    /// go with it, by cascade), settle the records it was the last to owe.
    ///
    /// The `SHARE` lock on the lanes is what makes the settle complete. A
    /// capture or a mark that is already writing lanes finishes first; one
    /// that starts later waits, and then reads a consumer set without this
    /// consumer. Without it a capture could read the set before the delete,
    /// fan a lane out to the retired consumer after it, and leave a record
    /// nobody will ever settle. Retiring is an operator act, so holding up
    /// listenable writes for the length of one delete is the cheap side.
    async fn retire(&self, consumer: &ConsumerName) -> Result<RetireOutcome, StorageError> {
        let mut tx = self.platform_transaction().await?;
        sqlx::query("LOCK TABLE proxima_core.publication_delivery IN SHARE MODE")
            .execute(tx.as_mut())
            .await
            .map_err(map_err)?;
        let owed: Vec<Uuid> = sqlx::query_scalar(
            "SELECT t FROM proxima_core.publication_delivery
              WHERE consumer = $1 AND state <> 'published'",
        )
        .bind(consumer.as_str())
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)?;
        let removed = sqlx::query("DELETE FROM proxima_core.publication_consumer WHERE name = $1")
            .bind(consumer.as_str())
            .execute(tx.as_mut())
            .await
            .map_err(map_err)?
            .rows_affected();
        let outcome = if removed == 0 {
            RetireOutcome::NotRegistered
        } else {
            settle_records(&mut tx, &owed).await?;
            RetireOutcome::Retired {
                undelivered: owed.len().try_into().unwrap_or(u64::MAX),
            }
        };
        crate::owner_scope::finish_transaction(tx, Ok(outcome)).await
    }
}

#[async_trait::async_trait]
impl PublicationOriginEligibilityPort for PgStorage {
    async fn check_committed(
        &self,
        original_owner: proxima_core::OwnerId,
        fact_id: proxima_core::MemoryId,
    ) -> Result<PublicationOriginEligibility, StorageError> {
        let mut tx = self.platform_transaction().await?;
        crate::access::owner_columns::lock_host_lifecycle_fence_shared_tx(&mut tx).await?;
        let result = self
            .check_in_transaction(&mut tx, original_owner, fact_id)
            .await?;
        tx.commit().await.map_err(map_err)?;
        Ok(result)
    }

    async fn check_in_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        original_owner: proxima_core::OwnerId,
        fact_id: proxima_core::MemoryId,
    ) -> Result<PublicationOriginEligibility, StorageError> {
        // The caller's UnitOfWork holds the lifecycle-wide shared fence from
        // transaction entry. Take the same per-Fact target fence used by
        // hard deletion and keep it through the caller's host-state write and
        // commit. Opening a nested pool transaction here would split the
        // eligibility decision from that write and permit a time-of-check race.
        super::forget::lock_lifecycle_targets_tx(tx, &[fact_id.into_inner()]).await?;
        let owner_id = original_owner.into_inner();
        let eligible: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1
                  FROM proxima_core.publication_origin origin
                 WHERE origin.t = $1
                   AND origin.original_owner_id = $2
                   AND NOT EXISTS (
                       SELECT 1 FROM proxima_core.erased_pin_target erased
                        WHERE erased.t = origin.t
                   )
            )",
        )
        .bind(fact_id.into_inner())
        .bind(owner_id)
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?;
        Ok(if eligible {
            PublicationOriginEligibility::Eligible
        } else {
            PublicationOriginEligibility::Ineligible
        })
    }
}

impl PgStorage {
    /// Read this installation's minted identity.
    ///
    /// The value a host that forwards the outbox to its own broker stamps on
    /// its copies, and its cleaner checks that stamp against. Both must come
    /// from THIS database — the same one that answers eligibility — or the
    /// check degenerates into comparing a configuration value with itself.
    ///
    /// Read once at boot rather than per message: the row is written by
    /// migration 0012 and a trigger refuses to change or remove it, so a
    /// second read can only return the same answer.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the row is unreadable or absent. Absent
    /// means the schema was tampered with — the migration seeds it — and
    /// refusing to boot is better than publishing unstamped events that no
    /// cleaner will ever be able to vouch for.
    pub async fn origin_scope(&self) -> Result<OriginScope, StorageError> {
        let mut tx = self.platform_transaction().await?;
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT installation_id FROM proxima_core.installation WHERE singleton",
        )
        .fetch_optional(tx.as_mut())
        .await
        .map_err(map_err)?;
        let result = id.map(OriginScope::new).ok_or_else(|| {
            internal("proxima_core.installation holds no row; the schema is incomplete")
        });
        crate::owner_scope::finish_transaction(tx, result).await
    }
}

/// Operator-only retention of DELIVERED records (issue #305, review R7).
///
/// DELIVERED means to every consumer: the record's `state` is the aggregate
/// of its lanes, and its lanes go with it by cascade. The predicate is
/// written twice over — `state = 'published'` and
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
        let mut tx = self.platform_transaction().await?;
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
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?
        .rows_affected();
        crate::owner_scope::finish_transaction(tx, Ok(deleted)).await
    }
}

/// One consumer's lane state on a record, or `None` when there is no such
/// lane (compliance erasure destroyed the record, the consumer was retired,
/// or it never existed).
async fn current_lane_state(
    connection: &mut sqlx::PgConnection,
    id: Uuid,
    consumer: &ConsumerName,
) -> Result<Option<String>, StorageError> {
    sqlx::query_scalar(
        "SELECT state::text FROM proxima_core.publication_delivery
          WHERE t = $1 AND consumer = $2",
    )
    .bind(id)
    .bind(consumer.as_str())
    .fetch_optional(connection)
    .await
    .map_err(map_err)
}

/// Decode one claimed row, with NO repair of a value the database should
/// not have been able to store.
///
/// Every fallback this could take would be a lie the publisher then ships:
/// a negative `schema_version` or `attempts`
/// read as `0` reports a contract version and a retry count that were never
/// written. None of these is reachable through the write path — the enum is
/// a SQL enum, the two integers are written from `u32`s — so reaching one
/// means the row was altered outside it, and the honest answer is to refuse
/// the record rather than to publish a plausible reconstruction of it.
fn claimed_from_row(row: &sqlx::postgres::PgRow) -> Result<ClaimedPublication, StorageError> {
    let id: Uuid = row.try_get("t").map_err(internal)?;
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

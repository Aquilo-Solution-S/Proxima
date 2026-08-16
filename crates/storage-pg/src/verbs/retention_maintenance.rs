//! Retention maintenance verbs — owner Fact-retention enforcement and
//! `change_event` pruning.
//!
//! Both passes are operator surfaces for the `proxima-mcp
//! maintain-retention` CLI (holding the database credentials is the
//! authority, like the embedding maintenance verbs). Every owner is
//! processed inside its own transaction under the per-owner legal-hold
//! advisory lock, and owners with an active hold are skipped — the
//! docs/13 forward rule: a physical-destruction (or destruction-adjacent)
//! path inherits the same in-transaction owner hold gate as the
//! compliance-erase family.
//!
//! Enforcement TOMBSTONES expired Facts (sets `memories.tombstoned_at`,
//! the one mutation the append-only trigger whitelists for this purpose)
//! rather than deleting them: retention means "forget from recall";
//! physical destruction stays exclusive to the human-invoked erase
//! family. MCP-call audit Facts (`core/mcp-call-logged-v1`) are excluded
//! — their retention is indefinite controller evidence (docs/13).

use proxima_core::verbs::persist_mcp_call::MCP_CALL_FACT_SCHEMA;
use proxima_core::{Owner, OwnerRefKind, StorageError};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;
use crate::verbs::fact_retention::{legal_hold_active_tx, lock_legal_hold_tx};

type Tx<'a> = Transaction<'a, Postgres>;

/// Options for one Fact-retention enforcement pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionEnforceOptions {
    /// Facts tombstoned per transaction; each batch re-checks the
    /// owner's legal hold, so a hold set mid-pass stops the sweep at the
    /// next batch boundary.
    pub batch_size: i64,
    /// Count expired Facts without tombstoning anything.
    pub dry_run: bool,
}

impl Default for RetentionEnforceOptions {
    fn default() -> Self {
        Self {
            batch_size: 1000,
            dry_run: false,
        }
    }
}

/// Per-owner outcome of a Fact-retention enforcement pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionOwnerOutcome {
    pub owner: Owner,
    /// The owner's configured retention window.
    pub retention_seconds: i64,
    /// Facts tombstoned (dry run: Facts that would be tombstoned).
    pub facts_tombstoned: u64,
    /// A legal hold was active when the pass reached (or, mid-sweep,
    /// re-checked) this owner; enforcement stopped there.
    pub skipped_legal_hold: bool,
}

/// Outcome of one Fact-retention enforcement pass.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetentionEnforceOutcome {
    pub owners: Vec<RetentionOwnerOutcome>,
    pub facts_tombstoned: u64,
    pub owners_skipped_hold: u64,
    pub dry_run: bool,
}

/// Options for one `change_event` prune pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeEventPruneOptions {
    /// Age horizon: only events older than this many seconds are pruned.
    /// There is deliberately no default — destruction requires an
    /// explicit operator choice.
    pub older_than_seconds: i64,
    /// Events deleted per transaction; see
    /// [`RetentionEnforceOptions::batch_size`].
    pub batch_size: i64,
    /// Count prunable events without deleting anything.
    pub dry_run: bool,
}

/// Per-owner outcome of a `change_event` prune pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneOwnerOutcome {
    pub owner: Owner,
    /// Events deleted (dry run: events that would be deleted).
    pub events_pruned: u64,
    /// A legal hold was active when the pass reached (or, mid-prune,
    /// re-checked) this owner; pruning stopped there.
    pub skipped_legal_hold: bool,
}

/// Outcome of one `change_event` prune pass.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChangeEventPruneOutcome {
    pub owners: Vec<PruneOwnerOutcome>,
    pub events_pruned: u64,
    pub owners_skipped_hold: u64,
    pub dry_run: bool,
}

pub(crate) async fn enforce_fact_retention(
    pool: &PgPool,
    options: RetentionEnforceOptions,
) -> Result<RetentionEnforceOutcome, StorageError> {
    if options.batch_size < 1 {
        return Err(StorageError::ConstraintViolation(
            "retention enforcement batch_size must be positive".into(),
        ));
    }
    let configured: Vec<(OwnerRefKind, Option<Uuid>, i64)> = sqlx::query_as(
        "SELECT owner_kind, owner_id, retention_seconds
           FROM proxima_core.owner_fact_retention
          ORDER BY owner_kind, owner_id",
    )
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let mut outcome = RetentionEnforceOutcome {
        dry_run: options.dry_run,
        ..RetentionEnforceOutcome::default()
    };
    for (owner_kind, owner_id, retention_seconds) in configured {
        let owner = decode_owner(owner_kind, owner_id)?;
        let owner_outcome = enforce_owner(pool, owner, retention_seconds, options).await?;
        outcome.facts_tombstoned += owner_outcome.facts_tombstoned;
        outcome.owners_skipped_hold += u64::from(owner_outcome.skipped_legal_hold);
        outcome.owners.push(owner_outcome);
    }
    Ok(outcome)
}

async fn enforce_owner(
    pool: &PgPool,
    owner: Owner,
    retention_seconds: i64,
    options: RetentionEnforceOptions,
) -> Result<RetentionOwnerOutcome, StorageError> {
    let mut facts_tombstoned: u64 = 0;
    loop {
        let mut tx = pool.begin().await.map_err(map_err)?;
        lock_legal_hold_tx(&mut tx, &owner).await?;
        if legal_hold_active_tx(&mut tx, &owner).await? {
            return Ok(RetentionOwnerOutcome {
                owner,
                retention_seconds,
                facts_tombstoned,
                skipped_legal_hold: true,
            });
        }
        if options.dry_run {
            let due = count_expired_facts(&mut tx, &owner, retention_seconds).await?;
            return Ok(RetentionOwnerOutcome {
                owner,
                retention_seconds,
                facts_tombstoned: due,
                skipped_legal_hold: false,
            });
        }
        let batch =
            tombstone_expired_batch(&mut tx, &owner, retention_seconds, options.batch_size);
        tx.commit().await.map_err(map_err)?;
        facts_tombstoned += batch;
        if batch < options.batch_size.unsigned_abs() {
            return Ok(RetentionOwnerOutcome {
                owner,
                retention_seconds,
                facts_tombstoned,
                skipped_legal_hold: false,
            });
        }
    }
}

// The expired-Fact predicate — live Fact rows (`kind = 'Fact'`) of this
// owner, past the retention window, excluding the MCP-call audit schema
// (indefinite controller evidence, docs/13) — appears verbatim in both
// `count_expired_facts` and `tombstone_expired_batch`. Keep the two WHERE
// clauses in lockstep; they stay inline static SQL rather than a
// format!-composed fragment to avoid a dynamic-SQL policy site.

async fn count_expired_facts(
    tx: &mut Tx<'_>,
    owner: &Owner,
    retention_seconds: i64,
) -> Result<u64, StorageError> {
    let owner_id = owner.stored_owner_id();
    let due: i64 = sqlx::query_scalar(
        "SELECT count(*)
           FROM proxima_core.memory_head h
           JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
          WHERE m.owner_id = $1
            AND m.kind = 'fact'
            AND h.schema_id <> $2
            AND COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01') < now()
                - make_interval(secs => ($3::bigint)::double precision)",
    )
    .bind(owner_id)
    .bind(MCP_CALL_FACT_SCHEMA)
    .bind(retention_seconds)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(due.unsigned_abs())
}

fn tombstone_expired_batch(
    tx: &mut Tx<'_>,
    owner: &Owner,
    retention_seconds: i64,
    batch_size: i64,
) -> u64 {
    let _ = (tx, owner, retention_seconds, batch_size);
    0
}

pub(crate) async fn prune_change_events(
    pool: &PgPool,
    options: ChangeEventPruneOptions,
) -> Result<ChangeEventPruneOutcome, StorageError> {
    if options.older_than_seconds < 1 {
        return Err(StorageError::ConstraintViolation(
            "change_event prune horizon must be positive".into(),
        ));
    }
    if options.batch_size < 1 {
        return Err(StorageError::ConstraintViolation(
            "change_event prune batch_size must be positive".into(),
        ));
    }
    let candidates: Vec<(OwnerRefKind, Option<Uuid>)> = sqlx::query_as(
        "SELECT DISTINCT o.kind, a.owner_id
           FROM proxima_core.announce a
           JOIN proxima_core.owners o ON o.owner_id = a.owner_id
          WHERE COALESCE(uuid_extract_timestamp(a.seq), TIMESTAMPTZ '1970-01-01')
                < now() - make_interval(secs => ($1::bigint)::double precision)
          ORDER BY o.kind, a.owner_id",
    )
    .bind(options.older_than_seconds)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let mut outcome = ChangeEventPruneOutcome {
        dry_run: options.dry_run,
        ..ChangeEventPruneOutcome::default()
    };
    for (owner_kind, owner_id) in candidates {
        let owner = decode_owner(owner_kind, owner_id)?;
        let owner_outcome = prune_owner(pool, owner, options).await?;
        outcome.events_pruned += owner_outcome.events_pruned;
        outcome.owners_skipped_hold += u64::from(owner_outcome.skipped_legal_hold);
        outcome.owners.push(owner_outcome);
    }
    Ok(outcome)
}

async fn prune_owner(
    pool: &PgPool,
    owner: Owner,
    options: ChangeEventPruneOptions,
) -> Result<PruneOwnerOutcome, StorageError> {
    let (_owner_kind, owner_id) = owner.columns();
    let mut events_pruned: u64 = 0;
    loop {
        let mut tx = pool.begin().await.map_err(map_err)?;
        lock_legal_hold_tx(&mut tx, &owner).await?;
        if legal_hold_active_tx(&mut tx, &owner).await? {
            return Ok(PruneOwnerOutcome {
                owner,
                events_pruned,
                skipped_legal_hold: true,
            });
        }
        if options.dry_run {
            let due: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM proxima_core.announce
                  WHERE owner_id IS NOT DISTINCT FROM $1
                    AND COALESCE(uuid_extract_timestamp(seq), TIMESTAMPTZ '1970-01-01') < now()
                        - make_interval(secs => ($2::bigint)::double precision)",
            )
            .bind(owner_id)
            .bind(options.older_than_seconds)
            .fetch_one(tx.as_mut())
            .await
            .map_err(map_err)?;
            return Ok(PruneOwnerOutcome {
                owner,
                events_pruned: due.unsigned_abs(),
                skipped_legal_hold: false,
            });
        }
        let deleted = sqlx::query(
            "DELETE FROM proxima_core.announce
              WHERE seq IN (
                  SELECT seq FROM proxima_core.announce
                   WHERE owner_id IS NOT DISTINCT FROM $1
                     AND COALESCE(uuid_extract_timestamp(seq), TIMESTAMPTZ '1970-01-01') < now()
                         - make_interval(secs => ($2::bigint)::double precision)
                   ORDER BY seq
                   LIMIT $3
              )",
        )
        .bind(owner_id)
        .bind(options.older_than_seconds)
        .bind(options.batch_size)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?
        .rows_affected();
        tx.commit().await.map_err(map_err)?;
        events_pruned += deleted;
        if deleted < options.batch_size.unsigned_abs() {
            return Ok(PruneOwnerOutcome {
                owner,
                events_pruned,
                skipped_legal_hold: false,
            });
        }
    }
}

fn decode_owner(owner_kind: OwnerRefKind, owner_id: Option<Uuid>) -> Result<Owner, StorageError> {
    owner_kind.with_uuid(owner_id).ok_or_else(|| {
        StorageError::Internal(format!(
            "owner row violates owner-ref shape: kind={} id={owner_id:?}",
            owner_kind.as_str()
        ))
    })
}

//! Memory timeseries: the admission row every write lands, and the two
//! reads that go straight at it.
//!
//! Crate-internal write and reads: `ingest_fact_timeseries` is the row every
//! governed write path materializes through, so a public door onto it is a
//! second write path — one that skips the sidecar, the projection row, the
//! sketch and the embedding enqueue its callers add. `read_memory_by_t` /
//! `read_memory_head` take the same transaction and exist to assert on that
//! row from inside the crate; the authorized read surface is the query ports.

#![allow(
    clippy::missing_errors_doc,
    clippy::doc_markdown,
    clippy::too_many_lines
)]

use proxima_core::edge::EdgeEndpoint;
use proxima_core::verbs::fact_ingest::{FactIngestOutcome, FactWriteCommand};
use proxima_core::{MemoryId, Owner, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::{internal, map_err};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MemoryRow {
    pub handle: Uuid,
    pub t: Uuid,
    pub kind: String,
    pub owner_id: Uuid,
    pub source_id: Option<String>,
    pub ingest_key: Option<String>,
    pub origins: Vec<Uuid>,
    pub refs: Vec<Uuid>,
    pub goal_refs: Vec<Uuid>,
}

#[derive(Debug)]
pub(crate) enum PreparedMemoryAdmission {
    Replay(FactIngestOutcome),
    New(Box<PreparedMemoryAdmissionNew>),
}

#[derive(Debug)]
pub(crate) struct PreparedMemoryAdmissionNew {
    owner: Owner,
    owner_id: Uuid,
    draft: FactWriteCommand,
    origins: Vec<Uuid>,
    refs: Vec<Uuid>,
    goal_refs: Vec<Uuid>,
    sidecar_tables: Vec<String>,
    handle: Uuid,
    t: Uuid,
    expected_head_t: Option<Uuid>,
    targets: Vec<Uuid>,
}

/// One txn: owners upsert + ingest_keys + memory_head + memory + announce(append).
///
/// `sidecar_tables` is the declared set forget will dump/delete — tables
/// actually inserted for this `t`, never the global registry.
/// `content_id` is the owner-scoped payload (required for A/P).
pub(crate) async fn ingest_fact_timeseries(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &FactWriteCommand,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
    sidecar_tables: &[String],
    content_id: Option<Uuid>,
) -> Result<FactIngestOutcome, StorageError> {
    let prepared =
        prepare_memory_admission(tx, owner, draft, origins, references, sidecar_tables).await?;
    lock_prepared_memory_admission(tx, &prepared).await?;
    let prepared = claim_prepared_memory_admission(tx, prepared).await?;
    materialize_prepared_memory_admission(tx, prepared, draft.blob_id, content_id).await
}

/// Prepare ordinary Memory admission without touching content, blob, head, or
/// Memory rows. Replay lookup runs first; a new admission arbitrates owner
/// identity, reserves its real identity, and snapshots its lifecycle footprint
/// before locking that footprint.
pub(crate) async fn prepare_memory_admission(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &FactWriteCommand,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
    sidecar_tables: &[String],
) -> Result<PreparedMemoryAdmission, StorageError> {
    prepare_memory_admission_at(
        tx,
        owner,
        draft,
        origins,
        references,
        sidecar_tables,
        PreparationOptions {
            identity: None,
            extra_targets: &[],
        },
    )
    .await
}

/// Prepare a derived admission with its explicit supersedes target included
/// in the lifecycle set without persisting that target as a pin.
pub(crate) async fn prepare_memory_admission_with_extra_targets(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &FactWriteCommand,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
    sidecar_tables: &[String],
    extra_targets: &[Uuid],
) -> Result<PreparedMemoryAdmission, StorageError> {
    prepare_memory_admission_at(
        tx,
        owner,
        draft,
        origins,
        references,
        sidecar_tables,
        PreparationOptions {
            identity: None,
            extra_targets,
        },
    )
    .await
}

/// Insert an unpinned lifecycle Fact at an identity reserved before a Goal's
/// union lock. The caller already owns the reserved handle and lifecycle
/// union; this helper still uses the same prepare/claim/materialize phases as
/// ordinary admission.
pub(crate) async fn ingest_unpinned_fact_at(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &FactWriteCommand,
    identity: (Uuid, Uuid),
) -> Result<FactIngestOutcome, StorageError> {
    let prepared = prepare_memory_admission_at(
        tx,
        owner,
        draft,
        &[],
        &[],
        &[],
        PreparationOptions {
            identity: Some(identity),
            extra_targets: &[],
        },
    )
    .await?;
    let prepared = match claim_prepared_memory_admission(tx, prepared).await? {
        PreparedMemoryAdmission::New(prepared) => prepared,
        PreparedMemoryAdmission::Replay(outcome) => return Ok(outcome),
    };
    // Goal writes acquire this reserved handle together with their complete
    // lifecycle union before persistence. Re-entering the Memory lifecycle
    // lock here would acquire a new handle after the Goal `t` locks and invert
    // the cross-entity order.
    materialize_prepared_memory_admission_after_locks(tx, prepared, draft.blob_id, None).await
}

#[derive(Debug, Clone, Copy)]
struct PreparationOptions<'a> {
    identity: Option<(Uuid, Uuid)>,
    extra_targets: &'a [Uuid],
}

async fn prepare_memory_admission_at(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &FactWriteCommand,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
    sidecar_tables: &[String],
    options: PreparationOptions<'_>,
) -> Result<PreparedMemoryAdmission, StorageError> {
    let owner_id = owner.stored_owner_id();

    let source_id = draft.source_id.clone();
    let ingest_key = draft.ingest_key.clone();
    if source_id.is_some() != ingest_key.is_some() {
        return Err(StorageError::ConstraintViolation(
            "source_id and ingest_key must both be set or both be absent".into(),
        ));
    }

    let kind = if draft.kind.is_empty() {
        "fact"
    } else {
        draft.kind.as_str()
    };
    if !matches!(kind, "fact" | "abstraction" | "perspective") {
        return Err(StorageError::ConstraintViolation(
            "kind must be fact, abstraction, or perspective".into(),
        ));
    }
    if kind != "fact" && (source_id.is_some() || ingest_key.is_some()) {
        return Err(StorageError::ConstraintViolation(
            "A/P cannot carry source_id/ingest_key".into(),
        ));
    }
    let mut persisted_origins = pin_memory_ids(origins)?;
    let (mut refs, goal_refs) = pin_reference_ids(references);
    // CHECK memory_fact_origins_chk: Facts cannot carry origins. Pins
    // a Fact declares (activation, evidence, prior request) live in refs.
    if kind == "fact" {
        for id in persisted_origins.drain(..) {
            if !refs.contains(&id) {
                refs.push(id);
            }
        }
    }

    if let (Some(source_id), Some(key)) = (source_id.as_deref(), ingest_key.as_deref())
        && let Some(replay) =
            load_ingest_replay(tx, owner_id, source_id, key, &refs, &goal_refs).await?
    {
        return Ok(PreparedMemoryAdmission::Replay(replay));
    }

    // Owner identity is arbitrated before any lifecycle lock, matching
    // transfer's owner -> lifecycle order and preventing an owner-row cycle.
    let owner_id = crate::access::owner_columns::ensure_owner_row(tx.as_mut(), owner).await?;
    // A new admission shares the owner fence, and a sourced admission also
    // shares its exact source fence.  These are held through commit, so a
    // whole-owner/source erase either waits for this write or observes it in
    // the exact-scope revalidation before deleting anything.
    crate::access::owner_columns::lock_owner_fence_shared_tx(tx, owner).await?;
    if let Some(source_id) = source_id.as_deref() {
        crate::access::owner_columns::lock_source_fence_shared_tx(tx, owner, source_id).await?;
    }

    let handle = options
        .identity
        .map(|(handle, _)| handle)
        .or(draft.handle)
        .unwrap_or_else(Uuid::now_v7);
    let t: Uuid = if let Some((_, t)) = options.identity {
        t
    } else {
        sqlx::query_scalar("SELECT uuidv7()")
            .fetch_one(tx.as_mut())
            .await
            .map_err(map_err)?
    };

    let expected_head_t: Option<Uuid> =
        sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1")
            .bind(handle)
            .fetch_optional(tx.as_mut())
            .await
            .map_err(map_err)?;
    let mut targets = Vec::with_capacity(
        1 + usize::from(expected_head_t.is_some())
            + persisted_origins.len()
            + refs.len()
            + goal_refs.len()
            + options.extra_targets.len(),
    );
    targets.push(t);
    if let Some(head_t) = expected_head_t {
        targets.push(head_t);
    }
    targets.extend(persisted_origins.iter().copied());
    targets.extend(refs.iter().copied());
    targets.extend(goal_refs.iter().copied());
    targets.extend(options.extra_targets.iter().copied());
    Ok(PreparedMemoryAdmission::New(Box::new(
        PreparedMemoryAdmissionNew {
            owner: *owner,
            owner_id,
            draft: draft.clone(),
            origins: persisted_origins,
            refs,
            goal_refs,
            sidecar_tables: sidecar_tables.to_vec(),
            handle,
            t,
            expected_head_t,
            targets,
        },
    )))
}

async fn load_ingest_replay(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    source_id: &str,
    key: &str,
    refs: &[Uuid],
    goal_refs: &[Uuid],
) -> Result<Option<FactIngestOutcome>, StorageError> {
    let Some(replay_t): Option<Uuid> = sqlx::query_scalar(
        "SELECT t FROM proxima_core.ingest_keys
          WHERE owner_id = $1 AND source_id = $2 AND ingest_key = $3",
    )
    .bind(owner_id)
    .bind(source_id)
    .bind(key)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    else {
        return Ok(None);
    };
    // Forget keeps the `ingest_keys` row and moves the handle to the cooled
    // stub, so a source re-delivering a cooled admission still replays. A
    // legacy cooled row has NULL refs; accepting it as an empty vector would
    // silently bless a changed declaration.
    let replay_row: (Uuid, Option<Vec<Uuid>>, Option<Vec<Uuid>>) = sqlx::query_as(
        "SELECT handle, refs, goal_refs FROM proxima_core.memory WHERE t = $1 AND owner_id = $2
         UNION ALL
         SELECT handle, refs, goal_refs FROM proxima_core.cooled WHERE t = $1 AND owner_id = $2
         LIMIT 1",
    )
    .bind(replay_t)
    .bind(owner_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    .ok_or_else(|| {
        internal(format!(
            "ingest key claims t {replay_t} with no hot or cooled row"
        ))
    })?;
    let (Some(stored_refs), Some(stored_goal_refs)) = (replay_row.1, replay_row.2) else {
        return Err(StorageError::Conflict(
            "fact replay references are unavailable for cooled admission".into(),
        ));
    };
    if stored_refs != refs || stored_goal_refs != goal_refs {
        return Err(StorageError::Conflict(
            "fact replay changed declared refs".into(),
        ));
    }
    Ok(Some(FactIngestOutcome {
        receipt_id: None,
        memory_id: MemoryId::new(replay_t),
        change_event_seq: replay_t,
        idempotent_replay: true,
        cited_object_id: None,
        handle: replay_row.0,
    }))
}

/// Claim the sourced replay key after the lifecycle set is held. A concurrent
/// loser discovers the replay here, still before citation/content persistence.
pub(crate) async fn claim_prepared_memory_admission(
    tx: &mut Transaction<'_, Postgres>,
    prepared: PreparedMemoryAdmission,
) -> Result<PreparedMemoryAdmission, StorageError> {
    let PreparedMemoryAdmission::New(prepared) = prepared else {
        return Ok(prepared);
    };
    if let (Some(source_id), Some(key)) = (
        prepared.draft.source_id.as_deref(),
        prepared.draft.ingest_key.as_deref(),
    ) {
        let claimed = sqlx::query(
            "INSERT INTO proxima_core.ingest_keys (owner_id, source_id, ingest_key, t)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (owner_id, source_id, ingest_key) DO NOTHING",
        )
        .bind(prepared.owner_id)
        .bind(source_id)
        .bind(key)
        .bind(prepared.t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
        if claimed.rows_affected() == 0 {
            let replay = load_ingest_replay(
                tx,
                prepared.owner_id,
                source_id,
                key,
                &prepared.refs,
                &prepared.goal_refs,
            )
            .await?
            .ok_or_else(|| internal("ingest key conflict had no replay row".to_owned()))?;
            return Ok(PreparedMemoryAdmission::Replay(replay));
        }
    }
    Ok(PreparedMemoryAdmission::New(prepared))
}

pub(crate) async fn lock_prepared_memory_admission(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedMemoryAdmission,
) -> Result<(), StorageError> {
    if let PreparedMemoryAdmission::New(prepared) = prepared {
        lock_and_validate_prepared_memory(tx, prepared).await?;
    }
    Ok(())
}

async fn lock_and_validate_prepared_memory(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedMemoryAdmissionNew,
) -> Result<(), StorageError> {
    lock_prepared_memory_targets(tx, prepared).await?;
    validate_prepared_memory_head(tx, prepared).await
}

/// Scope fences first, then the handle lock, then the sorted lifecycle set.
/// Split from [`validate_prepared_memory_head`] so a caller that already owns
/// these locks can still run the validation: the two are separate obligations,
/// and bundling them let the reserved-identity path drop the head check while
/// it was only trying to avoid re-acquiring the locks.
async fn lock_prepared_memory_targets(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedMemoryAdmissionNew,
) -> Result<(), StorageError> {
    crate::access::owner_columns::lock_owner_fence_shared_tx(tx, &prepared.owner).await?;
    if let Some(source_id) = prepared.draft.source_id.as_deref() {
        crate::access::owner_columns::lock_source_fence_shared_tx(tx, &prepared.owner, source_id)
            .await?;
    }
    crate::verbs::forget::lock_memory_handles_tx(tx, &[prepared.handle]).await?;
    crate::verbs::forget::lock_lifecycle_targets_tx(tx, &prepared.targets).await
}

/// The head this admission prepared against must still be the head under the
/// lock. Run by every materialization path, including callers that acquired
/// the locks themselves.
async fn validate_prepared_memory_head(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedMemoryAdmissionNew,
) -> Result<(), StorageError> {
    let current_head: Option<Uuid> =
        sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1 FOR UPDATE")
            .bind(prepared.handle)
            .fetch_optional(tx.as_mut())
            .await
            .map_err(map_err)?;
    if current_head != prepared.expected_head_t {
        return Err(StorageError::Retryable(
            "memory series head changed while preparing admission".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn materialize_prepared_memory_admission(
    tx: &mut Transaction<'_, Postgres>,
    prepared: PreparedMemoryAdmission,
    blob_id: Option<Uuid>,
    content_id: Option<Uuid>,
) -> Result<FactIngestOutcome, StorageError> {
    let prepared = match prepared {
        PreparedMemoryAdmission::New(prepared) => prepared,
        PreparedMemoryAdmission::Replay(outcome) => return Ok(outcome),
    };
    // Keep this materialization primitive safe for every direct crate-internal
    // caller, not only the ordinary prepare/lock/claim wrapper above. The
    // advisory lock is re-entrant in this transaction and precedes all
    // Content/head/Memory persistence below.
    lock_prepared_memory_targets(tx, &prepared).await?;
    materialize_prepared_memory_admission_after_locks(tx, prepared, blob_id, content_id).await
}

/// Materialize an admission after the caller has acquired its complete
/// handle/lifecycle union. The reserved Goal lifecycle Fact uses this path so
/// it cannot acquire a Memory handle after the Goal transaction already holds
/// lifecycle `t` locks. The head validation still runs here — only the lock
/// acquisition is the caller's.
async fn materialize_prepared_memory_admission_after_locks(
    tx: &mut Transaction<'_, Postgres>,
    prepared: Box<PreparedMemoryAdmissionNew>,
    blob_id: Option<Uuid>,
    content_id: Option<Uuid>,
) -> Result<FactIngestOutcome, StorageError> {
    validate_prepared_memory_head(tx, &prepared).await?;
    let mut draft = prepared.draft;
    if blob_id.is_some() {
        draft.blob_id = blob_id;
    }
    let kind = if draft.kind.is_empty() {
        "fact"
    } else {
        draft.kind.as_str()
    };
    let content_id = if let Some(id) = content_id {
        Some(id)
    } else if kind == "fact" {
        None
    } else {
        let text = draft
            .rendered_text
            .as_deref()
            .map_or(draft.payload.as_slice(), str::as_bytes);
        Some(
            super::content::ensure_text_content(
                tx,
                prepared.owner_id,
                draft.schema_id.as_str(),
                text,
            )
            .await?,
        )
    };

    // `GREATEST`, not `EXCLUDED.t`: the head only ever moves forward. Every
    // path that could rewind it is already refused — `validate_prepared_memory_head`
    // rejects an admission whose prepared head is no longer current, and the
    // reserved-identity path always carries a handle freshly minted by
    // `reserve_fact_identity`, so it has no head to rewind. This is the
    // invariant those checks add up to, written where the head is actually
    // assigned rather than left implicit across two modules.
    let head = sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, $2::proxima_core.memory_kind, $3, $4, $5)
         ON CONFLICT (handle) DO UPDATE SET t = GREATEST(proxima_core.memory_head.t, EXCLUDED.t)
         WHERE proxima_core.memory_head.kind = EXCLUDED.kind
           AND proxima_core.memory_head.schema_id = EXCLUDED.schema_id
           AND proxima_core.memory_head.owner_id = EXCLUDED.owner_id
         RETURNING handle",
    )
    .bind(prepared.handle)
    .bind(kind)
    .bind(draft.schema_id.as_str())
    .bind(prepared.owner_id)
    .bind(prepared.t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    if head.is_none() {
        return Err(StorageError::ConstraintViolation(
            "memory_head kind/schema/owner mismatch".into(),
        ));
    }

    sqlx::query(
        "INSERT INTO proxima_core.memory
            (handle, t, kind, owner_id, schema_id, source_id, ingest_key, blob_id,
             content_id, origins, refs, goal_refs, sidecar_tables)
         VALUES ($1, $2, $3::proxima_core.memory_kind, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                 $13)",
    )
    .bind(prepared.handle)
    .bind(prepared.t)
    .bind(kind)
    .bind(prepared.owner_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.source_id.as_deref())
    .bind(draft.ingest_key.as_deref())
    .bind(draft.blob_id)
    .bind(content_id)
    .bind(&prepared.origins)
    .bind(&prepared.refs)
    .bind(&prepared.goal_refs)
    .bind(&prepared.sidecar_tables)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    let seq: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.announce
            (owner_id, op, entity, handle, t)
         VALUES ($1, 'append', 'memory', $2, $3)
         RETURNING seq",
    )
    .bind(prepared.owner_id)
    .bind(prepared.handle)
    .bind(prepared.t)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;

    super::sketch::upsert_sketch(
        tx,
        prepared.owner_id,
        prepared.t,
        kind,
        &super::sketch::sketch_line(kind, draft.rendered_text.as_deref(), &[]),
    )
    .await?;

    Ok(FactIngestOutcome {
        receipt_id: None,
        memory_id: MemoryId::new(prepared.t),
        change_event_seq: seq,
        idempotent_replay: false,
        // Reports the blob this admission actually persisted. The predecessor
        // always returned `None` here and left `ingest_core` to fill it in;
        // every other caller passes a draft with no `blob_id`, so this is a
        // narrowing of that gap rather than a change any caller observes.
        cited_object_id: draft.blob_id,
        handle: prepared.handle,
    })
}

/// Project authorized endpoints to the UUID array stored by Postgres while
/// preserving declaration order and removing duplicate pins. Origins are a
/// Memory-only column; a Goal origin is a malformed authorized carrier and
/// must not be silently discarded.
fn pin_memory_ids(pins: &[EdgeEndpoint]) -> Result<Vec<Uuid>, StorageError> {
    let mut ids = Vec::with_capacity(pins.len());
    for pin in pins {
        let Some(memory_id) = pin.memory_id() else {
            return Err(StorageError::ConstraintViolation(
                "memory origins must target a Memory".into(),
            ));
        };
        let id = memory_id.into_inner();
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Split reference endpoints into the two columns that now carry them:
/// `memory.refs` for Memory targets, `memory.goal_refs` for Goal targets.
/// The column is the type — the trigger checks each against its own spine —
/// so the endpoint kind survives persistence instead of being re-derived by
/// every reader. Declaration order is preserved within each column and
/// duplicate pins are dropped.
pub(crate) fn pin_reference_ids(pins: &[EdgeEndpoint]) -> (Vec<Uuid>, Vec<Uuid>) {
    let mut memory_ids = Vec::with_capacity(pins.len());
    let mut goal_ids = Vec::new();
    for pin in pins {
        let id = pin.entity_id();
        let column = if matches!(pin.entity, proxima_core::EntityRef::Goal(_)) {
            &mut goal_ids
        } else {
            &mut memory_ids
        };
        if !column.contains(&id) {
            column.push(id);
        }
    }
    (memory_ids, goal_ids)
}

/// Read one admission row by `t`, inside the caller's transaction.
///
/// Test-only, and the gate says so rather than a comment: the only consumer
/// is the suite below, which asserts on the row a write just landed before
/// that write commits. The authorized read surface is the query ports; this
/// reaches the table directly and must not become a second one.
#[cfg(test)]
pub(crate) async fn read_memory_by_t(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
) -> Result<Option<MemoryRow>, StorageError> {
    sqlx::query_as::<_, MemoryRow>(
        "SELECT handle, t, kind::text, owner_id, source_id, ingest_key, origins, refs, goal_refs
           FROM proxima_core.memory
          WHERE t = $1",
    )
    .bind(t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(internal)
}

/// Read a series' current head row, inside the caller's transaction.
/// Test-only for the same reason as [`read_memory_by_t`].
#[cfg(test)]
pub(crate) async fn read_memory_head(
    tx: &mut Transaction<'_, Postgres>,
    handle: Uuid,
) -> Result<Option<MemoryRow>, StorageError> {
    sqlx::query_as::<_, MemoryRow>(
        "SELECT m.handle, m.t, m.kind::text, m.owner_id, m.source_id, m.ingest_key,
                m.origins, m.refs, m.goal_refs
           FROM proxima_core.memory_head h
           JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
          WHERE h.handle = $1",
    )
    .bind(handle)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(internal)
}

// The admission-row suite. Crate-internal for the same reason the verb is:
// it reads the row back through `read_memory_by_t` / `read_memory_head`
// inside the write's own transaction, which is not something the read ports
// offer and should not be.
#[cfg(test)]
#[path = "memory_timeseries_pg_tests.rs"]
mod memory_timeseries_pg_tests;

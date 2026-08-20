// Five erase entry points take (pool, owner parts, source scope, audit
// context, authorization) — every argument is a distinct authority the erase
// has to be handed, and bundling them into a struct would only move the arity
// to its constructor. `too_many_lines` is narrowed to the two functions that
// earn it, below.
#![allow(clippy::too_many_arguments)]

use proxima_core::compliance::{
    ComplianceAuditContext, ComplianceEraseCounts, ComplianceEraseOutcome, ComplianceEraseRefusal,
    ComplianceEraseTarget, ComplianceSidecarTables, EraseAuthorization,
};
use proxima_core::{ColdObjectStore, GroupId, OwnerRef, SourceId, StorageError, UserId};
use sqlx::{PgPool, Postgres, Transaction};

use crate::access::owner_columns::{lock_group_membership_tx, owner_binds};
use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::verbs::fact_retention::{legal_hold_active_tx, lock_legal_hold_tx};
use crate::verbs::forget::ColdPurgePlan;

type Tx<'a> = Transaction<'a, Postgres>;

#[derive(Debug, Clone, Copy)]
enum SelectionScope<'a> {
    Owner,
    Source(&'a SourceId),
}

/// Begin a bulk-erase transaction: disable the pool's request-serving
/// `statement_timeout` — a full-owner Art. 17 erase DELETEs across every
/// owner-scoped table and can legitimately run longer than the request bound;
/// `SET LOCAL` scopes the override to this transaction only) and defer
/// constraint checks until commit.
async fn begin_bulk_erase_tx(pool: &PgPool) -> Result<Tx<'_>, StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
    sqlx::query("SET LOCAL statement_timeout = 0")
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    sqlx::query("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    Ok(tx)
}

pub async fn record_compliance_outcome(
    pool: &PgPool,
    audit: &ComplianceAuditContext,
    outcome: &ComplianceEraseOutcome,
) -> Result<(), StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
    upsert_audit_outcome(&mut tx, audit, outcome, ComplianceEraseCounts::default()).await?;
    tx.commit().await.map_err(map_err)
}

/// Clear the durable purge-pending flag after a cited-object purge has been
/// confirmed to succeed. A single-statement `UPDATE`, deliberately outside
/// any erase transaction: the purge itself runs post-commit in the engine
/// (see `Engine::finalize_owner_erase_with_object_purge`), so clearing the
/// flag is a separate, later write against the already-committed audit row.
pub async fn clear_cited_object_purge_pending(
    pool: &PgPool,
    operation_id: uuid::Uuid,
) -> Result<(), StorageError> {
    sqlx::query(
        "UPDATE proxima_core.compliance_audit_log
            SET cited_object_purge_pending = false
          WHERE operation_id = $1",
    )
    .bind(operation_id)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

pub async fn erase_group_owner_if_abandoned(
    pool: &PgPool,
    cold: &dyn ColdObjectStore,
    auth: &EraseAuthorization,
    group_id: GroupId,
    object_purge_planned: bool,
    tables: &ComplianceSidecarTables,
) -> Result<ComplianceEraseOutcome, StorageError> {
    let owner = OwnerRef::Group(group_id);
    let mut tx = begin_bulk_erase_tx(pool).await?;
    if let Some(outcome) = refuse_if_legal_hold_active(&mut tx, auth, owner).await? {
        tx.commit().await.map_err(map_err)?;
        return Ok(outcome);
    }
    lock_group_membership_tx(&mut tx, group_id).await?;
    if group_member_count(&mut tx, group_id).await? > 0 {
        let outcome = refused(auth, ComplianceEraseRefusal::OwnerNotAbandoned);
        upsert_audit_outcome(
            &mut tx,
            auth.audit(),
            &outcome,
            ComplianceEraseCounts::default(),
        )
        .await?;
        tx.commit().await.map_err(map_err)?;
        return Ok(outcome);
    }
    let cold_purge = erase_selected(&mut tx, auth, owner, SelectionScope::Owner, tables).await?;
    let counts = final_counts(&mut tx).await?;
    let outcome = ComplianceEraseOutcome::Completed {
        operation_id: auth.audit().operation_id(),
        counts,
        cited_object_purge_pending: object_purge_planned,
        cold_object_purge_pending: !cold_purge.is_empty(),
    };
    upsert_audit_outcome(&mut tx, auth.audit(), &outcome, counts).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(finalize_cold_purge(pool, cold, &cold_purge, outcome).await)
}

pub async fn erase_personal_owner_if_drop_verified(
    pool: &PgPool,
    cold: &dyn ColdObjectStore,
    auth: &EraseAuthorization,
    user_id: UserId,
    object_purge_planned: bool,
    tables: &ComplianceSidecarTables,
) -> Result<ComplianceEraseOutcome, StorageError> {
    let owner = OwnerRef::Personal(user_id);
    let mut tx = begin_bulk_erase_tx(pool).await?;
    if let Some(outcome) = refuse_if_legal_hold_active(&mut tx, auth, owner).await? {
        tx.commit().await.map_err(map_err)?;
        return Ok(outcome);
    }
    let cold_purge = erase_selected(&mut tx, auth, owner, SelectionScope::Owner, tables).await?;
    let counts = final_counts(&mut tx).await?;
    let outcome = ComplianceEraseOutcome::Completed {
        operation_id: auth.audit().operation_id(),
        counts,
        cited_object_purge_pending: object_purge_planned,
        cold_object_purge_pending: !cold_purge.is_empty(),
    };
    upsert_audit_outcome(&mut tx, auth.audit(), &outcome, counts).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(finalize_cold_purge(pool, cold, &cold_purge, outcome).await)
}

pub async fn erase_group_source_scope_if_owner_abandoned(
    pool: &PgPool,
    cold: &dyn ColdObjectStore,
    auth: &EraseAuthorization,
    group_id: GroupId,
    source_id: &SourceId,
    tables: &ComplianceSidecarTables,
) -> Result<ComplianceEraseOutcome, StorageError> {
    let owner = OwnerRef::Group(group_id);
    let mut tx = begin_bulk_erase_tx(pool).await?;
    if let Some(outcome) = refuse_if_legal_hold_active(&mut tx, auth, owner).await? {
        tx.commit().await.map_err(map_err)?;
        return Ok(outcome);
    }
    lock_group_membership_tx(&mut tx, group_id).await?;
    if group_member_count(&mut tx, group_id).await? > 0 {
        let outcome = refused(auth, ComplianceEraseRefusal::SourceScopeOwnerStillLive);
        upsert_audit_outcome(
            &mut tx,
            auth.audit(),
            &outcome,
            ComplianceEraseCounts::default(),
        )
        .await?;
        tx.commit().await.map_err(map_err)?;
        return Ok(outcome);
    }
    let cold_purge = erase_selected(
        &mut tx,
        auth,
        owner,
        SelectionScope::Source(source_id),
        tables,
    )
    .await?;
    let counts = final_counts(&mut tx).await?;
    let outcome = ComplianceEraseOutcome::Completed {
        operation_id: auth.audit().operation_id(),
        counts,
        cited_object_purge_pending: false,
        cold_object_purge_pending: !cold_purge.is_empty(),
    };
    upsert_audit_outcome(&mut tx, auth.audit(), &outcome, counts).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(finalize_cold_purge(pool, cold, &cold_purge, outcome).await)
}

pub async fn erase_personal_source_scope_if_drop_verified(
    pool: &PgPool,
    cold: &dyn ColdObjectStore,
    auth: &EraseAuthorization,
    user_id: UserId,
    source_id: &SourceId,
    tables: &ComplianceSidecarTables,
) -> Result<ComplianceEraseOutcome, StorageError> {
    let owner = OwnerRef::Personal(user_id);
    let mut tx = begin_bulk_erase_tx(pool).await?;
    if let Some(outcome) = refuse_if_legal_hold_active(&mut tx, auth, owner).await? {
        tx.commit().await.map_err(map_err)?;
        return Ok(outcome);
    }
    let cold_purge = erase_selected(
        &mut tx,
        auth,
        owner,
        SelectionScope::Source(source_id),
        tables,
    )
    .await?;
    let counts = final_counts(&mut tx).await?;
    let outcome = ComplianceEraseOutcome::Completed {
        operation_id: auth.audit().operation_id(),
        counts,
        cited_object_purge_pending: false,
        cold_object_purge_pending: !cold_purge.is_empty(),
    };
    upsert_audit_outcome(&mut tx, auth.audit(), &outcome, counts).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(finalize_cold_purge(pool, cold, &cold_purge, outcome).await)
}

async fn finalize_cold_purge(
    pool: &PgPool,
    cold: &dyn ColdObjectStore,
    plan: &ColdPurgePlan,
    outcome: ComplianceEraseOutcome,
) -> ComplianceEraseOutcome {
    let purge = super::forget::purge_cold_objects_after_commit(pool, cold, plan).await;
    let ComplianceEraseOutcome::Completed {
        operation_id,
        counts,
        cited_object_purge_pending,
        ..
    } = outcome
    else {
        return outcome;
    };
    ComplianceEraseOutcome::Completed {
        operation_id,
        counts,
        cited_object_purge_pending,
        cold_object_purge_pending: purge.pending,
    }
}

async fn group_member_count(tx: &mut Tx<'_>, group_id: GroupId) -> Result<i64, StorageError> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.group_memberships WHERE group_id = $1",
    )
    .bind(group_id.into_inner())
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)
}

async fn refuse_if_legal_hold_active(
    tx: &mut Tx<'_>,
    auth: &EraseAuthorization,
    owner: OwnerRef,
) -> Result<Option<ComplianceEraseOutcome>, StorageError> {
    lock_legal_hold_tx(tx, &owner).await?;
    if !legal_hold_active_tx(tx, &owner).await? {
        return Ok(None);
    }
    let outcome = refused(auth, ComplianceEraseRefusal::LegalHoldActive);
    upsert_audit_outcome(tx, auth.audit(), &outcome, ComplianceEraseCounts::default()).await?;
    Ok(Some(outcome))
}

async fn erase_selected(
    tx: &mut Tx<'_>,
    auth: &EraseAuthorization,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
    tables: &ComplianceSidecarTables,
) -> Result<ColdPurgePlan, StorageError> {
    open_erase_bookkeeping(tx, auth, owner, scope).await?;

    let delegated_authority_grants = delete_delegated_authority_grants(tx, owner, scope).await?;
    record_count(tx, "delegated_authority_grants", delegated_authority_grants).await?;

    record_count(tx, "edges", 0).await?;

    let change_events = delete_change_events(tx, owner).await?;
    record_count(tx, "change_events", change_events).await?;

    let source_cursors = delete_source_cursors(tx, owner, scope).await?;
    record_count(tx, "source_cursors", source_cursors).await?;

    delete_ingest_keys(tx).await?;
    delete_goal_refs(tx);
    delete_memory_refs(tx);

    // These two sweeps are the whole sidecar story. There used to be a
    // second, hardcoded pass underneath each of them —
    // `delete_fixed_goal_sidecars` naming `task_goal_v1`, and
    // `delete_fixed_memory_sidecars` naming `agent_derivation_v1`,
    // `agent_note_v1` and `utterance_v1`. All four are registered schemas,
    // so the registry pass already deleted their rows and the fixed pass
    // deleted nothing, every time. What it did instead was hide the failure
    // mode it looked like insurance against: a core table that fell out of
    // the registry would have kept working here and gone missing everywhere
    // else. `the_registry_pass_reaches_every_core_sidecar` pins the set.
    let mut sidecar_rows =
        delete_dynamic_sidecars(tx, tables.goal.as_slice(), "t", "selected_goals", "goal_id")
            .await?;
    // Owner-pinned sidecars are held out of the Memory-keyed sweep: their
    // rows do not follow a transfer, so `selected_memories` is the wrong
    // set for them in both directions. They are erased below, by their own
    // `owner_id`.
    let memory_keyed_fact_tables = tables
        .fact
        .as_slice()
        .iter()
        .filter(|table| !tables.owner_pinned.as_slice().contains(table))
        .cloned()
        .collect::<Vec<_>>();
    sidecar_rows += delete_dynamic_sidecars(
        tx,
        &memory_keyed_fact_tables,
        "t",
        "selected_memories",
        "memory_id",
    )
    .await?;

    let sketches = sqlx::query(
        "DELETE FROM proxima_core.sketch s
          WHERE EXISTS (SELECT 1 FROM selected_memories sm WHERE sm.memory_id = s.t)
             OR EXISTS (SELECT 1 FROM selected_goals sg WHERE sg.goal_id = s.t)",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?
    .rows_affected();
    record_count(tx, "sketches", sketches).await?;

    let embedding_jobs = delete_embeddings(tx, "proxima_core.embedding_jobs").await?;
    record_count(tx, "embedding_jobs", embedding_jobs).await?;
    let embedding_heads = delete_embeddings(tx, "proxima_core.embedding_heads").await?;
    let embeddings = delete_embeddings(tx, "proxima_core.embeddings").await?;
    record_count(tx, "embeddings", embeddings.saturating_add(embedding_heads)).await?;

    let mcp_rows =
        delete_owner_pinned_sidecars(tx, tables.owner_pinned.as_slice(), owner, scope).await?;
    record_count(tx, "mcp_call_rows", mcp_rows).await?;

    let content_ids = selected_content_ids(tx).await?;
    let memories = delete_selected_table(
        tx,
        "proxima_core.memory",
        "t",
        "selected_memories",
        "memory_id",
    )
    .await?;
    let operation_id = auth.audit().operation_id();
    let (cooled, cold_purge) = delete_selected_cooled(tx, operation_id).await?;
    for id in content_ids {
        super::content::gc_unreferenced_content(tx, id).await?;
    }
    record_count(tx, "memories", memories.saturating_add(cooled)).await?;
    let goals =
        delete_selected_table(tx, "proxima_core.goal", "t", "selected_goals", "goal_id").await?;
    record_count(tx, "goals", goals).await?;
    let wake_configs = delete_wake_configs(tx, owner, scope).await?;
    record_count(tx, "wake_configs", wake_configs).await?;
    let blobs = delete_blobs(tx, owner, scope, operation_id, tables).await?;
    sidecar_rows += blobs.sidecar_rows;
    record_count(tx, "sidecar_rows", sidecar_rows).await?;
    record_count(tx, "blob_uploads", blobs.uploads).await?;
    record_count(tx, "blobs", blobs.blobs).await?;
    sync_selected_heads(tx).await?;
    let receipts = 0;
    record_count(tx, "receipts", receipts).await?;
    let source_batches = 0;
    record_count(tx, "source_batches", source_batches).await?;
    let mut object_keys = cold_purge.object_keys().to_vec();
    object_keys.extend_from_slice(blobs.cold_purge.object_keys());
    object_keys.sort_unstable();
    object_keys.dedup();
    Ok(ColdPurgePlan::from_keys(object_keys))
}

/// Build the selection sets, stamp the in-progress (`Refused`) audit row that a
/// crash mid-erase leaves behind, and open the per-transaction count table the
/// deletions below tally into.
async fn open_erase_bookkeeping(
    tx: &mut Tx<'_>,
    auth: &EraseAuthorization,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
) -> Result<(), StorageError> {
    create_selected_sets(tx, owner, scope).await?;
    capture_selected_handles(tx).await?;
    upsert_audit_outcome(
        tx,
        auth.audit(),
        &ComplianceEraseOutcome::Refused {
            operation_id: auth.audit().operation_id(),
            reason: ComplianceEraseRefusal::OwnerNotAbandoned,
        },
        ComplianceEraseCounts::default(),
    )
    .await?;
    let redactions = insert_redactions(tx, auth.audit().operation_id());
    sqlx::query("CREATE TEMP TABLE compliance_counts(name text PRIMARY KEY, count bigint NOT NULL) ON COMMIT DROP")
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    record_count(tx, "redacted_edge_targets", redactions).await?;
    record_count(tx, "suppressed_keys", 0).await
}

async fn create_selected_sets(
    tx: &mut Tx<'_>,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
) -> Result<(), StorageError> {
    // `owner_id` is NOT NULL on every owned table and `OwnerRef` has no
    // id-less kind, so `owner_id` binds non-NULL here and in every erase
    // statement below: plain `=` is exactly `IS NOT DISTINCT FROM` while
    // staying an index condition (`PostgreSQL` has no index strategy for
    // DistinctExpr).
    let (owner_kind, owner_id) = owner_binds(&owner);

    sqlx::query("CREATE TEMP TABLE selected_memories(memory_id uuid PRIMARY KEY, kind text NOT NULL) ON COMMIT DROP")
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    match scope {
        SelectionScope::Owner => {
            sqlx::query(
                "INSERT INTO selected_memories(memory_id, kind)
                 SELECT t, kind::text
                   FROM proxima_core.memory
                  WHERE owner_id = $1
                 UNION ALL
                 SELECT t, kind::text
                   FROM proxima_core.cooled
                  WHERE owner_id = $1",
            )
            .bind(owner_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
        }
        SelectionScope::Source(source_id) => {
            sqlx::query(
                "INSERT INTO selected_memories(memory_id, kind)
                 SELECT m.t, m.kind::text
                   FROM proxima_core.memory m
                  WHERE m.owner_id = $1
                    AND m.source_id = $2
                 UNION ALL
                 SELECT c.t, c.kind::text
                   FROM proxima_core.cooled c
                  WHERE c.owner_id = $1
                    AND c.source_id = $2",
            )
            .bind(owner_id)
            .bind(source_id.as_str())
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
        }
    }

    sqlx::query("CREATE TEMP TABLE selected_blobs(blob_id uuid PRIMARY KEY) ON COMMIT DROP")
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    match scope {
        SelectionScope::Owner => {
            sqlx::query(
                "INSERT INTO selected_blobs(blob_id)
                 SELECT blob_id FROM proxima_core.blob WHERE owner_id = $1",
            )
            .bind(owner_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
        }
        SelectionScope::Source(_) => {
            sqlx::query(
                "INSERT INTO selected_blobs(blob_id)
                 SELECT blob_id FROM (
                     SELECT m.blob_id
                       FROM proxima_core.memory m
                       JOIN selected_memories sm ON sm.memory_id = m.t
                      WHERE m.blob_id IS NOT NULL
                     UNION
                     SELECT c.blob_id
                       FROM proxima_core.cooled c
                       JOIN selected_memories sm ON sm.memory_id = c.t
                      WHERE c.blob_id IS NOT NULL
                 ) selected",
            )
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
        }
    }

    sqlx::query("CREATE TEMP TABLE selected_goals(goal_id uuid PRIMARY KEY) ON COMMIT DROP")
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    if matches!(scope, SelectionScope::Owner) {
        sqlx::query(
            "INSERT INTO selected_goals(goal_id)
             SELECT t FROM proxima_core.goal
              WHERE owner_id = $2",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    }

    Ok(())
}

async fn capture_selected_handles(tx: &mut Tx<'_>) -> Result<(), StorageError> {
    sqlx::query(
        "CREATE TEMP TABLE selected_memory_handles(handle uuid PRIMARY KEY) ON COMMIT DROP",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "INSERT INTO selected_memory_handles(handle)
         SELECT DISTINCT handle FROM (
             SELECT m.handle
               FROM proxima_core.memory m
               JOIN selected_memories sm ON sm.memory_id = m.t
             UNION
             SELECT c.handle
               FROM proxima_core.cooled c
               JOIN selected_memories sm ON sm.memory_id = c.t
         ) h",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query("CREATE TEMP TABLE selected_goal_handles(handle uuid PRIMARY KEY) ON COMMIT DROP")
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    sqlx::query(
        "INSERT INTO selected_goal_handles(handle)
         SELECT DISTINCT g.handle
           FROM proxima_core.goal g
           JOIN selected_goals sg ON sg.goal_id = g.t",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn sync_selected_heads(tx: &mut Tx<'_>) -> Result<(), StorageError> {
    sqlx::query(
        "UPDATE proxima_core.memory_head h
            SET t = r.t
           FROM (
                SELECT handle, t
                  FROM (
                    SELECT handle, t,
                           row_number() OVER (PARTITION BY handle ORDER BY t DESC) AS n
                      FROM proxima_core.memory
                     WHERE handle IN (SELECT handle FROM selected_memory_handles)
                  ) ranked
                 WHERE n = 1
           ) r
          WHERE h.handle = r.handle",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "DELETE FROM proxima_core.memory_head h
          WHERE h.handle IN (SELECT handle FROM selected_memory_handles)
            AND NOT EXISTS (
                SELECT 1 FROM proxima_core.memory m WHERE m.handle = h.handle
            )",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "UPDATE proxima_core.goal_head h
            SET t = r.t
           FROM (
                SELECT handle, t
                  FROM (
                    SELECT handle, t,
                           row_number() OVER (PARTITION BY handle ORDER BY t DESC) AS n
                      FROM proxima_core.goal
                     WHERE handle IN (SELECT handle FROM selected_goal_handles)
                  ) ranked
                 WHERE n = 1
           ) r
          WHERE h.handle = r.handle",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "DELETE FROM proxima_core.goal_head h
          WHERE h.handle IN (SELECT handle FROM selected_goal_handles)
            AND NOT EXISTS (
                SELECT 1 FROM proxima_core.goal g WHERE g.handle = h.handle
            )",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn delete_ingest_keys(tx: &mut Tx<'_>) -> Result<(), StorageError> {
    sqlx::query(
        "DELETE FROM proxima_core.ingest_keys k
          WHERE EXISTS (
                SELECT 1
                  FROM selected_memories sm
                 WHERE sm.memory_id = k.t
          )",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

fn insert_redactions(_tx: &mut Tx<'_>, _operation_id: uuid::Uuid) -> u64 {
    0
}

fn digest_bytes(domain: &str, parts: &[&[u8]]) -> Vec<u8> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proxima-compliance-audit-v1");
    hasher.update(b"\0");
    hasher.update(domain.as_bytes());
    for part in parts {
        hasher.update(b"\0");
        hasher.update(part);
    }
    hasher.finalize().as_bytes().to_vec()
}

async fn upsert_audit_outcome(
    tx: &mut Tx<'_>,
    audit: &ComplianceAuditContext,
    outcome: &ComplianceEraseOutcome,
    counts: ComplianceEraseCounts,
) -> Result<(), StorageError> {
    let (target_kind, owner, source_scope) = audit_target(audit.target());
    let owner_digest = owner_digest(owner);
    let requester_digest = audit
        .derived_requester()
        .map(|user| digest_bytes("requester", &[user.into_inner().as_bytes()]));
    let source_scope_digest = source_scope.map(|source| {
        digest_bytes(
            "source_scope",
            &[
                owner.stable_key_uuid().as_bytes(),
                source.as_str().as_bytes(),
            ],
        )
    });
    let (outcome_name, refusal) = outcome_labels(outcome);
    let (cold_purge_pending, cited_purge_pending) = outcome_purge_pending(outcome);
    sqlx::query(
        "INSERT INTO proxima_core.compliance_audit_log(
             operation_id, target_kind, outcome, refusal, owner_ref_digest,
             requester_digest, source_scope_digest, derived_auth_path, requested_at,
             completed_at, memories_count, goals_count, wake_configs_count,
             blobs_count, blob_uploads_count, sidecar_rows_count, edges_count,
             receipts_count, source_batches_count,
             source_cursors_count, embeddings_count, embedding_jobs_count, mcp_call_rows_count,
             change_events_count, redacted_edge_targets_count, suppressed_keys_count,
             delegated_authority_grants_count, cold_object_purge_pending,
             cited_object_purge_pending)
         VALUES ($1, $2, $3::proxima_core.compliance_erase_outcome,
                 $4::proxima_core.compliance_erase_refusal, $5, $6, $7, $8, $9,
                 now(), $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23,
                 $24, $25, $26, $27, $28)
         ON CONFLICT (operation_id) DO UPDATE SET
             outcome = EXCLUDED.outcome,
             refusal = EXCLUDED.refusal,
             completed_at = EXCLUDED.completed_at,
             memories_count = EXCLUDED.memories_count,
             goals_count = EXCLUDED.goals_count,
             wake_configs_count = EXCLUDED.wake_configs_count,
             blobs_count = EXCLUDED.blobs_count,
             blob_uploads_count = EXCLUDED.blob_uploads_count,
             sidecar_rows_count = EXCLUDED.sidecar_rows_count,
             edges_count = EXCLUDED.edges_count,
             receipts_count = EXCLUDED.receipts_count,
             source_batches_count = EXCLUDED.source_batches_count,
             source_cursors_count = EXCLUDED.source_cursors_count,
             embeddings_count = EXCLUDED.embeddings_count,
             embedding_jobs_count = EXCLUDED.embedding_jobs_count,
             mcp_call_rows_count = EXCLUDED.mcp_call_rows_count,
             change_events_count = EXCLUDED.change_events_count,
             redacted_edge_targets_count = EXCLUDED.redacted_edge_targets_count,
             suppressed_keys_count = EXCLUDED.suppressed_keys_count,
             delegated_authority_grants_count = EXCLUDED.delegated_authority_grants_count,
             cold_object_purge_pending = EXCLUDED.cold_object_purge_pending,
             cited_object_purge_pending = EXCLUDED.cited_object_purge_pending",
    )
    .bind(audit.operation_id())
    .bind(target_kind)
    .bind(outcome_name)
    .bind(refusal)
    .bind(owner_digest)
    .bind(requester_digest)
    .bind(source_scope_digest)
    .bind(format!("{:?}", audit.derived_auth_path()))
    .bind(audit.requested_at())
    .bind(i64::try_from(counts.memories).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.goals).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.wake_configs).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.blobs).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.blob_uploads).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.sidecar_rows).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.edges).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.receipts).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.source_batches).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.source_cursors).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.embeddings).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.embedding_jobs).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.mcp_call_rows).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.change_events).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.redacted_edge_targets).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.suppressed_keys).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.delegated_authority_grants).unwrap_or(i64::MAX))
    .bind(cold_purge_pending)
    .bind(cited_purge_pending)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

fn audit_target(target: &ComplianceEraseTarget) -> (&'static str, OwnerRef, Option<&SourceId>) {
    match target {
        ComplianceEraseTarget::GroupOwner { group_id } => {
            ("GroupOwner", OwnerRef::Group(*group_id), None)
        }
        ComplianceEraseTarget::PersonalOwner { user_id, .. } => {
            ("PersonalOwner", OwnerRef::Personal(*user_id), None)
        }
        ComplianceEraseTarget::GroupSourceScope {
            group_id,
            source_id,
        } => (
            "GroupSourceScope",
            OwnerRef::Group(*group_id),
            Some(source_id),
        ),
        ComplianceEraseTarget::PersonalSourceScope {
            user_id, source_id, ..
        } => (
            "PersonalSourceScope",
            OwnerRef::Personal(*user_id),
            Some(source_id),
        ),
    }
}

pub(crate) fn owner_digest(owner: OwnerRef) -> Vec<u8> {
    let (kind, owner_id) = owner.columns();
    let stable_key = owner.stable_key_uuid();
    let parts: Vec<&[u8]> = vec![
        kind.as_str().as_bytes(),
        stable_key.as_bytes(),
        owner_id.as_bytes(),
    ];
    digest_bytes("owner", &parts)
}

fn outcome_labels(outcome: &ComplianceEraseOutcome) -> (&'static str, Option<&'static str>) {
    match outcome {
        ComplianceEraseOutcome::Completed { .. } => ("Completed", None),
        ComplianceEraseOutcome::Refused { reason, .. } => ("Refused", Some(refusal_label(reason))),
        ComplianceEraseOutcome::NotFound { .. } => ("NotFound", None),
        ComplianceEraseOutcome::Unauthorized { .. } => ("Unauthorized", None),
    }
}

/// The durable purge-pending flag mirrors the outcome's own field: only a
/// `Completed` erase can have a cited-object purge outstanding, and every
/// other outcome (refused/not-found/unauthorized) never touched an object
/// store, so it is always `false`.
fn outcome_purge_pending(outcome: &ComplianceEraseOutcome) -> (bool, bool) {
    match outcome {
        ComplianceEraseOutcome::Completed {
            cited_object_purge_pending,
            cold_object_purge_pending,
            ..
        } => (*cold_object_purge_pending, *cited_object_purge_pending),
        ComplianceEraseOutcome::Refused { .. }
        | ComplianceEraseOutcome::NotFound { .. }
        | ComplianceEraseOutcome::Unauthorized { .. } => (false, false),
    }
}

fn refusal_label(reason: &ComplianceEraseRefusal) -> &'static str {
    match reason {
        ComplianceEraseRefusal::OwnerNotAbandoned => "OwnerNotAbandoned",
        ComplianceEraseRefusal::SourceScopeOwnerStillLive => "SourceScopeOwnerStillLive",
        ComplianceEraseRefusal::PersonalDropNotVerified => "PersonalDropNotVerified",
        ComplianceEraseRefusal::DropProofPortUnavailable => "DropProofPortUnavailable",
        ComplianceEraseRefusal::LegalHoldActive => "LegalHoldActive",
    }
}

fn refused(auth: &EraseAuthorization, reason: ComplianceEraseRefusal) -> ComplianceEraseOutcome {
    ComplianceEraseOutcome::Refused {
        operation_id: auth.audit().operation_id(),
        reason,
    }
}

async fn record_count(tx: &mut Tx<'_>, name: &str, count: u64) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO compliance_counts(name, count) VALUES ($1, $2)
         ON CONFLICT (name) DO UPDATE SET count = compliance_counts.count + EXCLUDED.count",
    )
    .bind(name)
    .bind(i64::try_from(count).unwrap_or(i64::MAX))
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn count_named(tx: &mut Tx<'_>, name: &str) -> Result<u64, StorageError> {
    let count: Option<i64> =
        sqlx::query_scalar("SELECT count FROM compliance_counts WHERE name = $1")
            .bind(name)
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_err)?;
    Ok(count.unwrap_or_default().try_into().unwrap_or_default())
}

async fn final_counts(tx: &mut Tx<'_>) -> Result<ComplianceEraseCounts, StorageError> {
    Ok(ComplianceEraseCounts {
        memories: count_named(tx, "memories").await?,
        goals: count_named(tx, "goals").await?,
        wake_configs: count_named(tx, "wake_configs").await?,
        blobs: count_named(tx, "blobs").await?,
        blob_uploads: count_named(tx, "blob_uploads").await?,
        sidecar_rows: count_named(tx, "sidecar_rows").await?,
        edges: count_named(tx, "edges").await?,
        receipts: count_named(tx, "receipts").await?,
        source_batches: count_named(tx, "source_batches").await?,
        source_cursors: count_named(tx, "source_cursors").await?,
        embeddings: count_named(tx, "embeddings").await?,
        embedding_jobs: count_named(tx, "embedding_jobs").await?,
        mcp_call_rows: count_named(tx, "mcp_call_rows").await?,
        change_events: count_named(tx, "change_events").await?,
        redacted_edge_targets: count_named(tx, "redacted_edge_targets").await?,
        suppressed_keys: count_named(tx, "suppressed_keys").await?,
        delegated_authority_grants: count_named(tx, "delegated_authority_grants").await?,
    })
}

async fn delete_dynamic_sidecars(
    tx: &mut Tx<'_>,
    tables: &[String],
    sidecar_column: &str,
    selected_table: &str,
    selected_column: &str,
) -> Result<u64, StorageError> {
    let mut total = 0;
    for table in tables {
        total += delete_fixed_by_selected(
            tx,
            table,
            sidecar_column,
            selected_table,
            selected_column,
            "sidecar",
        )
        .await?;
    }
    Ok(total)
}

/// Erase owner-pinned sidecars by the sidecar's OWN owner.
///
/// These rows record an act, not a Memory: an owner transfer moves the
/// Memory and leaves them behind. Reached through `selected_memories` they
/// would be unerasable by the owner that wrote them (its Memory is gone)
/// and erasable by the owner that received it (which never owned them) —
/// zombie rows on one side, someone else's audit trail on the other.
///
/// Source-scoped erase still asks the Memory which source a call belongs
/// to, deliberately without an owner predicate on that lookup: the row
/// being erased is already proven to be this owner's, and the Memory is
/// only being consulted for its `source_id`.
async fn delete_owner_pinned_sidecars(
    tx: &mut Tx<'_>,
    tables: &[String],
    owner: OwnerRef,
    scope: SelectionScope<'_>,
) -> Result<u64, StorageError> {
    let (_owner_kind, owner_id) = owner_binds(&owner);
    let mut total = 0;
    for table in tables {
        let ident = PgIdent::table(table)?;
        // SQL-POLICY: PgIdent
        let sql = match scope {
            SelectionScope::Owner => {
                format!(
                    "DELETE FROM {tbl} WHERE owner_id = $1",
                    tbl = ident.as_str()
                )
            }
            SelectionScope::Source(_) => format!(
                "DELETE FROM {tbl} a
                  WHERE a.owner_id = $1
                    AND (EXISTS (SELECT 1 FROM proxima_core.memory m
                                  WHERE m.t = a.t AND m.source_id = $2)
                      OR EXISTS (SELECT 1 FROM proxima_core.cooled c
                                  WHERE c.t = a.t AND c.source_id = $2))",
                tbl = ident.as_str()
            ),
        };
        // SQL-POLICY: PgIdent
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(owner_id);
        if let SelectionScope::Source(source_id) = scope {
            query = query.bind(source_id.as_str());
        }
        total += query
            .execute(&mut **tx)
            .await
            .map_err(map_err)?
            .rows_affected();
    }
    Ok(total)
}

async fn delete_fixed_by_selected(
    tx: &mut Tx<'_>,
    table: &str,
    table_column: &str,
    selected_table: &str,
    selected_column: &str,
    _name: &str,
) -> Result<u64, StorageError> {
    let table = PgIdent::table(table)?;
    let table_column = PgIdent::column(table_column)?;
    let selected_table = PgIdent::table(selected_table)?;
    let selected_column = PgIdent::column(selected_column)?;
    // SQL-POLICY: PgIdent
    let sql = format!(
        "DELETE FROM {table} t USING {selected_table} s WHERE t.{table_column} = s.{selected_column}",
        table = table.as_str(),
        selected_table = selected_table.as_str(),
        table_column = table_column.as_str(),
        selected_column = selected_column.as_str()
    );
    // SQL-POLICY: PgIdent
    let result = sqlx::query(sqlx::AssertSqlSafe(sql))
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(result.rows_affected())
}

async fn delete_selected_table(
    tx: &mut Tx<'_>,
    table: &str,
    table_column: &str,
    selected_table: &str,
    selected_column: &str,
) -> Result<u64, StorageError> {
    delete_fixed_by_selected(
        tx,
        table,
        table_column,
        selected_table,
        selected_column,
        table,
    )
    .await
}

/// Delete cooled stubs for selected admissions and mark their cold objects
/// pending destruction. The objects themselves are destroyed after this
/// transaction commits (see [`super::forget::purge_cold_objects_after_commit`]):
/// deleting them here would destroy the payload of an admission that a
/// rollback puts back.
async fn delete_selected_cooled(
    tx: &mut Tx<'_>,
    operation_id: uuid::Uuid,
) -> Result<(u64, ColdPurgePlan), StorageError> {
    let keys: Vec<String> = sqlx::query_scalar(
        "INSERT INTO proxima_core.cold_purge_pending
             (object_key, owner_id, compliance_operation_id)
         SELECT c.object_key, c.owner_id, $1
           FROM proxima_core.cooled c
           JOIN selected_memories sm ON sm.memory_id = c.t
         ON CONFLICT (object_key) DO UPDATE SET
             enqueued_at = now(),
             compliance_operation_id = COALESCE(
                 EXCLUDED.compliance_operation_id,
                 cold_purge_pending.compliance_operation_id
             )
         RETURNING object_key",
    )
    .bind(operation_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    let deleted = sqlx::query(
        "DELETE FROM proxima_core.cooled c
          WHERE EXISTS (
                SELECT 1 FROM selected_memories sm WHERE sm.memory_id = c.t
          )",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?
    .rows_affected();
    Ok((deleted, ColdPurgePlan::from_keys(keys)))
}

async fn selected_content_ids(tx: &mut Tx<'_>) -> Result<Vec<uuid::Uuid>, StorageError> {
    sqlx::query_scalar(
        "SELECT DISTINCT content_id FROM (
             SELECT m.content_id
               FROM proxima_core.memory m
               JOIN selected_memories sm ON sm.memory_id = m.t
              WHERE m.content_id IS NOT NULL
             UNION
             SELECT c.content_id
               FROM proxima_core.cooled c
               JOIN selected_memories sm ON sm.memory_id = c.t
              WHERE c.content_id IS NOT NULL
         ) x",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)
}

async fn delete_embeddings(tx: &mut Tx<'_>, table: &str) -> Result<u64, StorageError> {
    let table = PgIdent::table(table)?;
    // SQL-POLICY: PgIdent
    let sql = format!(
        "DELETE FROM {table} e
          WHERE EXISTS (SELECT 1 FROM selected_memories sm WHERE sm.memory_id = e.entity_id)
             OR EXISTS (SELECT 1 FROM selected_goals sg WHERE sg.goal_id = e.entity_id)",
        table = table.as_str()
    );
    // SQL-POLICY: PgIdent
    let result = sqlx::query(sqlx::AssertSqlSafe(sql))
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(result.rows_affected())
}

fn delete_goal_refs(_tx: &mut Tx<'_>) {}

fn delete_memory_refs(_tx: &mut Tx<'_>) {}

async fn delete_change_events(tx: &mut Tx<'_>, owner: OwnerRef) -> Result<u64, StorageError> {
    let (_owner_kind, owner_id) = owner_binds(&owner);
    let result = sqlx::query(
        "DELETE FROM proxima_core.announce a
          WHERE a.owner_id = $1
            AND (
                EXISTS (SELECT 1 FROM selected_memories sm WHERE sm.memory_id = a.t)
                OR EXISTS (SELECT 1 FROM selected_goals sg WHERE sg.goal_id = a.t)
            )",
    )
    .bind(owner_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected())
}

/// Delegations are owner-level authority, never source-attributable data.
/// Owner erasure removes grants for that exact owner. Dropping a personal
/// owner additionally removes every grant issued under that subject's bearer,
/// including grants targeting a group owner; otherwise the deleted identity
/// would remain durable in another owner's authority table.
async fn delete_delegated_authority_grants(
    tx: &mut Tx<'_>,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
) -> Result<u64, StorageError> {
    if matches!(scope, SelectionScope::Source(_)) {
        return Ok(0);
    }
    let (owner_kind, owner_id) = owner_binds(&owner);
    let erased_subject = match owner {
        OwnerRef::Personal(user_id) => Some(user_id.into_inner()),
        OwnerRef::Group(_) => None,
    };
    let result = sqlx::query(
        "DELETE FROM proxima_core.delegated_authority_grants dag
          WHERE (dag.owner_kind = $1
                 AND dag.owner_id = $2)
             OR ($3::uuid IS NOT NULL AND dag.subject_user_id = $3)",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(erased_subject)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected())
}

/// Destroy the owner's wake configuration. `wake_config` is owner-authored
/// content — a free-text `prompt`, the hard-context `hard_memory_t` set, the
/// armed `tool_ids` — and nothing else collects it: `goal.wake_id` is
/// `ON DELETE RESTRICT` and erase never deletes the `owners` row, so a
/// forgotten statement here leaves the prompt text behind forever.
///
/// Owner erase has already deleted every goal of the owner, so every wake row
/// of that owner goes; a wake row an outside owner's goal still arms fails the
/// RESTRICT FK and aborts the erase rather than silently surviving it.
/// Source-scope erase selects no goals and `wake_config` has no source
/// attribution, so every wake row remains outside that erase scope.
async fn delete_wake_configs(
    tx: &mut Tx<'_>,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
) -> Result<u64, StorageError> {
    let (_owner_kind, owner_id) = owner_binds(&owner);
    let result = match scope {
        SelectionScope::Owner => sqlx::query(
            "DELETE FROM proxima_core.wake_config
              WHERE owner_id = $1",
        )
        .bind(owner_id)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?,
        SelectionScope::Source(_) => return Ok(0),
    };
    Ok(result.rows_affected())
}

/// Rows destroyed by [`delete_blobs`], reported separately because the three
/// tables hold different owner data: content hashes, S3 upload metadata, and
/// whatever a flavor's citation payload carries.
struct BlobEraseCounts {
    sidecar_rows: u64,
    uploads: u64,
    blobs: u64,
    cold_purge: ColdPurgePlan,
}

/// Destroy the owner's cited-blob rows: the registered citation sidecar rows
/// keyed on `blob_id`, then `blob_uploads`, then `blob` itself. That order is
/// the FK order — `blob_uploads.blob_id` and a flavor sidecar's own
/// `blob_id` reference both point at `blob` — and `memory.blob_id` is why this
/// runs after the memory deletions above.
///
/// `blob` carries `schema_id` and `content_hash`; `blob_uploads` carries
/// `bucket`, `object_key`, `filename`, `mime`, `sha256`, `etag` and
/// `error_message`. All of it is owner data, and nothing else collected it:
/// erase never deletes the `owners` row, so the rows persisted forever.
///
/// Owner erase takes every
/// blob row of the owner — the memory rows are already gone, so a surviving
/// reference means the erase is wrong and the `NO ACTION` FK aborts it rather
/// than letting the row survive quietly. Source-scope candidates are captured
/// from the selected hot and cooled admissions before either table is deleted.
/// After those deletions, candidates still referenced by a surviving hot or
/// cooled admission are removed from the set.
///
/// `blob_uploads` rows with a NULL `blob_id` are pending or aborted uploads
/// attributable to the owner and to no source, so source-scope erase leaves
/// them exactly as it leaves owner-level delegated-authority grants.
///
/// ROW deletion is unconditional; OBJECT deletion is not. `blob` rows are
/// per-owner by `UNIQUE (owner_id, schema_id, content_hash)`, so deleting
/// this owner's rows can never touch another owner's — that part of the
/// invariant above is untouched by the shared-blob dedupe arm. The S3
/// object is the thing the arm made shareable, and
/// [`enqueue_blob_object_keys`] is where "the row is going" stopped
/// implying "the bytes are going".
async fn delete_blobs(
    tx: &mut Tx<'_>,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
    operation_id: uuid::Uuid,
    tables: &ComplianceSidecarTables,
) -> Result<BlobEraseCounts, StorageError> {
    let (_owner_kind, owner_id) = owner_binds(&owner);
    if matches!(scope, SelectionScope::Source(_)) {
        sqlx::query(
            "DELETE FROM selected_blobs sb
              WHERE EXISTS (
                    SELECT 1 FROM proxima_core.memory m WHERE m.blob_id = sb.blob_id
              )
                 OR EXISTS (
                    SELECT 1 FROM proxima_core.cooled c WHERE c.blob_id = sb.blob_id
              )",
        )
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    }
    let mut sidecar_rows = delete_dynamic_sidecars(
        tx,
        tables.cited_object.as_slice(),
        "cited_object_id",
        "selected_blobs",
        "blob_id",
    )
    .await?;
    sidecar_rows += delete_dynamic_sidecars(
        tx,
        tables.citation_mapping.as_slice(),
        "citation_mapping_id",
        "selected_blobs",
        "blob_id",
    )
    .await?;
    let object_keys = enqueue_blob_object_keys(tx, owner_id, scope, operation_id).await?;
    let uploads = match scope {
        SelectionScope::Owner => {
            sqlx::query("DELETE FROM proxima_core.blob_uploads WHERE owner_id = $1")
                .bind(owner_id)
                .execute(&mut **tx)
                .await
                .map_err(map_err)?
                .rows_affected()
        }
        SelectionScope::Source(_) => {
            delete_selected_table(
                tx,
                "proxima_core.blob_uploads",
                "blob_id",
                "selected_blobs",
                "blob_id",
            )
            .await?
        }
    };
    let blobs = delete_selected_table(
        tx,
        "proxima_core.blob",
        "blob_id",
        "selected_blobs",
        "blob_id",
    )
    .await?;
    Ok(BlobEraseCounts {
        sidecar_rows,
        uploads,
        blobs,
        cold_purge: ColdPurgePlan::from_keys(object_keys),
    })
}

/// Which of the erased scope's objects may actually be destroyed.
///
/// REFCOUNT BY QUERY, not by counter — `gc_unreferenced_content`'s idiom,
/// one level down. Before the shared-blob dedupe arm, an object had exactly
/// one `blob_uploads` row and "the row is going" and "the object is going"
/// were the same statement. A mount makes the relation many-to-one: two
/// owners' rows may name one object, so erasing one owner must leave the
/// bytes the other still reads.
///
/// The anti-join runs BEFORE the erase deletes this scope's rows, so it has
/// to exclude them explicitly rather than rely on them being gone: the
/// `NOT EXISTS` asks whether any row OUTSIDE the erased scope names the
/// key. Running it after the deletes would be simpler and wrong in the
/// other direction — nothing else in the transaction may observe the rows
/// as deleted before the commit that publishes it.
///
/// A key held back here is not leaked: it stays reachable through the
/// surviving owner's rows, and becomes an ordinary orphan for the bucket
/// sweep only once that owner's rows go too.
async fn enqueue_blob_object_keys(
    tx: &mut Tx<'_>,
    owner_id: uuid::Uuid,
    scope: SelectionScope<'_>,
    operation_id: uuid::Uuid,
) -> Result<Vec<String>, StorageError> {
    match scope {
        SelectionScope::Owner => sqlx::query_scalar(
            "INSERT INTO proxima_core.cold_purge_pending
                 (object_key, owner_id, compliance_operation_id)
             SELECT DISTINCT u.object_key, u.owner_id, $2
               FROM proxima_core.blob_uploads u
              WHERE u.owner_id = $1
                AND NOT EXISTS (
                        SELECT 1
                          FROM proxima_core.blob_uploads other
                         WHERE other.object_key = u.object_key
                           AND other.owner_id <> $1
                    )
             ON CONFLICT (object_key) DO UPDATE SET
                 enqueued_at = now(),
                 compliance_operation_id = COALESCE(
                     EXCLUDED.compliance_operation_id,
                     cold_purge_pending.compliance_operation_id
                 )
             RETURNING object_key",
        )
        .bind(owner_id)
        .bind(operation_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_err),
        SelectionScope::Source(_) => sqlx::query_scalar(
            "INSERT INTO proxima_core.cold_purge_pending
                 (object_key, owner_id, compliance_operation_id)
             SELECT DISTINCT u.object_key, u.owner_id, $1
               FROM proxima_core.blob_uploads u
               JOIN selected_blobs sb ON sb.blob_id = u.blob_id
              WHERE NOT EXISTS (
                        SELECT 1
                          FROM proxima_core.blob_uploads other
                         WHERE other.object_key = u.object_key
                           AND other.upload_id <> u.upload_id
                           AND NOT EXISTS (
                                   SELECT 1
                                     FROM selected_blobs sb2
                                    WHERE sb2.blob_id = other.blob_id
                               )
                    )
             ON CONFLICT (object_key) DO UPDATE SET
                 enqueued_at = now(),
                 compliance_operation_id = COALESCE(
                     EXCLUDED.compliance_operation_id,
                     cold_purge_pending.compliance_operation_id
                 )
             RETURNING object_key",
        )
        .bind(operation_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_err),
    }
}

/// Delete persisted projector source cursors for the erased scope inside the
/// erase transaction. Owner erase
/// removes every cursor for the owner; source-scope erase removes only the
/// matching `source`. Cursor bytes stay opaque — this is pure lawful cleanup so
/// a re-provisioned owner/source does not resume from a stale offset.
async fn delete_source_cursors(
    tx: &mut Tx<'_>,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
) -> Result<u64, StorageError> {
    let (owner_kind, owner_id) = owner_binds(&owner);
    let result = match scope {
        SelectionScope::Owner => sqlx::query(
            "DELETE FROM proxima_core.source_cursors
              WHERE owner_kind = $1
                AND owner_id = $2",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?,
        SelectionScope::Source(source_id) => sqlx::query(
            "DELETE FROM proxima_core.source_cursors
              WHERE owner_kind = $1
                AND owner_id = $2
                AND source = $3",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .bind(source_id.as_str())
        .execute(&mut **tx)
        .await
        .map_err(map_err)?,
    };
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    #[test]
    fn erase_sql_does_not_name_retired_suppression_table() {
        let src = include_str!("compliance_erase.rs");
        let needle = format!("{}.{}", "proxima_core", "compliance_suppression_keys");
        assert!(
            !src.contains(&needle),
            "v008 has no suppression table; Lean retired SuppressionKey"
        );
    }

    #[test]
    fn owner_erase_names_cooled() {
        let src = include_str!("compliance_erase.rs");
        assert!(
            src.contains("proxima_core.cooled"),
            "owner erase must select and delete cooled"
        );
        assert!(
            src.contains("gc_unreferenced_content"),
            "owner erase must GC Content"
        );
    }

    #[test]
    fn group_abandonment_counts_group_memberships() {
        let src = include_str!("compliance_erase.rs");
        let retired = format!("{}.{}", "proxima_core", "resolved_group_memberships");
        let live = format!("{}.{}", "proxima_core", "group_memberships");
        assert!(
            !src.contains(&retired),
            "P1: resolved_group_memberships is not in 0001_v008"
        );
        assert!(
            src.contains(&live),
            "P1: abandonment counts proxima_core.group_memberships"
        );
    }

    /// Parity pin for the deleted `delete_fixed_*_sidecars` overlays.
    ///
    /// Those two functions named four core sidecar tables by hand and
    /// deleted from them a second time, after the registry-driven sweep had
    /// already done it. The literals below are exactly the tables they
    /// named; the assertion is that the registry pass reaches each one, so
    /// removing the overlay removed duplicate work and not coverage.
    #[test]
    fn the_registry_pass_reaches_every_core_sidecar() {
        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        let tables = proxima_core::compliance::ComplianceSidecarTables::for_registry(&registry);

        for table in [
            "proxima_core.agent_derivation_v1",
            "proxima_core.agent_note_v1",
            "proxima_core.utterance_v1",
        ] {
            assert!(
                tables.fact.iter().any(|entry| entry == table),
                "{table} was in delete_fixed_memory_sidecars; the memory-keyed sweep must reach it"
            );
        }
        assert!(
            tables
                .goal
                .iter()
                .any(|entry| entry == "proxima_core.task_goal_v1"),
            "task_goal_v1 was in delete_fixed_goal_sidecars; the goal sweep must reach it"
        );
    }
}

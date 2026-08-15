// Five erase entry points take (pool, owner parts, source scope, audit
// context, authorization) — every argument is a distinct authority the erase
// has to be handed, and bundling them into a struct would only move the arity
// to its constructor. `too_many_lines` is narrowed to the two functions that
// earn it, below.
#![allow(clippy::too_many_arguments)]

use proxima_core::compliance::{
    ComplianceAuditContext, ComplianceEraseCounts, ComplianceEraseOutcome, ComplianceEraseRefusal,
    ComplianceEraseTarget, EraseAuthorization,
};
use proxima_core::{GroupId, OwnerRef, SourceId, StorageError, UserId};
use sqlx::{PgPool, Postgres, Transaction};

use crate::access::owner_columns::{lock_group_membership_tx, owner_binds};
use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::verbs::fact_retention::{legal_hold_active_tx, lock_legal_hold_tx};

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
    auth: &EraseAuthorization,
    group_id: GroupId,
    object_purge_planned: bool,
    fact_sidecar_tables: &[String],
    goal_sidecar_tables: &[String],
    citation_mapping_sidecar_tables: &[String],
    cited_object_sidecar_tables: &[String],
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
    erase_selected(
        &mut tx,
        auth,
        owner,
        SelectionScope::Owner,
        fact_sidecar_tables,
        goal_sidecar_tables,
        citation_mapping_sidecar_tables,
        cited_object_sidecar_tables,
    )
    .await?;
    let counts = final_counts(&mut tx).await?;
    let outcome = ComplianceEraseOutcome::Completed {
        operation_id: auth.audit().operation_id(),
        counts,
        cited_object_purge_pending: object_purge_planned,
    };
    upsert_audit_outcome(&mut tx, auth.audit(), &outcome, counts).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

pub async fn erase_personal_owner_if_drop_verified(
    pool: &PgPool,
    auth: &EraseAuthorization,
    user_id: UserId,
    object_purge_planned: bool,
    fact_sidecar_tables: &[String],
    goal_sidecar_tables: &[String],
    citation_mapping_sidecar_tables: &[String],
    cited_object_sidecar_tables: &[String],
) -> Result<ComplianceEraseOutcome, StorageError> {
    let owner = OwnerRef::Personal(user_id);
    let mut tx = begin_bulk_erase_tx(pool).await?;
    if let Some(outcome) = refuse_if_legal_hold_active(&mut tx, auth, owner).await? {
        tx.commit().await.map_err(map_err)?;
        return Ok(outcome);
    }
    erase_selected(
        &mut tx,
        auth,
        owner,
        SelectionScope::Owner,
        fact_sidecar_tables,
        goal_sidecar_tables,
        citation_mapping_sidecar_tables,
        cited_object_sidecar_tables,
    )
    .await?;
    let counts = final_counts(&mut tx).await?;
    let outcome = ComplianceEraseOutcome::Completed {
        operation_id: auth.audit().operation_id(),
        counts,
        cited_object_purge_pending: object_purge_planned,
    };
    upsert_audit_outcome(&mut tx, auth.audit(), &outcome, counts).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

pub async fn erase_group_source_scope_if_owner_abandoned(
    pool: &PgPool,
    auth: &EraseAuthorization,
    group_id: GroupId,
    source_id: &SourceId,
    fact_sidecar_tables: &[String],
    goal_sidecar_tables: &[String],
    citation_mapping_sidecar_tables: &[String],
    cited_object_sidecar_tables: &[String],
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
    erase_selected(
        &mut tx,
        auth,
        owner,
        SelectionScope::Source(source_id),
        fact_sidecar_tables,
        goal_sidecar_tables,
        citation_mapping_sidecar_tables,
        cited_object_sidecar_tables,
    )
    .await?;
    let counts = final_counts(&mut tx).await?;
    let outcome = ComplianceEraseOutcome::Completed {
        operation_id: auth.audit().operation_id(),
        counts,
        cited_object_purge_pending: false,
    };
    upsert_audit_outcome(&mut tx, auth.audit(), &outcome, counts).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

pub async fn erase_personal_source_scope_if_drop_verified(
    pool: &PgPool,
    auth: &EraseAuthorization,
    user_id: UserId,
    source_id: &SourceId,
    fact_sidecar_tables: &[String],
    goal_sidecar_tables: &[String],
    citation_mapping_sidecar_tables: &[String],
    cited_object_sidecar_tables: &[String],
) -> Result<ComplianceEraseOutcome, StorageError> {
    let owner = OwnerRef::Personal(user_id);
    let mut tx = begin_bulk_erase_tx(pool).await?;
    if let Some(outcome) = refuse_if_legal_hold_active(&mut tx, auth, owner).await? {
        tx.commit().await.map_err(map_err)?;
        return Ok(outcome);
    }
    erase_selected(
        &mut tx,
        auth,
        owner,
        SelectionScope::Source(source_id),
        fact_sidecar_tables,
        goal_sidecar_tables,
        citation_mapping_sidecar_tables,
        cited_object_sidecar_tables,
    )
    .await?;
    let counts = final_counts(&mut tx).await?;
    let outcome = ComplianceEraseOutcome::Completed {
        operation_id: auth.audit().operation_id(),
        counts,
        cited_object_purge_pending: false,
    };
    upsert_audit_outcome(&mut tx, auth.audit(), &outcome, counts).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

async fn group_member_count(tx: &mut Tx<'_>, group_id: GroupId) -> Result<i64, StorageError> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.resolved_group_memberships WHERE group_id = $1",
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

#[expect(
    clippy::too_many_lines,
    reason = "one erase transaction: every delete is ordered against the next and splitting it would hide that order behind call sites"
)]
async fn erase_selected(
    tx: &mut Tx<'_>,
    auth: &EraseAuthorization,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
    fact_sidecar_tables: &[String],
    goal_sidecar_tables: &[String],
    citation_mapping_sidecar_tables: &[String],
    cited_object_sidecar_tables: &[String],
) -> Result<(), StorageError> {
    create_selected_sets(tx, owner, scope).await?;
    repoint_surviving_fact_entity_heads(tx).await?;
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
    let redactions = insert_redactions(tx, auth.audit().operation_id()).await?;
    let suppressed = insert_suppression_keys(tx, auth.audit().operation_id(), owner).await?;
    sqlx::query("CREATE TEMP TABLE compliance_counts(name text PRIMARY KEY, count bigint NOT NULL) ON COMMIT DROP")
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    record_count(tx, "redacted_edge_targets", redactions).await?;
    record_count(tx, "suppressed_keys", suppressed).await?;

    let delegated_authority_grants = delete_delegated_authority_grants(tx, owner, scope).await?;
    record_count(tx, "delegated_authority_grants", delegated_authority_grants).await?;

    // No edge sidecars to sweep: an edge carries no content, so there is
    // nothing hanging off it to erase.
    let edges = delete_selected_edges(tx).await?;
    record_count(tx, "edges", edges).await?;

    let change_events = delete_change_events(tx, owner).await?;
    record_count(tx, "change_events", change_events).await?;

    let source_cursors = delete_source_cursors(tx, owner, scope).await?;
    record_count(tx, "source_cursors", source_cursors).await?;

    delete_goal_refs(tx).await?;
    delete_memory_refs(tx).await?;

    delete_dynamic_sidecars(
        tx,
        goal_sidecar_tables,
        "goal_id",
        "selected_goals",
        "goal_id",
    )
    .await?;
    delete_fixed_goal_sidecars(tx).await?;
    delete_dynamic_sidecars(
        tx,
        fact_sidecar_tables,
        "memory_id",
        "selected_memories",
        "memory_id",
    )
    .await?;
    delete_fixed_memory_sidecars(tx).await?;
    delete_dynamic_sidecars(
        tx,
        citation_mapping_sidecar_tables,
        "citation_mapping_id",
        "selected_citation_mappings",
        "citation_mapping_id",
    )
    .await?;
    delete_dynamic_sidecars(
        tx,
        cited_object_sidecar_tables,
        "cited_object_id",
        "selected_cited_objects",
        "cited_object_id",
    )
    .await?;
    delete_fixed_cited_object_sidecars(tx).await?;

    let embedding_jobs = delete_embeddings(tx, "proxima_core.embedding_jobs").await?;
    record_count(tx, "embedding_jobs", embedding_jobs).await?;
    let embedding_heads = delete_embeddings(tx, "proxima_core.embedding_heads").await?;
    let embeddings = delete_embeddings(tx, "proxima_core.embeddings").await?;
    record_count(tx, "embeddings", embeddings.saturating_add(embedding_heads)).await?;

    let citations = delete_selected_table(
        tx,
        "proxima_core.citation_mappings",
        "citation_mapping_id",
        "selected_citation_mappings",
        "citation_mapping_id",
    )
    .await?;
    record_count(tx, "citations", citations).await?;

    let cited_objects = delete_selected_table(
        tx,
        "proxima_core.cited_objects",
        "cited_object_id",
        "selected_cited_objects",
        "cited_object_id",
    )
    .await?;
    record_count(tx, "cited_objects", cited_objects).await?;

    let fact_entities = delete_selected_table(
        tx,
        "proxima_core.fact_entities",
        "fact_entity_id",
        "selected_fact_entities",
        "fact_entity_id",
    )
    .await?;
    record_count(tx, "fact_entities", fact_entities).await?;

    let mcp_rows = delete_fixed_by_selected(
        tx,
        "proxima_core.mcp_call_logged_v1",
        "memory_id",
        "selected_memories",
        "memory_id",
        "mcp_call_rows",
    )
    .await?;
    record_count(tx, "mcp_call_rows", mcp_rows).await?;

    let memories = delete_selected_table(
        tx,
        "proxima_core.memories",
        "memory_id",
        "selected_memories",
        "memory_id",
    )
    .await?;
    record_count(tx, "memories", memories).await?;
    let goals = delete_selected_table(
        tx,
        "proxima_core.goals",
        "goal_id",
        "selected_goals",
        "goal_id",
    )
    .await?;
    record_count(tx, "goals", goals).await?;
    let receipts = delete_selected_table(
        tx,
        "proxima_core.fact_receipts",
        "receipt_id",
        "selected_receipts",
        "receipt_id",
    )
    .await?;
    record_count(tx, "receipts", receipts).await?;
    let source_batches = delete_selected_table(
        tx,
        "proxima_core.source_batches",
        "id",
        "selected_source_batches",
        "id",
    )
    .await?;
    record_count(tx, "source_batches", source_batches).await?;
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "one temp-table build: the selection sets are defined against each other and a split would let a caller build half of them"
)]
async fn create_selected_sets(
    tx: &mut Tx<'_>,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
) -> Result<(), StorageError> {
    // Every erase entry point constructs `owner` as Group or Personal from a
    // typed id — World is not representable on this path — so `owner_id`
    // binds non-NULL here and in every erase statement below, and plain `=`
    // is exactly `IS NOT DISTINCT FROM` while staying an index condition
    // (`PostgreSQL` has no index strategy for DistinctExpr).
    let (owner_kind, owner_id) = owner_binds(&owner);
    sqlx::query("CREATE TEMP TABLE selected_source_batches(id uuid PRIMARY KEY, source_id text NOT NULL) ON COMMIT DROP")
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    match scope {
        SelectionScope::Owner => {
            sqlx::query(
                "INSERT INTO selected_source_batches(id, source_id)
                 SELECT id, source_id FROM proxima_core.source_batches
                  WHERE owner_kind = $1 AND owner_id = $2",
            )
            .bind(owner_kind)
            .bind(owner_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
        }
        SelectionScope::Source(source_id) => {
            sqlx::query(
                "INSERT INTO selected_source_batches(id, source_id)
                 SELECT id, source_id FROM proxima_core.source_batches
                  WHERE owner_kind = $1 AND owner_id = $2 AND source_id = $3",
            )
            .bind(owner_kind)
            .bind(owner_id)
            .bind(source_id.as_str())
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
        }
    }

    sqlx::query(
        "CREATE TEMP TABLE selected_receipts(receipt_id bytea PRIMARY KEY, source_batch_id uuid, source text NOT NULL, payload_hash bytea) ON COMMIT DROP",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    match scope {
        SelectionScope::Owner => {
            sqlx::query(
                "INSERT INTO selected_receipts(receipt_id, source_batch_id, source, payload_hash)
                 SELECT receipt_id, source_batch_id, source, payload_hash
                   FROM proxima_core.fact_receipts
                  WHERE owner_kind = $1 AND owner_id = $2",
            )
            .bind(owner_kind)
            .bind(owner_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
        }
        SelectionScope::Source(source_id) => {
            sqlx::query(
                "INSERT INTO selected_receipts(receipt_id, source_batch_id, source, payload_hash)
                 SELECT fr.receipt_id, fr.source_batch_id, fr.source, fr.payload_hash
                   FROM proxima_core.fact_receipts fr
                  WHERE fr.owner_kind = $1
                    AND fr.owner_id = $2
                    AND (fr.source = $3 OR EXISTS (
                        SELECT 1 FROM selected_source_batches sb WHERE sb.id = fr.source_batch_id
                    ))",
            )
            .bind(owner_kind)
            .bind(owner_id)
            .bind(source_id.as_str())
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
        }
    }

    sqlx::query("CREATE TEMP TABLE selected_memories(memory_id uuid PRIMARY KEY, kind proxima_core.entity_kind NOT NULL, fact_entity_id uuid, receipt_id bytea, source_batch_id uuid) ON COMMIT DROP")
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    match scope {
        SelectionScope::Owner => {
            sqlx::query(
                "INSERT INTO selected_memories(memory_id, kind, fact_entity_id, receipt_id, source_batch_id)
                 SELECT memory_id, COALESCE(kind, 'Fact'::proxima_core.entity_kind), fact_entity_id, receipt_id, source_batch_id
                   FROM proxima_core.memories
                  WHERE owner_kind = $1 AND owner_id = $2",
            )
            .bind(owner_kind)
            .bind(owner_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
        }
        SelectionScope::Source(_) => {
            sqlx::query(
                "INSERT INTO selected_memories(memory_id, kind, fact_entity_id, receipt_id, source_batch_id)
                 SELECT m.memory_id, m.kind, m.fact_entity_id, m.receipt_id, m.source_batch_id
                   FROM proxima_core.memories m
                  WHERE m.owner_kind = $1
                    AND m.owner_id = $2
                    AND (
                        EXISTS (SELECT 1 FROM selected_receipts sr WHERE sr.receipt_id = m.receipt_id)
                        OR EXISTS (SELECT 1 FROM selected_source_batches sb WHERE sb.id = m.source_batch_id)
                    )",
            )
            .bind(owner_kind)
            .bind(owner_id)
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
             SELECT goal_id FROM proxima_core.goals
              WHERE owner_kind = $1 AND owner_id = $2",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    }

    sqlx::query(
        "CREATE TEMP TABLE selected_fact_entities(fact_entity_id uuid PRIMARY KEY) ON COMMIT DROP",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    match scope {
        SelectionScope::Owner => {
            sqlx::query(
                "INSERT INTO selected_fact_entities(fact_entity_id)
                 SELECT fact_entity_id FROM proxima_core.fact_entities
                  WHERE owner_kind = $1 AND owner_id = $2",
            )
            .bind(owner_kind)
            .bind(owner_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
        }
        SelectionScope::Source(_) => {
            sqlx::query(
                "INSERT INTO selected_fact_entities(fact_entity_id)
                 SELECT DISTINCT fe.fact_entity_id
                   FROM proxima_core.fact_entities fe
                   JOIN selected_memories sm
                     ON sm.memory_id = fe.current_memory_id OR sm.fact_entity_id = fe.fact_entity_id
                  WHERE fe.owner_kind = $1
                    AND fe.owner_id = $2
                    AND NOT EXISTS (
                        SELECT 1
                          FROM proxima_core.memories survivor
                         WHERE survivor.fact_entity_id = fe.fact_entity_id
                           AND NOT EXISTS (
                               SELECT 1
                                 FROM selected_memories selected
                                WHERE selected.memory_id = survivor.memory_id
                           )
                    )",
            )
            .bind(owner_kind)
            .bind(owner_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
        }
    }

    sqlx::query("CREATE TEMP TABLE selected_citation_mappings(citation_mapping_id uuid PRIMARY KEY, cited_object_id uuid NOT NULL) ON COMMIT DROP")
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    sqlx::query(
        "INSERT INTO selected_citation_mappings(citation_mapping_id, cited_object_id)
         SELECT cm.citation_mapping_id, cm.cited_object_id
           FROM proxima_core.citation_mappings cm
          WHERE cm.owner_kind = $1
            AND cm.owner_id = $2
            AND (EXISTS (SELECT 1 FROM selected_memories sm WHERE sm.memory_id = cm.memory_id)
                 OR $3::boolean)",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(matches!(scope, SelectionScope::Owner))
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    sqlx::query(
        "CREATE TEMP TABLE selected_cited_objects(cited_object_id uuid PRIMARY KEY) ON COMMIT DROP",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "INSERT INTO selected_cited_objects(cited_object_id)
         SELECT co.cited_object_id
           FROM proxima_core.cited_objects co
          WHERE co.owner_kind = $1
            AND co.owner_id = $2
            AND (
                $3::boolean
                OR EXISTS (
                    SELECT 1 FROM selected_citation_mappings scm
                     WHERE scm.cited_object_id = co.cited_object_id
                )
            )
            AND NOT EXISTS (
                SELECT 1 FROM proxima_core.citation_mappings cm
                 WHERE cm.cited_object_id = co.cited_object_id
                   AND NOT EXISTS (
                       SELECT 1 FROM selected_citation_mappings scm
                        WHERE scm.citation_mapping_id = cm.citation_mapping_id
                   )
            )",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(matches!(scope, SelectionScope::Owner))
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    // An edge has no id, so the selection carries the key itself. It is
    // selected when its SOURCE is going — the row is owned by the source
    // owner, and an edge whose source survives keeps existing with its
    // target withheld (that is what the redaction table records).
    sqlx::query(
        "CREATE TEMP TABLE selected_edges(
             source_kind proxima_core.edge_endpoint_kind,
             source_id uuid,
             target_kind proxima_core.edge_endpoint_kind,
             target_id uuid,
             kind proxima_core.edge_kind,
             PRIMARY KEY (source_kind, source_id, target_kind, target_id, kind)
         ) ON COMMIT DROP",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    let owner_scoped = matches!(scope, SelectionScope::Owner);
    sqlx::query(
        "INSERT INTO selected_edges(source_kind, source_id, target_kind, target_id, kind)
         SELECT e.source_kind, e.source_id, e.target_kind, e.target_id, e.kind
           FROM proxima_core.edges e
          WHERE ($3::boolean AND e.owner_kind = $1 AND e.owner_id = $2)
             OR EXISTS (SELECT 1 FROM selected_memories sm WHERE sm.memory_id = e.source_id)
             OR EXISTS (SELECT 1 FROM selected_goals sg WHERE sg.goal_id = e.source_id)
             OR EXISTS (SELECT 1 FROM selected_fact_entities sfe
                         WHERE sfe.fact_entity_id = e.source_id)",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_scoped)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn delete_selected_edges(tx: &mut Tx<'_>) -> Result<u64, StorageError> {
    let result = sqlx::query(
        "DELETE FROM proxima_core.edges e
          USING selected_edges se
          WHERE e.source_kind = se.source_kind AND e.source_id = se.source_id
            AND e.target_kind = se.target_kind AND e.target_id = se.target_id
            AND e.kind = se.kind",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected())
}

async fn repoint_surviving_fact_entity_heads(tx: &mut Tx<'_>) -> Result<(), StorageError> {
    sqlx::query(
        "WITH surviving_heads AS (
             SELECT DISTINCT ON (fe.fact_entity_id)
                    fe.fact_entity_id, survivor.memory_id, survivor.created_at
               FROM proxima_core.fact_entities fe
               JOIN selected_memories selected_current
                 ON selected_current.memory_id = fe.current_memory_id
               JOIN proxima_core.memories survivor
                 ON survivor.fact_entity_id = fe.fact_entity_id
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM selected_memories selected
                     WHERE selected.memory_id = survivor.memory_id
                )
              ORDER BY fe.fact_entity_id, survivor.created_at DESC, survivor.memory_id DESC
         )
         UPDATE proxima_core.fact_entities fe
            SET current_memory_id = surviving_heads.memory_id,
                current_created_at = surviving_heads.created_at
           FROM surviving_heads
          WHERE fe.fact_entity_id = surviving_heads.fact_entity_id",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn insert_redactions(tx: &mut Tx<'_>, operation_id: uuid::Uuid) -> Result<u64, StorageError> {
    let result = sqlx::query(
        "INSERT INTO proxima_core.compliance_edge_target_redactions
            (operation_id, source_kind, source_id, target_kind, target_id, kind)
         SELECT $1, e.source_kind, e.source_id, e.target_kind, e.target_id, e.kind
           FROM proxima_core.edges e
          WHERE NOT EXISTS (
                    SELECT 1 FROM selected_edges se
                     WHERE se.source_kind = e.source_kind AND se.source_id = e.source_id
                       AND se.target_kind = e.target_kind AND se.target_id = e.target_id
                       AND se.kind = e.kind
                )
            AND (
                EXISTS (SELECT 1 FROM selected_memories sm WHERE sm.memory_id = e.target_id)
                OR EXISTS (SELECT 1 FROM selected_goals sg WHERE sg.goal_id = e.target_id)
                OR EXISTS (SELECT 1 FROM selected_fact_entities sfe
                            WHERE sfe.fact_entity_id = e.target_id)
            )
         ON CONFLICT DO NOTHING",
    )
    .bind(operation_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected())
}

async fn insert_suppression_keys(
    tx: &mut Tx<'_>,
    operation_id: uuid::Uuid,
    owner: OwnerRef,
) -> Result<u64, StorageError> {
    let (owner_kind, owner_id) = suppression_owner_parts(owner);
    let mut count = 0;

    let source_scope = sqlx::query(
        "INSERT INTO proxima_core.compliance_suppression_keys(
             key_class, suppression_key, operation_id)
         SELECT 'owner_source_scope'::proxima_core.compliance_suppression_key_class,
                decode(md5($1 || chr(31) || 'owner_source_scope' || chr(31) || $2 || chr(31) || $3 || chr(31) || coalesce($4, '') || chr(31) || source_id), 'hex'),
                $5
           FROM (SELECT DISTINCT source_id FROM selected_source_batches) scopes
         ON CONFLICT DO NOTHING",
    )
    .bind(SUPPRESSION_DOMAIN)
    .bind(&owner_kind)
    .bind(owner.stable_key_uuid().to_string())
    .bind(&owner_id)
    .bind(operation_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    count += source_scope.rows_affected();

    let source_batch = sqlx::query(
        "INSERT INTO proxima_core.compliance_suppression_keys(
             key_class, suppression_key, operation_id)
         SELECT 'source_batch'::proxima_core.compliance_suppression_key_class,
                decode(md5($1 || chr(31) || 'source_batch' || chr(31) || $2 || chr(31) || $3 || chr(31) || coalesce($4, '') || chr(31) || id::text), 'hex'),
                $5
           FROM selected_source_batches
         ON CONFLICT DO NOTHING",
    )
    .bind(SUPPRESSION_DOMAIN)
    .bind(&owner_kind)
    .bind(owner.stable_key_uuid().to_string())
    .bind(&owner_id)
    .bind(operation_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    count += source_batch.rows_affected();

    let receipt_content = sqlx::query(
        "INSERT INTO proxima_core.compliance_suppression_keys(
             key_class, suppression_key, operation_id)
         SELECT 'receipt_content'::proxima_core.compliance_suppression_key_class,
                decode(md5($1 || chr(31) || 'receipt_content' || chr(31) || $2 || chr(31) || $3 || chr(31) || coalesce($4, '') || chr(31) || encode(receipt_id, 'hex')), 'hex'),
                $5
           FROM selected_receipts
         ON CONFLICT DO NOTHING",
    )
    .bind(SUPPRESSION_DOMAIN)
    .bind(&owner_kind)
    .bind(owner.stable_key_uuid().to_string())
    .bind(&owner_id)
    .bind(operation_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    count += receipt_content.rows_affected();

    Ok(count)
}

pub async fn check_suppression_for_fact_tx(
    tx: &mut Tx<'_>,
    owner: OwnerRef,
    source_id: &SourceId,
    source_batch_id: uuid::Uuid,
    receipt_id: &[u8],
) -> Result<(), StorageError> {
    let (owner_kind, owner_id) = suppression_owner_parts(owner);
    let exists: Option<(i32,)> = sqlx::query_as(
        "WITH candidate_keys(key_class, suppression_key) AS (
             VALUES
             ('owner_source_scope'::proxima_core.compliance_suppression_key_class,
              decode(md5($1 || chr(31) || 'owner_source_scope' || chr(31) || $2 || chr(31) || $3 || chr(31) || coalesce($4, '') || chr(31) || $5), 'hex')),
             ('source_batch'::proxima_core.compliance_suppression_key_class,
              decode(md5($1 || chr(31) || 'source_batch' || chr(31) || $2 || chr(31) || $3 || chr(31) || coalesce($4, '') || chr(31) || $6::text), 'hex')),
             ('receipt_content'::proxima_core.compliance_suppression_key_class,
              decode(md5($1 || chr(31) || 'receipt_content' || chr(31) || $2 || chr(31) || $3 || chr(31) || coalesce($4, '') || chr(31) || encode($7::bytea, 'hex')), 'hex'))
         )
         SELECT 1
           FROM proxima_core.compliance_suppression_keys s
           JOIN candidate_keys k
             ON k.key_class = s.key_class
            AND k.suppression_key = s.suppression_key
          LIMIT 1",
    )
    .bind(SUPPRESSION_DOMAIN)
    .bind(&owner_kind)
    .bind(owner.stable_key_uuid().to_string())
    .bind(&owner_id)
    .bind(source_id.as_str())
    .bind(source_batch_id)
    .bind(receipt_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    if exists.is_some() {
        return Err(StorageError::Suppressed(
            "fact ingest suppressed by compliance erase".into(),
        ));
    }
    Ok(())
}

const SUPPRESSION_DOMAIN: &str = "proxima-compliance-suppression-v2";

fn suppression_owner_parts(owner: OwnerRef) -> (String, Option<String>) {
    let (kind, owner_id) = owner.columns();
    (kind.as_str().to_string(), owner_id.map(|id| id.to_string()))
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
    let purge_pending = outcome_purge_pending(outcome);
    sqlx::query(
        "INSERT INTO proxima_core.compliance_audit_log(
             operation_id, target_kind, outcome, refusal, owner_ref_digest,
             requester_digest, source_scope_digest, derived_auth_path, requested_at,
             completed_at, memories_count, goals_count, edges_count, fact_entities_count,
             receipts_count, source_batches_count, citations_count, cited_objects_count,
             source_cursors_count, embeddings_count, embedding_jobs_count, mcp_call_rows_count,
             change_events_count, redacted_edge_targets_count, suppressed_keys_count,
             delegated_authority_grants_count, cited_object_purge_pending)
         VALUES ($1, $2, $3::proxima_core.compliance_erase_outcome,
                 $4::proxima_core.compliance_erase_refusal, $5, $6, $7, $8, $9,
                 now(), $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26)
         ON CONFLICT (operation_id) DO UPDATE SET
             outcome = EXCLUDED.outcome,
             refusal = EXCLUDED.refusal,
             completed_at = EXCLUDED.completed_at,
             memories_count = EXCLUDED.memories_count,
             goals_count = EXCLUDED.goals_count,
             edges_count = EXCLUDED.edges_count,
             fact_entities_count = EXCLUDED.fact_entities_count,
             receipts_count = EXCLUDED.receipts_count,
             source_batches_count = EXCLUDED.source_batches_count,
             citations_count = EXCLUDED.citations_count,
             cited_objects_count = EXCLUDED.cited_objects_count,
             source_cursors_count = EXCLUDED.source_cursors_count,
             embeddings_count = EXCLUDED.embeddings_count,
             embedding_jobs_count = EXCLUDED.embedding_jobs_count,
             mcp_call_rows_count = EXCLUDED.mcp_call_rows_count,
             change_events_count = EXCLUDED.change_events_count,
             redacted_edge_targets_count = EXCLUDED.redacted_edge_targets_count,
             suppressed_keys_count = EXCLUDED.suppressed_keys_count,
             delegated_authority_grants_count = EXCLUDED.delegated_authority_grants_count,
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
    .bind(i64::try_from(counts.edges).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.fact_entities).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.receipts).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.source_batches).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.citations).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.cited_objects).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.source_cursors).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.embeddings).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.embedding_jobs).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.mcp_call_rows).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.change_events).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.redacted_edge_targets).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.suppressed_keys).unwrap_or(i64::MAX))
    .bind(i64::try_from(counts.delegated_authority_grants).unwrap_or(i64::MAX))
    .bind(purge_pending)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

fn audit_target(target: &ComplianceEraseTarget) -> (&'static str, OwnerRef, Option<&SourceId>) {
    match target {
        ComplianceEraseTarget::WorldOwner => ("WorldOwner", OwnerRef::World, None),
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
    let mut parts: Vec<&[u8]> = vec![kind.as_str().as_bytes(), stable_key.as_bytes()];
    if let Some(id) = owner_id.as_ref() {
        parts.push(id.as_bytes());
    }
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
fn outcome_purge_pending(outcome: &ComplianceEraseOutcome) -> bool {
    match outcome {
        ComplianceEraseOutcome::Completed {
            cited_object_purge_pending,
            ..
        } => *cited_object_purge_pending,
        ComplianceEraseOutcome::Refused { .. }
        | ComplianceEraseOutcome::NotFound { .. }
        | ComplianceEraseOutcome::Unauthorized { .. } => false,
    }
}

fn refusal_label(reason: &ComplianceEraseRefusal) -> &'static str {
    match reason {
        ComplianceEraseRefusal::OwnerNotAbandoned => "OwnerNotAbandoned",
        ComplianceEraseRefusal::WorldOwner => "WorldOwner",
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
        edges: count_named(tx, "edges").await?,
        fact_entities: count_named(tx, "fact_entities").await?,
        receipts: count_named(tx, "receipts").await?,
        source_batches: count_named(tx, "source_batches").await?,
        citations: count_named(tx, "citations").await?,
        cited_objects: count_named(tx, "cited_objects").await?,
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

async fn delete_fixed_goal_sidecars(tx: &mut Tx<'_>) -> Result<(), StorageError> {
    for table in [
        "proxima_core.goal_activated_v1",
        "proxima_core.goal_paused_v1",
        "proxima_core.goal_achieved_v1",
        "proxima_core.goal_abandoned_v1",
    ] {
        delete_fixed_by_selected(
            tx,
            table,
            "goal_id",
            "selected_goals",
            "goal_id",
            "goal_sidecar",
        )
        .await?;
        delete_fixed_by_selected(
            tx,
            table,
            "memory_id",
            "selected_memories",
            "memory_id",
            "goal_sidecar",
        )
        .await?;
    }
    delete_fixed_by_selected(
        tx,
        "proxima_core.goal_wake_config",
        "goal_id",
        "selected_goals",
        "goal_id",
        "goal_wake",
    )
    .await?;
    delete_fixed_by_selected(
        tx,
        "proxima_core.goal_wake_config",
        "trigger_memory_id",
        "selected_memories",
        "memory_id",
        "goal_wake",
    )
    .await?;
    delete_fixed_by_selected(
        tx,
        "proxima_core.task_goal_v1",
        "goal_id",
        "selected_goals",
        "goal_id",
        "task_goal",
    )
    .await?;
    Ok(())
}

async fn delete_fixed_memory_sidecars(tx: &mut Tx<'_>) -> Result<(), StorageError> {
    for table in [
        "proxima_core.agent_derivation_v1",
        "proxima_core.agent_note_v1",
        "proxima_core.utterance_v1",
    ] {
        delete_fixed_by_selected(
            tx,
            table,
            "memory_id",
            "selected_memories",
            "memory_id",
            "memory_sidecar",
        )
        .await?;
    }
    Ok(())
}

async fn delete_fixed_cited_object_sidecars(tx: &mut Tx<'_>) -> Result<(), StorageError> {
    for table in [
        "proxima_core.cited_mcp_call_io_v1",
        "proxima_core.cited_uploaded_blob_v1",
        "proxima_core.cited_object_uploads",
    ] {
        delete_fixed_by_selected(
            tx,
            table,
            "cited_object_id",
            "selected_cited_objects",
            "cited_object_id",
            "cited_sidecar",
        )
        .await?;
    }
    Ok(())
}

async fn delete_embeddings(tx: &mut Tx<'_>, table: &str) -> Result<u64, StorageError> {
    let table = PgIdent::table(table)?;
    // SQL-POLICY: PgIdent
    let sql = format!(
        "DELETE FROM {table} e
          WHERE EXISTS (SELECT 1 FROM selected_memories sm WHERE sm.kind = e.entity_kind AND sm.memory_id = e.entity_id)
             OR EXISTS (SELECT 1 FROM selected_goals sg WHERE 'Goal'::proxima_core.entity_kind = e.entity_kind AND sg.goal_id = e.entity_id)",
        table = table.as_str()
    );
    // SQL-POLICY: PgIdent
    let result = sqlx::query(sqlx::AssertSqlSafe(sql))
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(result.rows_affected())
}

async fn delete_goal_refs(tx: &mut Tx<'_>) -> Result<(), StorageError> {
    sqlx::query(
        "UPDATE proxima_core.goals g SET supersedes = NULL
          WHERE EXISTS (SELECT 1 FROM selected_goals sg WHERE sg.goal_id = g.supersedes)",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn delete_memory_refs(tx: &mut Tx<'_>) -> Result<(), StorageError> {
    // Both ends of the lineage pointer, and the authorship column: every
    // reference into the selection has to be cleared before the rows go.
    sqlx::query(
        "UPDATE proxima_core.memories m SET supersedes = NULL
          WHERE EXISTS (SELECT 1 FROM selected_memories sm WHERE sm.memory_id = m.supersedes)",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "UPDATE proxima_core.memories m SET superseded_by = NULL
          WHERE EXISTS (SELECT 1 FROM selected_memories sm WHERE sm.memory_id = m.superseded_by)",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "UPDATE proxima_core.memories m SET authoring_perspective_id = NULL
          WHERE EXISTS (
              SELECT 1 FROM selected_memories sm
               WHERE sm.memory_id = m.authoring_perspective_id
          )",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn delete_change_events(tx: &mut Tx<'_>, owner: OwnerRef) -> Result<u64, StorageError> {
    let (owner_kind, owner_id) = owner_binds(&owner);
    let result = sqlx::query(
        "DELETE FROM proxima_core.change_event ce
          WHERE ce.owner_kind = $1
            AND ce.owner_id = $2
            AND (
                EXISTS (SELECT 1 FROM selected_memories sm WHERE sm.memory_id = ce.entity_memory_id)
                OR EXISTS (SELECT 1 FROM selected_goals sg WHERE sg.goal_id = ce.entity_goal_id)
                OR EXISTS (
                    SELECT 1 FROM selected_edges se
                     WHERE se.source_kind = ce.edge_source_kind AND se.source_id = ce.edge_source_id
                       AND se.target_kind = ce.edge_target_kind AND se.target_id = ce.edge_target_id
                       AND se.kind = ce.edge_kind
                )
                OR EXISTS (SELECT 1 FROM selected_memories sm WHERE sm.memory_id = ce.edge_source_id OR sm.memory_id = ce.edge_target_id)
                OR EXISTS (SELECT 1 FROM selected_goals sg WHERE sg.goal_id = ce.edge_source_id OR sg.goal_id = ce.edge_target_id)
                OR EXISTS (SELECT 1 FROM selected_fact_entities sfe WHERE sfe.fact_entity_id = ce.edge_source_id OR sfe.fact_entity_id = ce.edge_target_id)
            )",
    )
    .bind(owner_kind)
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
        OwnerRef::World | OwnerRef::Group(_) => None,
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

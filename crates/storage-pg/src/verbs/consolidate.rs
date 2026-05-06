//! M5 — F→A consolidation storage verbs.
//!
//! Two functions:
//!
//! * [`load_batch_facts`] — fetch every Fact in a closed source-batch
//!   joined against its typed sidecar payload. Used by the engine's
//!   F→A dispatcher to build the operator's `F2AContext.facts`.
//!
//! * [`consolidate_batch_f2a`] — atomic single-tx persist of the
//!   operator's outputs: substrate `memories` (Abstractions), the
//!   typed sidecar via `jsonb_populate_record`, provenance edges,
//!   embeddings, `change_event` outbox rows, and the
//!   `source_batch_f2a` dedup row.

use proxima_core::operators::{
    A2PInvocationKey, A2PLineageKey, AbstractionRow, ConsolidateA2POutcome, ConsolidateA2PRequest,
    ConsolidateBatchF2AOutcome, ConsolidateBatchF2ARequest, F2AInvocationKey, FactRow, SidecarSpec,
};
use proxima_core::{MemoryId, Owner, Principal, SchemaVersion, SourceBatchId, StorageError};
use sqlx::PgPool;

use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::verbs::edge_append::{EdgeDraft, append_edge_in_tx};

/// SQL fragment listing batches that are closed and have no
/// `source_batch_f2a` row for this exact invocation key. Used by the
/// engine's catch-up dispatcher.
///
/// # Errors
///
/// `Internal` on sqlx failure.
pub async fn list_unconsolidated_batches(
    pool: &PgPool,
    owner: &Owner,
    key: &F2AInvocationKey<'_>,
) -> Result<Vec<SourceBatchId>, StorageError> {
    let (owner_kind, owner_principal_id) = owner_principal_columns(owner);

    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT sb.id \
         FROM proxima_core.source_batches sb \
         WHERE sb.owner_principal_kind = $1 \
           AND sb.owner_principal_id = $2 \
           AND sb.closed_at IS NOT NULL \
           AND NOT EXISTS ( \
                SELECT 1 FROM proxima_core.source_batch_f2a f2a \
                WHERE f2a.batch_id = sb.id \
                  AND f2a.operator_id = $3 \
                  AND f2a.prompt_version = $4 \
                  AND f2a.model_id = $5 \
                  AND f2a.personality_id = $6 \
                  AND f2a.personality_state_hash = $7 \
           ) \
         ORDER BY sb.opened_at ASC",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(key.operator_id)
    .bind(key.prompt_version)
    .bind(key.model_id)
    .bind(key.personality_id)
    .bind(key.personality_state_hash)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|(id,)| SourceBatchId::new(id))
        .collect())
}

fn owner_columns(owner: &Owner) -> (&'static str, uuid::Uuid, uuid::Uuid) {
    let (kind, principal_id) = owner_principal_columns(owner);
    (kind, principal_id, owner.org_id.into_inner())
}

fn owner_principal_columns(owner: &Owner) -> (&'static str, uuid::Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    (kind, principal_id)
}

/// Load all Facts in a closed source-batch with their typed sidecar
/// payloads serialised to JSON via `row_to_json(s.*)`.
///
/// One SELECT per spec, results concatenated. The substrate
/// `proxima_core.events` table carries `source_batch_id`; we JOIN
/// memories ↔ events to land at the right batch and then LEFT JOIN
/// the sidecar to pick up typed payload columns.
///
/// # Errors
///
/// Returns `Internal` on sqlx failure or a malformed sidecar
/// identifier.
pub async fn load_batch_facts(
    pool: &PgPool,
    owner: &Owner,
    batch_id: SourceBatchId,
    sidecars: &[SidecarSpec],
) -> Result<Vec<FactRow>, StorageError> {
    let (owner_kind, owner_principal_id) = owner_principal_columns(owner);
    let batch_uuid = batch_id.into_inner();

    let mut out = Vec::new();
    for spec in sidecars {
        let sidecar = PgIdent::table(&spec.sidecar_table)?;

        let sql = format!(
            "SELECT m.memory_id, e.schema_version, row_to_json(s.*) AS payload \
             FROM proxima_core.memories m \
             JOIN proxima_core.events e ON m.event_id = e.event_id \
             JOIN {sidecar} s ON s.memory_id = m.memory_id \
             WHERE e.source_batch_id = $1 \
               AND m.owner_principal_kind = $2 \
               AND m.owner_principal_id = $3 \
               AND m.schema_id = $4",
            sidecar = sidecar.as_str(),
        );

        let rows: Vec<(uuid::Uuid, i32, serde_json::Value)> = sqlx::query_as(&sql)
            .bind(batch_uuid)
            .bind(owner_kind)
            .bind(owner_principal_id)
            .bind(spec.schema_id.as_str())
            .fetch_all(pool)
            .await
            .map_err(map_err)?;

        for (memory_id, schema_version, payload) in rows {
            out.push(FactRow {
                memory_id: MemoryId::new(memory_id),
                schema_id: spec.schema_id.clone(),
                schema_version: SchemaVersion::new(u32::try_from(schema_version).unwrap_or(0)),
                payload_json: payload,
            });
        }
    }

    Ok(out)
}

/// Load current Abstraction heads for A→P input, newest first.
///
/// The storage trait receives concrete sidecar tables from the
/// build-time schema registry; this function never accepts ad-hoc
/// schema tables from outside the engine.
///
/// # Errors
///
/// Returns `Internal` on sqlx failure or a malformed sidecar identifier.
pub async fn load_a2p_abstractions(
    pool: &PgPool,
    owner: &Owner,
    sidecars: &[SidecarSpec],
    limit: usize,
) -> Result<Vec<AbstractionRow>, StorageError> {
    let (owner_kind, owner_principal_id) = owner_principal_columns(owner);
    let mut rows_all = Vec::new();
    for spec in sidecars {
        let sidecar = PgIdent::table(&spec.sidecar_table)?;
        let sql = format!(
            "SELECT m.memory_id, m.schema_version, m.text, row_to_json(s.*) AS payload, m.created_at \
             FROM proxima_core.memories m \
             JOIN {sidecar} s ON s.memory_id = m.memory_id \
             WHERE m.owner_principal_kind = $1 \
               AND m.owner_principal_id = $2 \
               AND m.kind = 'Abstraction' \
               AND m.schema_id = $3 \
               AND NOT EXISTS ( \
                    SELECT 1 FROM proxima_core.memories newer \
                    WHERE newer.supersedes = m.memory_id \
               ) \
             ORDER BY m.created_at DESC, m.memory_id DESC \
             LIMIT $4",
            sidecar = sidecar.as_str(),
        );
        let rows: Vec<(
            uuid::Uuid,
            i32,
            String,
            serde_json::Value,
            time::OffsetDateTime,
        )> = sqlx::query_as(&sql)
            .bind(owner_kind)
            .bind(owner_principal_id)
            .bind(spec.schema_id.as_str())
            .bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .fetch_all(pool)
            .await
            .map_err(map_err)?;
        for (memory_id, schema_version, text, payload, created_at) in rows {
            rows_all.push((
                created_at,
                memory_id,
                AbstractionRow {
                    memory_id: MemoryId::new(memory_id),
                    schema_id: spec.schema_id.clone(),
                    schema_version: SchemaVersion::new(u32::try_from(schema_version).unwrap_or(0)),
                    text,
                    payload_json: payload,
                },
            ));
        }
    }
    rows_all.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    Ok(rows_all
        .into_iter()
        .take(limit)
        .map(|(_, _, row)| row)
        .collect())
}

/// Probe `proxima_core.a2p_invocations` for the full A→P invocation key.
///
/// Owner scope is principal-only — matches `load_a2p_abstractions`, so
/// same-principal/different-org runs hit the same idempotency row.
///
/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub async fn has_a2p_invocation(
    pool: &PgPool,
    owner: &Owner,
    key: &A2PInvocationKey<'_>,
) -> Result<bool, StorageError> {
    let (owner_kind, owner_principal_id) = owner_principal_columns(owner);
    let exists: Option<(i32,)> = sqlx::query_as(
        "SELECT 1 FROM proxima_core.a2p_invocations \
         WHERE owner_principal_kind = $1 \
           AND owner_principal_id = $2 \
           AND operator_id = $3 \
           AND prompt_version = $4 \
           AND model_id = $5 \
           AND personality_id = $6 \
           AND personality_state_hash = $7 \
           AND context_hash = $8 \
           AND input_hash = $9",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(key.operator_id)
    .bind(key.prompt_version)
    .bind(key.model_id)
    .bind(key.personality_id)
    .bind(key.personality_state_hash)
    .bind(key.context_hash)
    .bind(key.input_hash)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(exists.is_some())
}

/// Most recent Perspective head for this operator/model/personality
/// lineage, ignoring context/input shifts. Owner scope is principal-only.
///
/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub async fn lookup_prior_a2p_head(
    pool: &PgPool,
    owner: &Owner,
    key: &A2PLineageKey<'_>,
) -> Result<Option<MemoryId>, StorageError> {
    let (owner_kind, owner_principal_id) = owner_principal_columns(owner);
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT head_memory_id FROM proxima_core.a2p_invocations \
         WHERE owner_principal_kind = $1 \
           AND owner_principal_id = $2 \
           AND operator_id = $3 \
           AND prompt_version = $4 \
           AND model_id = $5 \
           AND personality_id = $6 \
           AND personality_state_hash = $7 \
           AND head_memory_id IS NOT NULL \
         ORDER BY run_at DESC \
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(key.operator_id)
    .bind(key.prompt_version)
    .bind(key.model_id)
    .bind(key.personality_id)
    .bind(key.personality_state_hash)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(row.map(|(id,)| MemoryId::new(id)))
}

/// Atomic F→A persistence. See `Storage::consolidate_batch_f2a`.
///
/// Layout per tx:
///
/// 1. Pre-check `source_batch_f2a` for the full invocation key —
///    `(batch_id, operator_id, prompt_version, model_id,
///    personality_id, personality_state_hash)` — and short-circuit
///    to `already_consolidated = true` on hit.
/// 2. Insert N substrate `memories` rows (Abstraction kind).
/// 3. Insert N typed sidecar rows via `jsonb_populate_record`.
/// 4. Insert M provenance edges (one per (Abstraction, Fact) pair).
///    Validated `provenance ⊆ batch facts` upstream of this verb;
///    the storage layer trusts the dispatcher's check.
/// 5. Insert N embedding rows (`float4[]` in M5; pgvector swap is
///    M6+).
/// 6. Insert N + M `change_event` rows (`EntityAppend` × N,
///    `EdgeAppend` × M).
/// 7. Insert the `source_batch_f2a` dedup row.
///
/// # Errors
///
/// Returns `ConstraintViolation` for shape / FK / check-constraint
/// failures (typically a malformed payload or an out-of-scope
/// provenance `memory_id`); `Internal` on sqlx failure.
#[allow(clippy::too_many_lines)]
pub async fn consolidate_batch_f2a(
    pool: &PgPool,
    req: &ConsolidateBatchF2ARequest<'_>,
) -> Result<ConsolidateBatchF2AOutcome, StorageError> {
    let output_sidecar_table = PgIdent::table(req.output_sidecar_table)?;

    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&req.owner);
    let batch_uuid = req.batch_id.into_inner();
    let personality_id = req.personality.personality_id.as_str().to_string();
    let personality_state_hash = req.personality.state_hash.into_inner();

    let mut tx = pool.begin().await.map_err(map_err)?;

    // 1. dedup pre-check.
    let existing: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT batch_id FROM proxima_core.source_batch_f2a \
         WHERE batch_id = $1 \
           AND operator_id = $2 \
           AND prompt_version = $3 \
           AND model_id = $4 \
           AND personality_id = $5 \
           AND personality_state_hash = $6",
    )
    .bind(batch_uuid)
    .bind(req.operator_id)
    .bind(req.prompt_version)
    .bind(req.model_id)
    .bind(&personality_id)
    .bind(&personality_state_hash[..])
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?;

    if existing.is_some() {
        return Ok(ConsolidateBatchF2AOutcome {
            abstraction_ids: Vec::new(),
            already_consolidated: true,
        });
    }

    let mut abstraction_ids: Vec<MemoryId> = Vec::with_capacity(req.abstractions.len());

    for abs in req.abstractions {
        let memory_id = uuid::Uuid::now_v7();
        abstraction_ids.push(MemoryId::new(memory_id));

        // 2. substrate memory row (Abstraction).
        //
        // operator_kind is the substrate-fixed enum (`'FtoA'` |
        // `'AtoP'`), not the operator's stable id. Per doc 04
        // §"Idempotence", the operator's lineage is keyed on
        // `(operator_kind, schema_id, prompt_version, model_id)` —
        // the operator_id (`proxima-code/commit-summary`) lives on
        // `source_batch_f2a` instead.
        sqlx::query(
            "INSERT INTO proxima_core.memories \
                (memory_id, owner_principal_kind, owner_principal_id, owner_org_id, \
                 schema_id, schema_version, kind, text, operator_kind, model_id, prompt_version, \
                 personality_id, personality_state_hash) \
             VALUES ($1, $2, $3, $4, $5, $6, 'Abstraction', $7, 'FtoA', $8, $9, $10, $11)",
        )
        .bind(memory_id)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(abs.schema_id.as_str())
        .bind(i32::try_from(abs.schema_version.into_inner()).unwrap_or(1))
        .bind(&abs.text)
        .bind(req.model_id)
        .bind(req.prompt_version)
        .bind(&personality_id)
        .bind(&personality_state_hash[..])
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        // 3. typed sidecar via jsonb_populate_record. The payload's
        //    JSON object plus `memory_id` is cast into the sidecar's
        //    row type and INSERTed.
        let sidecar_sql = format!(
            "INSERT INTO {sidecar} \
             SELECT * FROM jsonb_populate_record( \
                 NULL::{sidecar}, \
                 ($1::jsonb || jsonb_build_object('memory_id', $2::uuid)) \
             )",
            sidecar = output_sidecar_table.as_str(),
        );
        sqlx::query(&sidecar_sql)
            .bind(&abs.typed_payload)
            .bind(memory_id)
            .execute(&mut *tx)
            .await
            .map_err(map_err)?;

        // 6a. EntityAppend change_event for the new Abstraction.
        let change_seq = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.change_event \
                (seq, owner_principal_kind, owner_principal_id, owner_org_id, kind, \
                 entity_kind, entity_memory_id, entity_schema_id, entity_schema_version, \
                 entity_personality_id) \
             VALUES ($1, $2, $3, $4, 'EntityAppend', 'Abstraction', $5, $6, $7, $8)",
        )
        .bind(change_seq)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(memory_id)
        .bind(abs.schema_id.as_str())
        .bind(i32::try_from(abs.schema_version.into_inner()).unwrap_or(1))
        .bind(&personality_id)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        // 4. provenance edges A→F (one per source Fact).
        for prov_id in &abs.provenance {
            let edge_id = uuid::Uuid::now_v7();
            let draft = EdgeDraft {
                edge_id,
                relation: req.provenance_relation,
                source_kind: "Abstraction",
                source_memory_id: Some(memory_id),
                source_goal_id: None,
                target_kind: "Fact",
                target_memory_id: Some(prov_id.into_inner()),
                target_goal_id: None,
                authorship_kind: "OperatorFtoA",
                authorship_owner_memory_id: Some(memory_id),
                owner: &req.owner,
            };
            append_edge_in_tx(&mut tx, &draft, None).await?;
        }

        // 5. embedding row.
        let dim = i32::try_from(abs.embedding.len())
            .map_err(|_| StorageError::ConstraintViolation("embedding dim too large".into()))?;
        sqlx::query(
            "INSERT INTO proxima_core.embeddings \
                (entity_kind, entity_id, embedding_version, model_id, vec, dim, \
                 owner_principal_kind, owner_principal_id, owner_org_id) \
             VALUES ('Abstraction', $1, 1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(memory_id)
        .bind(&abs.embedding_model_id)
        .bind(&abs.embedding)
        .bind(dim)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    }

    // 7. dedup row.
    let head_memory_id = abstraction_ids.last().map(|m| m.into_inner());
    sqlx::query(
        "INSERT INTO proxima_core.source_batch_f2a \
            (batch_id, operator_id, prompt_version, model_id, \
             personality_id, personality_state_hash, head_memory_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(batch_uuid)
    .bind(req.operator_id)
    .bind(req.prompt_version)
    .bind(req.model_id)
    .bind(&personality_id)
    .bind(&personality_state_hash[..])
    .bind(head_memory_id)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    tx.commit().await.map_err(map_err)?;

    Ok(ConsolidateBatchF2AOutcome {
        abstraction_ids,
        already_consolidated: false,
    })
}

/// Atomic A→P persistence. See `Storage::consolidate_a2p`.
///
/// # Errors
///
/// Returns `ConstraintViolation` for shape / FK / check-constraint failures
/// (typically a malformed payload or an out-of-scope provenance `memory_id`);
/// `Internal` on sqlx failure.
#[allow(clippy::too_many_lines)]
pub async fn consolidate_a2p(
    pool: &PgPool,
    req: &ConsolidateA2PRequest<'_>,
) -> Result<ConsolidateA2POutcome, StorageError> {
    let output_sidecar_table = PgIdent::table(req.output_sidecar_table)?;
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&req.owner);
    let personality_id = req.personality.personality_id.as_str().to_string();
    let personality_state_hash = req.personality.state_hash.into_inner();

    let mut tx = pool.begin().await.map_err(map_err)?;

    // a2p_invocations is principal-scoped (see migration comment) — skip
    // owner_org_id here so same-principal/different-org runs share the row.
    let existing: Option<(i32,)> = sqlx::query_as(
        "SELECT 1 FROM proxima_core.a2p_invocations \
         WHERE owner_principal_kind = $1 \
           AND owner_principal_id = $2 \
           AND operator_id = $3 \
           AND prompt_version = $4 \
           AND model_id = $5 \
           AND personality_id = $6 \
           AND personality_state_hash = $7 \
           AND context_hash = $8 \
           AND input_hash = $9",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(req.operator_id)
    .bind(req.prompt_version)
    .bind(req.model_id)
    .bind(&personality_id)
    .bind(&personality_state_hash[..])
    .bind(&req.context_hash[..])
    .bind(&req.input_hash[..])
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?;

    if existing.is_some() {
        return Ok(ConsolidateA2POutcome {
            perspective_ids: Vec::new(),
            already_consolidated: true,
        });
    }

    let mut perspective_ids: Vec<MemoryId> = Vec::with_capacity(req.perspectives.len());

    for perspective in req.perspectives {
        let memory_id = uuid::Uuid::now_v7();
        perspective_ids.push(MemoryId::new(memory_id));
        let prior_head = req.prior_head.map(MemoryId::into_inner);

        sqlx::query(
            "INSERT INTO proxima_core.memories \
                (memory_id, owner_principal_kind, owner_principal_id, owner_org_id, \
                 schema_id, schema_version, kind, text, operator_kind, model_id, prompt_version, \
                 personality_id, personality_state_hash, supersedes) \
             VALUES ($1, $2, $3, $4, $5, $6, 'Perspective', $7, 'AtoP', $8, $9, $10, $11, $12)",
        )
        .bind(memory_id)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(perspective.schema_id.as_str())
        .bind(i32::try_from(perspective.schema_version.into_inner()).unwrap_or(1))
        .bind(&perspective.text)
        .bind(req.model_id)
        .bind(req.prompt_version)
        .bind(&personality_id)
        .bind(&personality_state_hash[..])
        .bind(prior_head)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        let sidecar_sql = format!(
            "INSERT INTO {sidecar} \
             SELECT * FROM jsonb_populate_record( \
                 NULL::{sidecar}, \
                 ($1::jsonb || jsonb_build_object('memory_id', $2::uuid)) \
             )",
            sidecar = output_sidecar_table.as_str(),
        );
        sqlx::query(&sidecar_sql)
            .bind(&perspective.typed_payload)
            .bind(memory_id)
            .execute(&mut *tx)
            .await
            .map_err(map_err)?;

        let change_seq = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.change_event \
                (seq, owner_principal_kind, owner_principal_id, owner_org_id, kind, \
                 entity_kind, entity_memory_id, entity_schema_id, entity_schema_version, \
                 entity_personality_id, supersedes_memory_id) \
             VALUES ($1, $2, $3, $4, 'EntityAppend', 'Perspective', $5, $6, $7, $8, $9)",
        )
        .bind(change_seq)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(memory_id)
        .bind(perspective.schema_id.as_str())
        .bind(i32::try_from(perspective.schema_version.into_inner()).unwrap_or(1))
        .bind(&personality_id)
        .bind(prior_head)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        for prov_id in &perspective.provenance {
            let draft = EdgeDraft {
                edge_id: uuid::Uuid::now_v7(),
                relation: req.provenance_relation,
                source_kind: "Perspective",
                source_memory_id: Some(memory_id),
                source_goal_id: None,
                target_kind: "Abstraction",
                target_memory_id: Some(prov_id.into_inner()),
                target_goal_id: None,
                authorship_kind: "OperatorAtoP",
                authorship_owner_memory_id: Some(memory_id),
                owner: &req.owner,
            };
            append_edge_in_tx(&mut tx, &draft, None).await?;
        }

        if let Some(prior_head) = prior_head {
            let draft = EdgeDraft {
                edge_id: uuid::Uuid::now_v7(),
                relation: req.supersedes_relation,
                source_kind: "Perspective",
                source_memory_id: Some(memory_id),
                source_goal_id: None,
                target_kind: "Perspective",
                target_memory_id: Some(prior_head),
                target_goal_id: None,
                authorship_kind: "Engine",
                authorship_owner_memory_id: None,
                owner: &req.owner,
            };
            append_edge_in_tx(&mut tx, &draft, None).await?;
        }

        let dim = i32::try_from(perspective.embedding.len())
            .map_err(|_| StorageError::ConstraintViolation("embedding dim too large".into()))?;
        sqlx::query(
            "INSERT INTO proxima_core.embeddings \
                (entity_kind, entity_id, embedding_version, model_id, vec, dim, \
                 owner_principal_kind, owner_principal_id, owner_org_id) \
             VALUES ('Perspective', $1, 1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(memory_id)
        .bind(&perspective.embedding_model_id)
        .bind(&perspective.embedding)
        .bind(dim)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    }

    let head_memory_id = perspective_ids.last().map(|m| m.into_inner());
    sqlx::query(
        "INSERT INTO proxima_core.a2p_invocations \
            (owner_principal_kind, owner_principal_id, \
             operator_id, prompt_version, model_id, personality_id, personality_state_hash, \
             context_hash, input_hash, head_memory_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(req.operator_id)
    .bind(req.prompt_version)
    .bind(req.model_id)
    .bind(&personality_id)
    .bind(&personality_state_hash[..])
    .bind(&req.context_hash[..])
    .bind(&req.input_hash[..])
    .bind(head_memory_id)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    tx.commit().await.map_err(map_err)?;

    Ok(ConsolidateA2POutcome {
        perspective_ids,
        already_consolidated: false,
    })
}

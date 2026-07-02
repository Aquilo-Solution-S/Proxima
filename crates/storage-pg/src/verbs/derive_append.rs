//! Operator-derived memory append verb.

use proxima_core::llm::EMBEDDING_DIM;
use std::collections::BTreeSet;

use proxima_core::{
    DerivedEdgeSpec, EdgeAuthorshipKind, EntityKind, InputContractId, MemoryId, MemoryOperatorKind,
    OperatorId, Owner, OwnerRefKind, RelationClass, SchemaId, SchemaVersion, SourceBatchId,
    StorageError,
};
use sqlx::{Postgres, Transaction};

use crate::error::map_err;
use crate::sidecars::PgSidecarFuture;

type StoredEdgeProofRow = (
    String,
    EntityKind,
    uuid::Uuid,
    EntityKind,
    uuid::Uuid,
    EdgeAuthorshipKind,
    Option<uuid::Uuid>,
);
type InputProofRow = (uuid::Uuid, Option<EntityKind>, Option<uuid::Uuid>, bool);

#[derive(Debug, Clone)]
pub struct DerivedDraft<'a> {
    pub memory_id: uuid::Uuid,
    pub owner: Owner,
    pub kind: EntityKind,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub text: String,
    pub operator_kind: MemoryOperatorKind,
    pub operator_id: OperatorId,
    pub input_contract_id: InputContractId,
    pub source_batch_id: Option<SourceBatchId>,
    pub model_id: &'a str,
    pub prompt_version: &'a str,
    pub supersedes: Option<MemoryId>,
    pub embedding: Option<Vec<f32>>,
    pub embedding_model_id: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct DerivedOutcome {
    pub memory_id: MemoryId,
    pub idempotent_replay: bool,
}

/// Append one Derived row, optional typed sidecar, and one change event.
///
/// # Errors
///
/// Returns storage constraint/internal errors from Postgres.
pub(crate) async fn append_derived_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    sidecar: impl for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t DerivedOutcome,
    ) -> PgSidecarFuture<'t>,
) -> Result<DerivedOutcome, StorageError> {
    let (owner_kind, owner_id) = crate::access::owner_columns::owner_binds(&draft.owner);
    if let Some(prior) = draft.supersedes {
        validate_supersedes_in_owner(tx, &draft.owner, prior, draft.kind).await?;
    }

    let inserted: Option<(uuid::Uuid,)> = sqlx::query_as(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id,
             model_id, prompt_version, supersedes)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
         ON CONFLICT (memory_id) DO NOTHING
         RETURNING memory_id",
    )
    .bind(draft.memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(draft.schema_id.as_str())
    .bind(i32::try_from(draft.schema_version.into_inner()).unwrap_or(1))
    .bind(draft.kind)
    .bind(&draft.text)
    .bind(draft.operator_kind)
    .bind(draft.operator_id.into_inner())
    .bind(draft.input_contract_id.into_inner())
    .bind(draft.source_batch_id.map(SourceBatchId::into_inner))
    .bind(draft.model_id)
    .bind(draft.prompt_version)
    .bind(draft.supersedes.map(MemoryId::into_inner))
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;

    if inserted.is_none() {
        validate_derived_replay_equivalent(tx, draft, owner_kind, owner_id).await?;
        return Ok(DerivedOutcome {
            memory_id: MemoryId::new(draft.memory_id),
            idempotent_replay: true,
        });
    }
    let outcome = DerivedOutcome {
        memory_id: MemoryId::new(draft.memory_id),
        idempotent_replay: false,
    };
    sidecar(tx, &outcome).await?;

    insert_embedding_in_tx(tx, draft, owner_kind, owner_id).await?;

    let seq = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_kind, owner_id,
             kind, entity_kind, entity_memory_id, entity_schema_id, entity_schema_version,
             supersedes_memory_id)
         VALUES ($1, $2, $3, 'EntityAppend', $4, $5, $6, $7, $8)",
    )
    .bind(seq)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(draft.kind)
    .bind(draft.memory_id)
    .bind(draft.schema_id.as_str())
    .bind(i32::try_from(draft.schema_version.into_inner()).unwrap_or(1))
    .bind(draft.supersedes.map(MemoryId::into_inner))
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    Ok(outcome)
}

/// Append one operator-derived memory with its declared output→input ledger
/// edges in the same transaction.
///
/// Flavor-SDK in-tx write tier: validates the operator proof ledger (edge
/// shape, input liveness, F→A batch closure, supersedes owner/kind) but does
/// NOT authorize the caller against the owner. Stays `pub` because
/// `proxima-code` persists derived memories inside multi-write transactions
/// through it (`ingest::blobs`, `mcp::emit_execution_request`); sealing this
/// tier behind a permit is flavor-boundary work, not the engine-verb proof
/// gate.
///
/// # Errors
///
/// Returns `ConstraintViolation` when the operator proof shape does not match
/// persisted input rows, `Conflict` when an idempotent replay changes proof
/// metadata or ledger edges, and storage errors from Postgres.
pub async fn append_derived_with_edges_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    edges: &[DerivedEdgeSpec<'_>],
    sidecar: impl for<'t> FnOnce(
        &'t mut Transaction<'_, Postgres>,
        &'t DerivedOutcome,
    ) -> PgSidecarFuture<'t>,
) -> Result<DerivedOutcome, StorageError> {
    validate_derived_draft_edges_in_tx(tx, draft, edges).await?;
    let outcome = append_derived_in_tx(tx, draft, sidecar).await?;
    if outcome.idempotent_replay {
        validate_derived_edge_replay_equivalent(tx, draft, edges).await?;
        return Ok(outcome);
    }
    for edge in edges {
        let draft = edge_draft_from_spec(edge);
        crate::verbs::edge_append::append_edge_in_tx(tx.as_mut(), &draft).await?;
    }
    Ok(outcome)
}

fn edge_draft_from_spec<'a>(
    edge: &'a DerivedEdgeSpec<'a>,
) -> crate::verbs::edge_append::EdgeDraft<'a> {
    crate::verbs::edge_append::EdgeDraft {
        edge_id: uuid::Uuid::now_v7(),
        relation: edge.relation,
        source_kind: edge.source_kind,
        source_memory_id: Some(edge.source_memory_id.into_inner()),
        source_goal_id: None,
        source_fact_entity_id: None,
        target_kind: edge.target_kind,
        target_memory_id: Some(edge.target_memory_id.into_inner()),
        target_goal_id: None,
        target_fact_entity_id: None,
        authorship_kind: edge.authorship_kind,
        authorship_owner_memory_id: edge.authorship_owner_memory_id.map(MemoryId::into_inner),
        owner: edge.owner,
    }
}

async fn validate_derived_draft_edges_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    edges: &[DerivedEdgeSpec<'_>],
) -> Result<(), StorageError> {
    let expected_authorship = draft.operator_kind.edge_authorship();
    let expected_input_kind = draft.operator_kind.phase().input_kind();
    validate_no_wrong_phase_operator_edges(draft, edges, expected_authorship)?;
    let input_ids = validate_proof_edges_and_collect_inputs(
        draft,
        edges,
        expected_authorship,
        expected_input_kind,
    )?;
    let rows = load_live_input_proof_rows_in_tx(tx, &input_ids).await?;
    let ftoa_batch = validate_input_proof_rows(draft, rows, input_ids.len(), expected_input_kind)?;
    validate_derived_source_batch(draft, ftoa_batch)
}

fn validate_no_wrong_phase_operator_edges(
    draft: &DerivedDraft<'_>,
    edges: &[DerivedEdgeSpec<'_>],
    expected_authorship: EdgeAuthorshipKind,
) -> Result<(), StorageError> {
    for edge in edges {
        if edge.source_memory_id.into_inner() == draft.memory_id
            && edge.relation.descriptor.class == RelationClass::Provenance
            && edge.authorship_kind.is_operator()
            && edge.authorship_kind != expected_authorship
        {
            return Err(StorageError::ConstraintViolation(
                "operator provenance edge authorship kind does not match operator phase".into(),
            ));
        }
    }
    Ok(())
}

fn validate_proof_edges_and_collect_inputs(
    draft: &DerivedDraft<'_>,
    edges: &[DerivedEdgeSpec<'_>],
    expected_authorship: EdgeAuthorshipKind,
    expected_input_kind: EntityKind,
) -> Result<Vec<uuid::Uuid>, StorageError> {
    let mut input_ids = Vec::new();
    let mut seen = BTreeSet::new();
    for edge in edges.iter().filter(|edge| {
        edge.source_memory_id.into_inner() == draft.memory_id
            && edge.authorship_kind == expected_authorship
    }) {
        if edge.relation.descriptor.class != RelationClass::Provenance {
            return Err(StorageError::ConstraintViolation(
                "operator proof edges must use Provenance relations".into(),
            ));
        }
        if edge.source_kind != draft.kind || edge.target_kind != expected_input_kind {
            return Err(StorageError::ConstraintViolation(
                "operator proof edge shape does not match operator phase".into(),
            ));
        }
        if !seen.insert(edge.target_memory_id) {
            return Err(StorageError::ConstraintViolation(
                "operator invocation inputs must be unique".into(),
            ));
        }
        input_ids.push(edge.target_memory_id.into_inner());
    }
    if input_ids.is_empty() {
        return Err(StorageError::ConstraintViolation(
            "operator invocation inputs must be nonempty".into(),
        ));
    }
    Ok(input_ids)
}

async fn load_live_input_proof_rows_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    input_ids: &[uuid::Uuid],
) -> Result<Vec<InputProofRow>, StorageError> {
    sqlx::query_as(
        "SELECT m.memory_id, m.kind, fr.source_batch_id, sb.closed_at IS NOT NULL
           FROM proxima_core.memories m
           LEFT JOIN proxima_core.fact_receipts fr ON fr.receipt_id = m.receipt_id
           LEFT JOIN proxima_core.source_batches sb ON sb.id = fr.source_batch_id
          WHERE m.memory_id = ANY($1::uuid[])
            AND m.tombstoned_at IS NULL",
    )
    .bind(input_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)
}

fn validate_input_proof_rows(
    draft: &DerivedDraft<'_>,
    rows: Vec<InputProofRow>,
    expected_len: usize,
    expected_input_kind: EntityKind,
) -> Result<Option<uuid::Uuid>, StorageError> {
    if rows.len() != expected_len {
        return Err(StorageError::ConstraintViolation(
            "operator invocation inputs must exist and be live".into(),
        ));
    }
    let mut ftoa_batch = None;
    for (memory_id, actual_kind, source_batch_id, closed) in rows {
        let actual = actual_kind.unwrap_or(EntityKind::Fact);
        if actual != expected_input_kind {
            return Err(StorageError::ConstraintViolation(format!(
                "invalid input kind for {:?}: expected {expected_input_kind:?}, got {actual:?}",
                MemoryId::new(memory_id)
            )));
        }
        if draft.operator_kind == MemoryOperatorKind::FtoA {
            ftoa_batch = Some(validate_ftoa_input_batch(
                source_batch_id,
                closed,
                ftoa_batch,
            )?);
        }
    }
    Ok(ftoa_batch)
}

fn validate_ftoa_input_batch(
    source_batch_id: Option<uuid::Uuid>,
    closed: bool,
    existing_batch: Option<uuid::Uuid>,
) -> Result<uuid::Uuid, StorageError> {
    let source_batch_id = source_batch_id.ok_or_else(|| {
        StorageError::ConstraintViolation("F→A operator inputs must be receipted Facts".into())
    })?;
    if !closed {
        return Err(StorageError::ConstraintViolation(
            "F→A operator source batch must be closed".into(),
        ));
    }
    match existing_batch {
        Some(existing) if existing != source_batch_id => Err(StorageError::ConstraintViolation(
            "F→A operator inputs must belong to one source batch".into(),
        )),
        Some(existing) => Ok(existing),
        None => Ok(source_batch_id),
    }
}

fn validate_derived_source_batch(
    draft: &DerivedDraft<'_>,
    ftoa_batch: Option<uuid::Uuid>,
) -> Result<(), StorageError> {
    if draft.operator_kind == MemoryOperatorKind::FtoA {
        let expected = ftoa_batch.ok_or_else(|| {
            StorageError::ConstraintViolation("F→A operator source batch is required".into())
        })?;
        if draft.source_batch_id.map(SourceBatchId::into_inner) != Some(expected) {
            return Err(StorageError::ConstraintViolation(
                "F→A operator source_batch_id must match input Facts".into(),
            ));
        }
    } else if draft.source_batch_id.is_some() {
        return Err(StorageError::ConstraintViolation(
            "source_batch_id is only valid for F→A operator invocations".into(),
        ));
    }
    Ok(())
}

fn operator_edge_authorship_values() -> [&'static str; 4] {
    [
        EdgeAuthorshipKind::OperatorFtoA.as_str(),
        EdgeAuthorshipKind::OperatorAtoA.as_str(),
        EdgeAuthorshipKind::OperatorAtoP.as_str(),
        EdgeAuthorshipKind::OperatorAtoGoal.as_str(),
    ]
}

async fn validate_derived_edge_replay_equivalent(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    edges: &[DerivedEdgeSpec<'_>],
) -> Result<(), StorageError> {
    let expected_authorship = draft.operator_kind.edge_authorship();
    let expected = edges
        .iter()
        .filter(|edge| {
            edge.source_memory_id.into_inner() == draft.memory_id
                && edge.authorship_kind == expected_authorship
                && edge.relation.descriptor.class == RelationClass::Provenance
        })
        .map(|edge| {
            (
                edge.relation.descriptor.relation.as_str().to_string(),
                format!("{:?}", edge.source_kind),
                edge.source_memory_id.into_inner(),
                format!("{:?}", edge.target_kind),
                edge.target_memory_id.into_inner(),
                format!("{:?}", edge.authorship_kind),
                edge.authorship_owner_memory_id.map(MemoryId::into_inner),
            )
        })
        .collect::<BTreeSet<_>>();
    let rows: Vec<StoredEdgeProofRow> = sqlx::query_as(
        "SELECT relation, source_kind, source_memory_id, target_kind, target_memory_id,
                authorship_kind, authorship_owner_memory_id
           FROM proxima_core.edges
          WHERE source_memory_id = $1
            AND relation_class = 'Provenance'
            AND authorship_kind::text = ANY($2::text[])",
    )
    .bind(draft.memory_id)
    .bind(operator_edge_authorship_values())
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    let actual = rows
        .into_iter()
        .map(
            |(
                relation,
                source_kind,
                source_memory_id,
                target_kind,
                target_memory_id,
                authorship_kind,
                authorship_owner_memory_id,
            )| {
                (
                    relation,
                    format!("{source_kind:?}"),
                    source_memory_id,
                    format!("{target_kind:?}"),
                    target_memory_id,
                    format!("{authorship_kind:?}"),
                    authorship_owner_memory_id,
                )
            },
        )
        .collect::<BTreeSet<_>>();
    if actual == expected {
        Ok(())
    } else {
        Err(StorageError::Conflict(
            "derived memory idempotent replay edge proof mismatch".into(),
        ))
    }
}

async fn validate_derived_replay_equivalent(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    owner_kind: OwnerRefKind,
    owner_id: Option<uuid::Uuid>,
) -> Result<(), StorageError> {
    let equivalent: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.memories
              WHERE memory_id = $1
                AND owner_kind = $2
                AND owner_id IS NOT DISTINCT FROM $3
                AND schema_id = $4
                AND schema_version = $5
                AND kind = $6
                AND text = $7
                AND operator_kind = $8
                AND operator_id = $9
                AND input_contract_id = $10
                AND source_batch_id IS NOT DISTINCT FROM $11
                AND model_id = $12
                AND prompt_version = $13
                AND supersedes IS NOT DISTINCT FROM $14
                AND receipt_id IS NULL
                AND citation_mapping_id IS NULL
                AND tombstoned_at IS NULL
         )",
    )
    .bind(draft.memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(draft.schema_id.as_str())
    .bind(i32::try_from(draft.schema_version.into_inner()).unwrap_or(1))
    .bind(draft.kind)
    .bind(&draft.text)
    .bind(draft.operator_kind)
    .bind(draft.operator_id.into_inner())
    .bind(draft.input_contract_id.into_inner())
    .bind(draft.source_batch_id.map(SourceBatchId::into_inner))
    .bind(draft.model_id)
    .bind(draft.prompt_version)
    .bind(draft.supersedes.map(MemoryId::into_inner))
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;

    if equivalent {
        Ok(())
    } else {
        Err(StorageError::Conflict(
            "derived memory idempotent replay proof mismatch".into(),
        ))
    }
}

async fn validate_supersedes_in_owner(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    prior: MemoryId,
    kind: EntityKind,
) -> Result<(), StorageError> {
    let (owner_kind, owner_id) = owner.columns();
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.memories m
              WHERE m.memory_id = $1
                AND m.tombstoned_at IS NULL
                AND m.kind = $4
                AND EXISTS (
                    SELECT 1
                      FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo
                     WHERE eo.entity_id = m.memory_id
                       AND eo.owner_kind = $2
                       AND eo.owner_id = $3
)
         )",
    )
    .bind(prior.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(kind)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    if exists {
        Ok(())
    } else {
        Err(StorageError::ConstraintViolation(
            "supersedes crosses Owner boundary or does not exist".into(),
        ))
    }
}

async fn insert_embedding_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    _owner_kind: OwnerRefKind,
    _owner_id: Option<uuid::Uuid>,
) -> Result<(), StorageError> {
    let (Some(embedding), Some(embedding_model_id)) = (&draft.embedding, draft.embedding_model_id)
    else {
        return Ok(());
    };
    crate::verbs::fact_embeddings::insert_memory_embedding(
        tx,
        &draft.owner,
        draft.kind,
        MemoryId::new(draft.memory_id),
        embedding_model_id,
        EMBEDDING_DIM,
        embedding,
    )
    .await?;
    Ok(())
}

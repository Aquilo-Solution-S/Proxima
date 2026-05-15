//! Atomic `persist_wake_trace` storage verb.

use proxima_core::verbs::persist_wake_trace::{WakeTracePersistInput, WakeTracePersistOutcome};
use proxima_core::{
    CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION, FlavorRegistryFrozen, MemoryId, Principal,
    StorageError,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::map_err;
use crate::verbs::edge_append::{EdgeDraft, append_edge_in_tx};

const WAKE_TRACE_FACT_SCHEMA: &str = "proxima-core/wake-trace-v1";
const WAKE_TRACE_JSONL_SCHEMA: &str = "proxima-core/wake-trace-jsonl-v1";
const WAKE_TRACE_CITATION_SCHEMA: &str = "proxima-core/wake-trace-citation-v1";

/// Persist one wake trace in a new transaction.
///
/// # Errors
///
/// Returns [`StorageError`] if validation fails or any storage write fails.
pub async fn persist_wake_trace_atomic(
    pool: &PgPool,
    registry: &FlavorRegistryFrozen,
    input: &WakeTracePersistInput,
) -> Result<WakeTracePersistOutcome, StorageError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
    let outcome = persist_wake_trace_in_tx(&mut tx, registry, input).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

/// Persist one wake trace using an existing transaction.
///
/// # Errors
///
/// Returns [`StorageError`] if validation fails or any storage write fails.
#[allow(clippy::too_many_lines)]
pub async fn persist_wake_trace_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    registry: &FlavorRegistryFrozen,
    input: &WakeTracePersistInput,
) -> Result<WakeTracePersistOutcome, StorageError> {
    validate_jsonl_content_hash(input)?;

    let event_id = input.event_id();
    let event_id_bytes = event_id.into_inner();

    let (owner_kind, owner_principal_id) = match &input.owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    let owner_org_id = input.owner.org_id.into_inner();

    validate_memory_ref_owner(
        tx.as_mut(),
        input.root_perspective_memory_id.into_inner(),
        "Perspective",
        owner_kind,
        owner_principal_id,
        owner_org_id,
        "root perspective",
    )
    .await?;
    validate_memory_ref_owner(
        tx.as_mut(),
        input.triggering_memory_id.into_inner(),
        "Fact",
        owner_kind,
        owner_principal_id,
        owner_org_id,
        "triggering memory",
    )
    .await?;
    for goal_id in &input.active_goal_ids {
        validate_goal_ref_owner(
            tx.as_mut(),
            goal_id.into_inner(),
            owner_kind,
            owner_principal_id,
            owner_org_id,
        )
        .await?;
    }
    ensure_source_batch_owner(
        tx.as_mut(),
        input.source_batch_id.into_inner(),
        input.source_id.as_str(),
        owner_kind,
        owner_principal_id,
        owner_org_id,
    )
    .await?;

    let existing: Option<(uuid::Uuid, uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
        "SELECT m.memory_id, m.citation_mapping_id, cm.cited_object_id \
         FROM proxima_core.memories m \
         JOIN proxima_core.citation_mappings cm \
           ON cm.citation_mapping_id = m.citation_mapping_id \
         WHERE m.event_id = $1",
    )
    .bind(&event_id_bytes[..])
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;

    if let Some((memory_id, citation_mapping_id, cited_object_id)) = existing {
        let (seq,): (uuid::Uuid,) = sqlx::query_as(
            "SELECT seq FROM proxima_core.change_event \
             WHERE entity_memory_id = $1 ORDER BY seq ASC LIMIT 1",
        )
        .bind(memory_id)
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?;

        return Ok(WakeTracePersistOutcome {
            event_id,
            fact_memory_id: MemoryId::new(memory_id),
            cited_object_id,
            citation_mapping_id,
            change_event_seq: seq,
            idempotent_replay: true,
        });
    }

    let cited_object_id = uuid::Uuid::now_v7();
    let citation_mapping_id = uuid::Uuid::now_v7();
    let memory_id = uuid::Uuid::now_v7();
    let change_seq = uuid::Uuid::now_v7();

    let cited_object_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.cited_objects \
            (cited_object_id, schema_id, owner_principal_kind, \
             owner_principal_id, owner_org_id, content_hash) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (owner_principal_kind, owner_principal_id, \
                      owner_org_id, schema_id, content_hash) \
         DO UPDATE SET schema_id = EXCLUDED.schema_id \
         RETURNING cited_object_id",
    )
    .bind(cited_object_id)
    .bind(WAKE_TRACE_JSONL_SCHEMA)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&input.jsonl_content_hash[..])
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.cited_wake_trace_jsonl_v1 \
            (cited_object_id, byte_len, line_count, truncated, storage_path, body) \
         VALUES ($1, $2, $3, $4, NULL, $5) \
         ON CONFLICT (cited_object_id) DO NOTHING",
    )
    .bind(cited_object_id)
    .bind(i64::try_from(input.jsonl_bytes.len()).unwrap_or(i64::MAX))
    .bind(i64::try_from(input.jsonl_line_count).unwrap_or(i64::MAX))
    .bind(input.jsonl_truncated)
    .bind(&input.jsonl_bytes[..])
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.events \
            (event_id, source_id, source_batch_id, \
             owner_principal_kind, owner_principal_id, owner_org_id, \
             schema_id, schema_version, observed_at, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8, $9)",
    )
    .bind(&event_id_bytes[..])
    .bind(input.source_id.as_str())
    .bind(input.source_batch_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(WAKE_TRACE_FACT_SCHEMA)
    .bind(input.observed_at)
    .bind(input.occurred_at)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.memories \
            (memory_id, owner_principal_kind, owner_principal_id, \
             owner_org_id, schema_id, schema_version, event_id, citation_mapping_id, \
             personality_instance_id) \
         VALUES ($1, $2, $3, $4, $5, 1, $6, $7, $8)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(WAKE_TRACE_FACT_SCHEMA)
    .bind(&event_id_bytes[..])
    .bind(citation_mapping_id)
    .bind(input.authoring_personality_instance_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.citation_mappings \
            (citation_mapping_id, schema_id, memory_id, \
             cited_object_id, owner_principal_kind, \
             owner_principal_id, owner_org_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(citation_mapping_id)
    .bind(WAKE_TRACE_CITATION_SCHEMA)
    .bind(memory_id)
    .bind(cited_object_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    let (range_start, range_end) = input.citation_byte_range.map_or((None, None), |(a, b)| {
        (
            Some(i64::try_from(a).unwrap_or(i64::MAX)),
            Some(i64::try_from(b).unwrap_or(i64::MAX)),
        )
    });
    sqlx::query(
        "INSERT INTO proxima_core.citation_wake_trace_v1 \
            (citation_mapping_id, byte_range_start, byte_range_end) \
         VALUES ($1, $2, $3)",
    )
    .bind(citation_mapping_id)
    .bind(range_start)
    .bind(range_end)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    let wt = &input.wake_trace;
    sqlx::query(
        "INSERT INTO proxima_core.wake_trace_v1 \
            (memory_id, invocation_id, wake_entry_id, personality_instance_id, \
             model_target_ref, model_id, started_at, finished_at, \
             outcome_kind, failure_reason, rounds_used, finish_reason, \
             total_prompt_tokens, total_completion_tokens, tool_call_count, \
             jsonl_truncated) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
    )
    .bind(memory_id)
    .bind(wt.invocation_id)
    .bind(wt.wake_entry_id)
    .bind(wt.personality_instance_id)
    .bind(&wt.model_target_ref)
    .bind(&wt.model_id)
    .bind(wt.started_at)
    .bind(wt.finished_at)
    .bind(&wt.outcome_kind)
    .bind(wt.failure_reason.as_deref())
    .bind(i32::try_from(wt.rounds_used).unwrap_or(i32::MAX))
    .bind(wt.finish_reason.as_deref())
    .bind(
        wt.total_prompt_tokens
            .map(|t| i64::try_from(t).unwrap_or(i64::MAX)),
    )
    .bind(
        wt.total_completion_tokens
            .map(|t| i64::try_from(t).unwrap_or(i64::MAX)),
    )
    .bind(i32::try_from(wt.tool_call_count).unwrap_or(i32::MAX))
    .bind(wt.jsonl_truncated)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.change_event \
            (seq, owner_principal_kind, owner_principal_id, \
             owner_org_id, kind, entity_kind, \
             entity_memory_id, entity_schema_id, entity_schema_version, \
             entity_personality_instance_id) \
         VALUES ($1, $2, $3, $4, 'EntityAppend', 'Fact', $5, $6, 1, $7)",
    )
    .bind(change_seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(memory_id)
    .bind(WAKE_TRACE_FACT_SCHEMA)
    .bind(input.authoring_personality_instance_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    let authored_relation = registry
        .resolve_relation(CORE_AUTHORED_RELATION)
        .ok_or_else(|| {
            StorageError::Internal(format!(
                "relation {CORE_AUTHORED_RELATION} missing from frozen registry"
            ))
        })?;
    let authored = EdgeDraft {
        edge_id: uuid::Uuid::now_v7(),
        relation: authored_relation,
        source_kind: "Perspective",
        source_memory_id: Some(input.root_perspective_memory_id.into_inner()),
        source_goal_id: None,
        target_kind: "Fact",
        target_memory_id: Some(memory_id),
        target_goal_id: None,
        authorship_kind: "Engine",
        authorship_owner_memory_id: None,
        owner: &input.owner,
    };
    append_edge_in_tx(tx.as_mut(), &authored, None).await?;

    let derived_relation = registry
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| {
            StorageError::Internal(format!(
                "relation {CORE_DERIVED_FROM_RELATION} missing from frozen registry"
            ))
        })?;
    let derived_to_trigger = EdgeDraft {
        edge_id: uuid::Uuid::now_v7(),
        relation: derived_relation,
        source_kind: "Fact",
        source_memory_id: Some(memory_id),
        source_goal_id: None,
        target_kind: "Fact",
        target_memory_id: Some(input.triggering_memory_id.into_inner()),
        target_goal_id: None,
        authorship_kind: "Engine",
        authorship_owner_memory_id: None,
        owner: &input.owner,
    };
    append_edge_in_tx(tx.as_mut(), &derived_to_trigger, None).await?;

    for goal_id in &input.active_goal_ids {
        let derived_to_goal = EdgeDraft {
            edge_id: uuid::Uuid::now_v7(),
            relation: derived_relation,
            source_kind: "Fact",
            source_memory_id: Some(memory_id),
            source_goal_id: None,
            target_kind: "Goal",
            target_memory_id: None,
            target_goal_id: Some(goal_id.into_inner()),
            authorship_kind: "Engine",
            authorship_owner_memory_id: None,
            owner: &input.owner,
        };
        append_edge_in_tx(tx.as_mut(), &derived_to_goal, None).await?;
    }

    Ok(WakeTracePersistOutcome {
        event_id,
        fact_memory_id: MemoryId::new(memory_id),
        cited_object_id,
        citation_mapping_id,
        change_event_seq: change_seq,
        idempotent_replay: false,
    })
}

fn validate_jsonl_content_hash(input: &WakeTracePersistInput) -> Result<(), StorageError> {
    let actual = blake3::hash(&input.jsonl_bytes);
    if actual.as_bytes() != &input.jsonl_content_hash {
        return Err(StorageError::ConstraintViolation(
            "wake trace JSONL content hash does not match body".to_string(),
        ));
    }
    Ok(())
}

async fn validate_memory_ref_owner(
    tx: &mut sqlx::PgConnection,
    memory_id: uuid::Uuid,
    expected_kind: &str,
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
    owner_org_id: uuid::Uuid,
    label: &str,
) -> Result<(), StorageError> {
    let row: Option<(String, uuid::Uuid, uuid::Uuid, String)> = sqlx::query_as(
        "SELECT owner_principal_kind, owner_principal_id, owner_org_id, \
                COALESCE(kind, 'Fact') \
         FROM proxima_core.memories \
         WHERE memory_id = $1",
    )
    .bind(memory_id)
    .fetch_optional(tx)
    .await
    .map_err(map_err)?;

    let Some((row_kind, row_principal_id, row_org_id, row_memory_kind)) = row else {
        return Err(StorageError::NotFound);
    };
    if row_kind != owner_kind
        || row_principal_id != owner_principal_id
        || row_org_id != owner_org_id
    {
        return Err(StorageError::ConstraintViolation(format!(
            "{label} crosses Owner boundary"
        )));
    }
    if row_memory_kind != expected_kind {
        return Err(StorageError::ConstraintViolation(format!(
            "{label} must be {expected_kind}"
        )));
    }
    Ok(())
}

async fn validate_goal_ref_owner(
    tx: &mut sqlx::PgConnection,
    goal_id: uuid::Uuid,
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
    owner_org_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let row: Option<(String, uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
        "SELECT owner_principal_kind, owner_principal_id, owner_org_id \
         FROM proxima_core.goals \
         WHERE goal_id = $1",
    )
    .bind(goal_id)
    .fetch_optional(tx)
    .await
    .map_err(map_err)?;

    let Some((row_kind, row_principal_id, row_org_id)) = row else {
        return Err(StorageError::NotFound);
    };
    if row_kind != owner_kind
        || row_principal_id != owner_principal_id
        || row_org_id != owner_org_id
    {
        return Err(StorageError::ConstraintViolation(
            "active goal crosses Owner boundary".to_string(),
        ));
    }
    Ok(())
}

async fn ensure_source_batch_owner(
    tx: &mut sqlx::PgConnection,
    source_batch_id: uuid::Uuid,
    source_id: &str,
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
    owner_org_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let row: Option<(String, String, uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
        "SELECT source_id, owner_principal_kind, owner_principal_id, owner_org_id \
         FROM proxima_core.source_batches \
         WHERE id = $1",
    )
    .bind(source_batch_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?;

    match row {
        None => {
            sqlx::query(
                "INSERT INTO proxima_core.source_batches \
                    (id, source_id, owner_principal_kind, \
                     owner_principal_id, owner_org_id) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(source_batch_id)
            .bind(source_id)
            .bind(owner_kind)
            .bind(owner_principal_id)
            .bind(owner_org_id)
            .execute(tx)
            .await
            .map_err(map_err)?;
            Ok(())
        }
        Some((row_source_id, row_kind, row_principal_id, row_org_id))
            if row_source_id == source_id
                && row_kind == owner_kind
                && row_principal_id == owner_principal_id
                && row_org_id == owner_org_id =>
        {
            Ok(())
        }
        Some(_) => Err(StorageError::ConstraintViolation(
            "source batch id collides across Owner or source".to_string(),
        )),
    }
}

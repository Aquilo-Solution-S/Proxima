// Workspace review ingestion functions
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use proxima_core::{
    relation::CORE_DERIVED_FROM_RELATION, MemoryId, SchemaId, SchemaVersion, SourceBatchId,
    SourceId,
};
use proxima_core::mcp::{McpToolCtx, McpToolError};
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use proxima_core::FactPayload;

use crate::payloads::WorkspaceReviewV1;

use crate::mcp::sql::map_storage;

/// Ingest a workspace review Fact.
///
/// # Errors
///
/// Returns an error if serialization fails or event ingestion fails.
pub async fn ingest_workspace_review(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    payload: &WorkspaceReviewV1,
) -> Result<proxima_core::verbs::event_ingest::EventIngestOutcome, McpToolError> {
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes)
        .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
    let content_hash = blake3::hash(&payload_bytes);
    let observed_at = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new(super::WORKSPACE_REVIEW_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: ctx.owner.clone(),
        schema_id: SchemaId::new(WorkspaceReviewV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(WorkspaceReviewV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at,
        occurred_at: payload.reviewed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(super::WORKSPACE_REVIEW_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(super::WORKSPACE_REVIEW_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    ingest_event_in_tx(tx, &draft)
        .await
        .map_err(McpToolError::Storage)
}

/// Insert workspace review sidecar row.
///
/// # Errors
///
/// Returns an error if serialization or database insertion fails.
pub async fn insert_workspace_review_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &WorkspaceReviewV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_code.workspace_review_v1
            (memory_id, workspace_run_memory_id, execution_request_memory_id,
             verdict, round_index, summary, findings_json,
             correction_instructions, verification_summary, reviewed_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(memory_id.into_inner())
    .bind(payload.workspace_run_memory_id)
    .bind(payload.execution_request_memory_id)
    .bind(payload.verdict.as_str())
    .bind(i32::try_from(payload.round_index).unwrap_or(i32::MAX))
    .bind(&payload.summary)
    .bind(
        serde_json::to_value(&payload.findings)
            .map_err(|err| McpToolError::InvalidInput(format!("serialize findings: {err}")))?,
    )
    .bind(payload.correction_instructions.as_deref())
    .bind(payload.verification_summary.as_deref())
    .bind(payload.reviewed_at)
    .execute(&mut **tx)
    .await
    .map_err(map_storage)?;
    Ok(())
}

/// Append a derived edge from a review to a target.
///
/// # Errors
///
/// Returns an error if the relation is not registered or edge insertion fails.
pub async fn append_review_derived_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    review_memory_id: MemoryId,
    target_memory_id: MemoryId,
) -> Result<Uuid, McpToolError> {
    let relation = ctx
        .registry
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| McpToolError::Other("core/derived-from relation not registered".into()))?;
    let edge_id = Uuid::now_v7();
    append_edge_in_tx(
        tx,
        &EdgeDraft {
            edge_id,
            relation,
            source_kind: "Fact",
            source_memory_id: Some(review_memory_id.into_inner()),
            source_goal_id: None,
            target_kind: "Fact",
            target_memory_id: Some(target_memory_id.into_inner()),
            target_goal_id: None,
            authorship_kind: "ExternalAgent",
            authorship_owner_memory_id: ctx.caller_self_perspective.map(MemoryId::into_inner),
            owner: &ctx.owner,
        },
        None,
    )
    .await
    .map_err(McpToolError::Storage)?;
    Ok(edge_id)
}

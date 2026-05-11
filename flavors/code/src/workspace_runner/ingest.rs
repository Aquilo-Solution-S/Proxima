use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::{
    FactPayload, MemoryId, SchemaId, SchemaVersion, SourceBatchId, SourceId,
    WorkspaceFinalizeInput, WorkspaceRunnerError,
};
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use sqlx::PgPool;
use uuid::Uuid;

use crate::payloads::WorkspaceRunV1;

use super::{WORKSPACE_RUN_OBJECT_SCHEMA, WORKSPACE_RUN_WHOLE_SCHEMA, WORKSPACE_RUNNER_SOURCE_ID};

pub(super) async fn ingest_workspace_run(
    pool: &PgPool,
    payload: &WorkspaceRunV1,
    input: WorkspaceFinalizeInput<'_>,
) -> Result<MemoryId, WorkspaceRunnerError> {
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes).map_err(|err| {
        WorkspaceRunnerError::FinalizeFailed(format!("serialize workspace run: {err}"))
    })?;
    let content_hash = blake3::hash(&payload_bytes);
    let observed_at = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new(WORKSPACE_RUNNER_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: input.owner.clone(),
        schema_id: SchemaId::new(WorkspaceRunV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(WorkspaceRunV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at,
        occurred_at: observed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(WORKSPACE_RUN_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(WORKSPACE_RUN_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    };

    let mut tx = pool
        .begin()
        .await
        .map_err(|err| WorkspaceRunnerError::Internal(format!("begin workspace tx: {err}")))?;
    let outcome = ingest_event_in_tx(&mut tx, &draft)
        .await
        .map_err(|err| WorkspaceRunnerError::FinalizeFailed(format!("event ingest: {err}")))?;
    if !outcome.idempotent_replay {
        sqlx::query(
            "INSERT INTO proxima_code.workspace_run_v1
                (memory_id, wake_invocation_id, repo_id, target_branch,
                 worktree_path, branch_name, parent_sha, head_sha,
                 diff_stat_json, exit_code, stdout_tail, stderr_tail, duration_ms)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(outcome.memory_id.into_inner())
        .bind(payload.wake_invocation_id)
        .bind(payload.repo_id)
        .bind(&payload.target_branch)
        .bind(&payload.worktree_path)
        .bind(&payload.branch_name)
        .bind(&payload.parent_sha)
        .bind(&payload.head_sha)
        .bind(
            serde_json::to_value(&payload.diff_stat_json).map_err(|err| {
                WorkspaceRunnerError::FinalizeFailed(format!("serialize diff stat: {err}"))
            })?,
        )
        .bind(payload.exit_code)
        .bind(payload.stdout_tail.as_deref())
        .bind(payload.stderr_tail.as_deref())
        .bind(payload.duration_ms.and_then(|v| i64::try_from(v).ok()))
        .execute(&mut *tx)
        .await
        .map_err(|err| WorkspaceRunnerError::FinalizeFailed(format!("insert sidecar: {err}")))?;

        let authored = EdgeDraft {
            edge_id: Uuid::now_v7(),
            relation: input.authored_relation,
            source_kind: "Perspective",
            source_memory_id: Some(input.root_perspective_memory_id.into_inner()),
            source_goal_id: None,
            target_kind: "Fact",
            target_memory_id: Some(outcome.memory_id.into_inner()),
            target_goal_id: None,
            authorship_kind: "Engine",
            authorship_owner_memory_id: None,
            owner: input.owner,
        };
        append_edge_in_tx(&mut tx, &authored, None)
            .await
            .map_err(|err| {
                WorkspaceRunnerError::FinalizeFailed(format!("append authored edge: {err}"))
            })?;

        let derived = EdgeDraft {
            edge_id: Uuid::now_v7(),
            relation: input.derived_from_relation,
            source_kind: "Fact",
            source_memory_id: Some(outcome.memory_id.into_inner()),
            source_goal_id: None,
            target_kind: "Fact",
            target_memory_id: Some(input.triggering_memory_id.into_inner()),
            target_goal_id: None,
            authorship_kind: "EventSource",
            authorship_owner_memory_id: None,
            owner: input.owner,
        };
        append_edge_in_tx(&mut tx, &derived, None)
            .await
            .map_err(|err| {
                WorkspaceRunnerError::FinalizeFailed(format!("append derived edge: {err}"))
            })?;
    }
    tx.commit()
        .await
        .map_err(|err| WorkspaceRunnerError::Internal(format!("commit workspace tx: {err}")))?;
    Ok(outcome.memory_id)
}

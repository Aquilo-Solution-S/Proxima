use proxima_core::{
    CORE_WORKSPACE_RUN_SOURCE_ID, CoreWorkspaceRunV1, EdgeAuthorshipKind, EntityKind, MemoryId,
    SourceBatchId, SourceId, WorkspaceFinalizeInput, WorkspaceRunnerError,
    core_workspace_run_event_draft,
};
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use sqlx::PgPool;
use uuid::Uuid;

#[allow(clippy::too_many_lines)]
pub(super) async fn ingest_workspace_run(
    pool: &PgPool,
    payload: &CoreWorkspaceRunV1,
    input: WorkspaceFinalizeInput<'_>,
) -> Result<MemoryId, WorkspaceRunnerError> {
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes).map_err(|err| {
        WorkspaceRunnerError::FinalizeFailed(format!("serialize workspace run: {err}"))
    })?;
    let observed_at = time::OffsetDateTime::now_utc();
    let draft = core_workspace_run_event_draft(
        input.owner.clone(),
        &payload_bytes,
        SourceBatchId::new(Uuid::now_v7()),
        SourceId::new(CORE_WORKSPACE_RUN_SOURCE_ID.to_string()),
        observed_at,
    );

    let mut tx = pool
        .begin()
        .await
        .map_err(|err| WorkspaceRunnerError::Internal(format!("begin workspace tx: {err}")))?;
    let outcome = ingest_event_in_tx(&mut tx, &draft)
        .await
        .map_err(|err| WorkspaceRunnerError::FinalizeFailed(format!("event ingest: {err}")))?;
    if !outcome.idempotent_replay {
        sqlx::query(
            "INSERT INTO proxima_core.workspace_run_v1
                (memory_id, wake_invocation_id, wake_entry_id, personality_instance_id,
                 binding_kind, finalize, repo_path, base_ref, worktree_path, branch_name,
                 parent_sha, head_sha, committed, diff_stat_json,
                 exit_code, stdout_tail, stderr_tail, duration_ms)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)",
        )
        .bind(outcome.memory_id.into_inner())
        .bind(payload.wake_invocation_id)
        .bind(payload.wake_entry_id)
        .bind(payload.personality_instance_id)
        .bind(&payload.binding_kind)
        .bind(&payload.finalize)
        .bind(&payload.repo_path)
        .bind(&payload.base_ref)
        .bind(&payload.worktree_path)
        .bind(&payload.branch_name)
        .bind(&payload.parent_sha)
        .bind(&payload.head_sha)
        .bind(payload.committed)
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
            source_kind: EntityKind::Perspective,
            source_memory_id: Some(input.root_perspective_memory_id.into_inner()),
            source_goal_id: None,
            target_kind: EntityKind::Fact,
            target_memory_id: Some(outcome.memory_id.into_inner()),
            target_goal_id: None,
            authorship_kind: EdgeAuthorshipKind::Engine,
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
            source_kind: EntityKind::Fact,
            source_memory_id: Some(outcome.memory_id.into_inner()),
            source_goal_id: None,
            target_kind: EntityKind::Fact,
            target_memory_id: Some(input.triggering_memory_id.into_inner()),
            target_goal_id: None,
            authorship_kind: EdgeAuthorshipKind::Engine,
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

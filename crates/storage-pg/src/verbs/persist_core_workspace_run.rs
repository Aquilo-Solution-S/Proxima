//! Atomic core workspace-run persistence.

use proxima_core::{
    CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION, CoreWorkspaceRunPersistInput,
    CoreWorkspaceRunPersistOutcome, EdgeAuthorshipKind, EntityKind, FlavorRegistryFrozen, MemoryId,
    OwnerPrincipalKind, Principal, StorageError, core_workspace_run_event_draft,
};
use sqlx::PgPool;

use crate::error::map_err;
use crate::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use crate::verbs::event_ingest::ingest_event_in_tx;

pub async fn persist_core_workspace_run_atomic(
    pool: &PgPool,
    registry: &FlavorRegistryFrozen,
    input: &CoreWorkspaceRunPersistInput,
) -> Result<CoreWorkspaceRunPersistOutcome, StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;

    let (owner_kind, owner_principal_id) = match &input.owner.principal {
        Principal::User(u) => (OwnerPrincipalKind::User, u.into_inner()),
        Principal::Group(g) => (OwnerPrincipalKind::Group, g.into_inner()),
    };
    let owner_org_id = input.owner.org_id.into_inner();

    if let Some(memory_id) = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT r.memory_id
           FROM proxima_core.workspace_run_v1 r
           JOIN proxima_core.memories m USING (memory_id)
          WHERE r.wake_invocation_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4",
    )
    .bind(input.run.wake_invocation_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?
    {
        let change_event_seq = sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT seq
               FROM proxima_core.change_event
              WHERE entity_memory_id = $1
              ORDER BY seq ASC
              LIMIT 1",
        )
        .bind(memory_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_err)?;
        tx.commit().await.map_err(map_err)?;
        return Ok(CoreWorkspaceRunPersistOutcome {
            memory_id: MemoryId::new(memory_id),
            change_event_seq,
            idempotent_replay: true,
        });
    }

    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(&input.run, &mut payload_bytes)
        .map_err(|err| StorageError::Internal(format!("serialize core workspace run: {err}")))?;
    let draft = core_workspace_run_event_draft(
        input.owner.clone(),
        &payload_bytes,
        input.source_batch_id,
        input.source_id.clone(),
        input.observed_at,
    );
    let ingest = ingest_event_in_tx(&mut tx, &draft).await?;
    let memory_id = ingest.memory_id.into_inner();

    sqlx::query(
        "INSERT INTO proxima_core.workspace_run_v1
            (memory_id, wake_invocation_id, wake_entry_id, personality_instance_id,
             binding_kind, finalize, repo_path, base_ref, worktree_path, branch_name,
             parent_sha, head_sha, committed, diff_stat_json,
             exit_code, stdout_tail, stderr_tail, duration_ms)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)
         ON CONFLICT (memory_id) DO NOTHING",
    )
    .bind(memory_id)
    .bind(input.run.wake_invocation_id)
    .bind(input.run.wake_entry_id)
    .bind(input.run.personality_instance_id)
    .bind(&input.run.binding_kind)
    .bind(&input.run.finalize)
    .bind(&input.run.repo_path)
    .bind(&input.run.base_ref)
    .bind(&input.run.worktree_path)
    .bind(&input.run.branch_name)
    .bind(&input.run.parent_sha)
    .bind(&input.run.head_sha)
    .bind(input.run.committed)
    .bind(
        serde_json::to_value(&input.run.diff_stat_json).map_err(|err| {
            StorageError::Internal(format!("serialize core workspace diff stat: {err}"))
        })?,
    )
    .bind(input.run.exit_code)
    .bind(input.run.stdout_tail.as_deref())
    .bind(input.run.stderr_tail.as_deref())
    .bind(input.run.duration_ms.and_then(|v| i64::try_from(v).ok()))
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    let authored_relation = registry
        .resolve_relation(CORE_AUTHORED_RELATION)
        .ok_or_else(|| StorageError::Internal("missing core/authored relation".into()))?;
    append_edge_in_tx(
        tx.as_mut(),
        &EdgeDraft {
            edge_id: uuid::Uuid::now_v7(),
            relation: authored_relation,
            source_kind: EntityKind::Perspective,
            source_memory_id: Some(input.root_perspective_memory_id.into_inner()),
            source_goal_id: None,
            target_kind: EntityKind::Fact,
            target_memory_id: Some(memory_id),
            target_goal_id: None,
            authorship_kind: EdgeAuthorshipKind::Engine,
            authorship_owner_memory_id: None,
            owner: &input.owner,
        },
        None,
    )
    .await?;

    let derived_relation = registry
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| StorageError::Internal("missing core/derived-from relation".into()))?;
    append_edge_in_tx(
        tx.as_mut(),
        &EdgeDraft {
            edge_id: uuid::Uuid::now_v7(),
            relation: derived_relation,
            source_kind: EntityKind::Fact,
            source_memory_id: Some(memory_id),
            source_goal_id: None,
            target_kind: EntityKind::Fact,
            target_memory_id: Some(input.triggering_memory_id.into_inner()),
            target_goal_id: None,
            authorship_kind: EdgeAuthorshipKind::Engine,
            authorship_owner_memory_id: None,
            owner: &input.owner,
        },
        None,
    )
    .await?;

    tx.commit().await.map_err(map_err)?;
    Ok(CoreWorkspaceRunPersistOutcome {
        memory_id: ingest.memory_id,
        change_event_seq: ingest.change_event_seq,
        idempotent_replay: ingest.idempotent_replay,
    })
}

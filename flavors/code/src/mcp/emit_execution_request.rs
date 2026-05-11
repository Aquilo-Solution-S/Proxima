use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::relation::{CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION};
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::{
    EdgeId, FactPayload, MemoryId, SchemaId, SchemaVersion, SourceBatchId, SourceId,
};
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::payloads::ExecutionRequestV1;

use super::sql::{map_storage, owner_principal, resolve_repo_identifier};

const EXECUTION_REQUEST_SOURCE_ID: &str = "proxima-code/execution-request";
const EXECUTION_REQUEST_OBJECT_SCHEMA: &str = "proxima-code/execution-request-object-v1";
const EXECUTION_REQUEST_WHOLE_SCHEMA: &str = "proxima-code/execution-request-whole-v1";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeEmitExecutionRequestArgs {
    pub repo_handle: String,
    pub title: String,
    pub instructions: String,
    pub request_key: String,
    pub idempotency_key: String,
    pub goal_activated_memory: String,
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct CodeEmitExecutionRequestOutput {
    pub handle: String,
    pub authored_edge_handle: Option<String>,
    pub derived_edge_handles: Vec<String>,
    pub idempotent_replay: bool,
}

#[derive(Debug)]
pub struct CodeEmitExecutionRequestTool;

impl McpTool for CodeEmitExecutionRequestTool {
    const NAME: &'static str = "proxima-code/code_emit_execution_request";
    const DESCRIPTION: &'static str =
        "Emit a repo-scoped proxima-code/execution-request-v1 Fact for an Active Goal.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[ExecutionRequestV1::SCHEMA_ID];

    type Args = CodeEmitExecutionRequestArgs;
    type Output = CodeEmitExecutionRequestOutput;

    fn call(
        ctx: McpToolCtx,
        args: CodeEmitExecutionRequestArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeEmitExecutionRequestOutput, McpToolError>>
    {
        Box::pin(async move {
            let repo_id = resolve_repo_identifier(&ctx, &args.repo_handle).await?;
            validate_repo(&ctx, repo_id).await?;

            let title = normalize_text("title", &args.title, 1, 240)?;
            let instructions = normalize_text("instructions", &args.instructions, 1, 20_000)?;
            let request_key = normalize_text("request_key", &args.request_key, 1, 240)?;
            let idempotency_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            if request_key != idempotency_key {
                return Err(McpToolError::InvalidInput(
                    "request_key must match idempotency_key".into(),
                ));
            }

            let planner_root = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput(
                    "caller_self_perspective is required to author an execution request".into(),
                )
            })?;
            let goal_activated_memory_id = resolve_memory_id(&ctx, &args.goal_activated_memory)?;
            let evidence = resolve_evidence(&ctx, &args.evidence)?;

            let mut tx = ctx.pool.begin().await.map_err(map_storage)?;
            let goal_id =
                validate_goal_activated_fact(&mut tx, &ctx, goal_activated_memory_id).await?;
            validate_active_goal_context(&mut tx, &ctx, goal_id, planner_root).await?;
            validate_evidence_in_owner(&mut tx, &ctx, &evidence).await?;

            let payload = ExecutionRequestV1 {
                repo_id,
                title,
                instructions,
                request_key,
            };
            let outcome = ingest_execution_request(&mut tx, &ctx, &payload).await?;
            let (authored_edge_id, derived_edge_ids) = if outcome.idempotent_replay {
                (None, Vec::new())
            } else {
                insert_sidecar(&mut tx, outcome.memory_id, &payload).await?;
                let authored_edge_id =
                    append_authored_edge(&mut tx, &ctx, planner_root, outcome.memory_id).await?;
                let mut derived_edge_ids = Vec::with_capacity(1 + evidence.len());
                derived_edge_ids.push(
                    append_derived_edge(&mut tx, &ctx, outcome.memory_id, goal_activated_memory_id)
                        .await?,
                );
                for memory_id in evidence {
                    derived_edge_ids.push(
                        append_derived_edge(&mut tx, &ctx, outcome.memory_id, memory_id).await?,
                    );
                }
                (Some(authored_edge_id), derived_edge_ids)
            };
            tx.commit().await.map_err(map_storage)?;

            Ok(CodeEmitExecutionRequestOutput {
                handle: ctx
                    .handles
                    .assign_memory(outcome.memory_id)
                    .as_str()
                    .to_string(),
                authored_edge_handle: authored_edge_id.map(|edge_id| {
                    ctx.handles
                        .assign_edge(EdgeId::new(edge_id))
                        .as_str()
                        .to_string()
                }),
                derived_edge_handles: derived_edge_ids
                    .into_iter()
                    .map(|edge_id| {
                        ctx.handles
                            .assign_edge(EdgeId::new(edge_id))
                            .as_str()
                            .to_string()
                    })
                    .collect(),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

fn normalize_text(
    field: &str,
    value: &str,
    min: usize,
    max: usize,
) -> Result<String, McpToolError> {
    let trimmed = value.trim();
    let len = trimmed.chars().count();
    if len < min || len > max {
        return Err(McpToolError::InvalidInput(format!(
            "{field} must be {min}..={max} chars"
        )));
    }
    Ok(trimmed.to_string())
}

fn resolve_memory_id(ctx: &McpToolCtx, raw: &str) -> Result<MemoryId, McpToolError> {
    if let Some(memory_id) = ctx.handles.resolve_memory(raw) {
        return Ok(memory_id);
    }
    Uuid::parse_str(raw)
        .map(MemoryId::new)
        .map_err(|_| McpToolError::UnknownHandle(raw.to_string()))
}

fn resolve_evidence(ctx: &McpToolCtx, raw: &[String]) -> Result<Vec<MemoryId>, McpToolError> {
    raw.iter()
        .map(|value| resolve_memory_id(ctx, value))
        .collect()
}

async fn validate_repo(ctx: &McpToolCtx, repo_id: Uuid) -> Result<(), McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1
             FROM proxima_code.repos
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND repo_id = $3
         )",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(repo_id)
    .fetch_one(&ctx.pool)
    .await
    .map_err(map_storage)?;
    if !exists {
        return Err(McpToolError::InvalidInput(format!(
            "repo not found for owner: {repo_id}"
        )));
    }
    Ok(())
}

async fn validate_goal_activated_fact(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<Uuid, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let row: Option<(String, String, Uuid)> = sqlx::query_as(
        "SELECT COALESCE(m.kind, 'Fact') AS kind, m.schema_id, g.goal_id
         FROM proxima_core.memories m
         JOIN proxima_goal.goal_activated_v1 g USING (memory_id)
         WHERE m.memory_id = $1
           AND m.owner_principal_kind = $2
           AND m.owner_principal_id = $3",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_storage)?;
    let Some((kind, schema_id, goal_id)) = row else {
        return Err(McpToolError::InvalidInput(format!(
            "goal_activated_memory is not visible: {}",
            memory_id.into_inner()
        )));
    };
    if kind != "Fact" || schema_id != "proxima-goal/goal-activated-v1" {
        return Err(McpToolError::InvalidInput(
            "goal_activated_memory must be a proxima-goal/goal-activated-v1 Fact".into(),
        ));
    }
    Ok(goal_id)
}

async fn validate_active_goal_context(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    goal_id: Uuid,
    planner_root: MemoryId,
) -> Result<(), McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1
             FROM proxima_core.goals g
             WHERE g.goal_id = $3
               AND g.owner_principal_kind = $1
               AND g.owner_principal_id = $2
               AND g.state = 'Active'
         )",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(goal_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_storage)?;
    if !active {
        return Err(McpToolError::InvalidInput(
            "activated goal is not Active".into(),
        ));
    }

    let assigned: bool = sqlx::query_scalar(
        "WITH RECURSIVE lineage(goal_id) AS (
             SELECT $3::uuid
             UNION
             SELECT g.supersedes
               FROM proxima_core.goals g
               JOIN lineage l ON g.goal_id = l.goal_id
              WHERE g.supersedes IS NOT NULL
                AND g.owner_principal_kind = $1
                AND g.owner_principal_id = $2
         )
         SELECT EXISTS(
             SELECT 1
             FROM proxima_core.edges e
             JOIN lineage l ON l.goal_id = e.source_goal_id
             WHERE e.owner_principal_kind = $1
               AND e.owner_principal_id = $2
               AND e.relation = 'core/inspires'
               AND e.source_kind = 'Goal'
               AND e.target_kind = 'Perspective'
               AND e.target_memory_id = $4
         )",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(goal_id)
    .bind(planner_root.into_inner())
    .fetch_one(&mut **tx)
    .await
    .map_err(map_storage)?;
    if !assigned {
        return Err(McpToolError::InvalidInput(
            "activated goal is Active but not assigned to caller Root Perspective".into(),
        ));
    }
    Ok(())
}

async fn validate_evidence_in_owner(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    evidence: &[MemoryId],
) -> Result<(), McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    for memory_id in evidence {
        let row: Option<String> = sqlx::query_scalar(
            "SELECT COALESCE(kind, 'Fact') AS kind
             FROM proxima_core.memories
             WHERE memory_id = $1
               AND owner_principal_kind = $2
               AND owner_principal_id = $3",
        )
        .bind(memory_id.into_inner())
        .bind(owner_kind)
        .bind(owner_principal_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_storage)?;
        match row.as_deref() {
            Some("Fact" | "Abstraction") => {}
            Some(_) => {
                return Err(McpToolError::LayeringViolation(format!(
                    "evidence {} must be Fact or Abstraction",
                    memory_id.into_inner()
                )));
            }
            None => {
                return Err(McpToolError::InvalidInput(format!(
                    "evidence not visible: {}",
                    memory_id.into_inner()
                )));
            }
        }
    }
    Ok(())
}

async fn ingest_execution_request(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    payload: &ExecutionRequestV1,
) -> Result<proxima_core::verbs::event_ingest::EventIngestOutcome, McpToolError> {
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes)
        .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
    let content_hash = blake3::hash(&payload_bytes);
    let observed_at = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new(EXECUTION_REQUEST_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: ctx.owner.clone(),
        schema_id: SchemaId::new(ExecutionRequestV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(ExecutionRequestV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at,
        occurred_at: observed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(EXECUTION_REQUEST_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(EXECUTION_REQUEST_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    ingest_event_in_tx(tx, &draft)
        .await
        .map_err(McpToolError::Storage)
}

async fn insert_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ExecutionRequestV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_code.execution_request_v1
            (memory_id, repo_id, title, instructions, request_key)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(memory_id.into_inner())
    .bind(payload.repo_id)
    .bind(&payload.title)
    .bind(&payload.instructions)
    .bind(&payload.request_key)
    .execute(&mut **tx)
    .await
    .map_err(map_storage)?;
    Ok(())
}

async fn append_authored_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    planner_root: MemoryId,
    request_memory_id: MemoryId,
) -> Result<Uuid, McpToolError> {
    let relation = ctx
        .registry
        .resolve_relation(CORE_AUTHORED_RELATION)
        .ok_or_else(|| McpToolError::Other("core/authored relation not registered".into()))?;
    let edge_id = Uuid::now_v7();
    append_edge_in_tx(
        tx,
        &EdgeDraft {
            edge_id,
            relation,
            source_kind: "Perspective",
            source_memory_id: Some(planner_root.into_inner()),
            source_goal_id: None,
            target_kind: "Fact",
            target_memory_id: Some(request_memory_id.into_inner()),
            target_goal_id: None,
            authorship_kind: "ExternalAgent",
            authorship_owner_memory_id: Some(planner_root.into_inner()),
            owner: &ctx.owner,
        },
        None,
    )
    .await
    .map_err(McpToolError::Storage)?;
    Ok(edge_id)
}

async fn append_derived_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    request_memory_id: MemoryId,
    evidence_memory_id: MemoryId,
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
            source_memory_id: Some(request_memory_id.into_inner()),
            source_goal_id: None,
            target_kind: "Fact",
            target_memory_id: Some(evidence_memory_id.into_inner()),
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

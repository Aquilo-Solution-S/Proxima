#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value
)]

use proxima_core::mcp::{McpToolCtx, McpToolError};
use proxima_core::relation::{CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION};
use proxima_core::verbs::event_ingest::{
    CitationMappingHint, CitedObjectHint, EventDraft, EventIngestOutcome,
};
use proxima_core::verbs::goal_write::{
    GoalAuthorship, GoalAuthorshipKind, GoalAuthorshipOrigin, GoalDraft, SystemOrigin,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    EdgeAuthorshipKind, EntityKind, FactPayload, GoalId, GoalPayload, MemoryId, Owner,
    OwnerPrincipalKind, PersonalityInstanceId, Principal, SchemaId, SchemaVersion, SourceBatchId,
    SourceId, StorageError,
};
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use sqlx::{Postgres, Transaction};

use crate::payloads::{
    GoalAchievedV1, GoalActivatedV1, GoalProposedV1, SimpleTextGoalV1, TaskGoalV1,
};
use crate::relations::MOTIVATED_BY_RELATION;

const LIFECYCLE_SOURCE_ID: &str = "proxima-goal/lifecycle";
const LIFECYCLE_OBJECT_SCHEMA: &str = "proxima-goal/lifecycle-object-v1";
const LIFECYCLE_CITATION_MAPPING_SCHEMA: &str = "proxima-goal/lifecycle-whole-v1";

#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "schema_id", content = "body")]
pub enum GoalPayloadInput {
    #[serde(rename = "proxima-goal/simple-text-v1")]
    SimpleText(SimpleTextGoalBody),
    #[serde(rename = "proxima-goal/task-v1")]
    Task(TaskGoalBody),
}

#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
pub struct SimpleTextGoalBody {
    #[schemars(description = "Goal title, 1 to 240 chars.")]
    pub title: String,
    #[schemars(description = "Goal text/body, 1 to 20000 chars.")]
    pub text: String,
}

#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
pub struct TaskGoalBody {
    #[schemars(description = "Task Goal title, 1 to 240 chars.")]
    pub title: String,
    #[schemars(description = "Task Goal text/body, 1 to 20000 chars.")]
    pub text: String,
    #[schemars(
        description = "Optional RFC3339 due timestamp. Omit or null when the task has no due date."
    )]
    pub due_at: Option<String>,
    #[schemars(description = "Optional task priority. Omit or null when priority is unspecified.")]
    pub priority: Option<TaskPriorityInput>,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, schemars::JsonSchema)]
pub enum TaskPriorityInput {
    Low,
    Medium,
    High,
}

#[derive(Debug)]
pub struct EncodedGoalPayload {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub title: String,
    pub text: String,
    pub bytes: Vec<u8>,
    pub sidecar: GoalSidecar,
}

#[derive(Debug)]
pub enum GoalSidecar {
    SimpleText,
    Task {
        due_at: Option<time::OffsetDateTime>,
        priority: Option<crate::payloads::TaskPriority>,
    },
}

impl GoalPayloadInput {
    pub fn encode(
        &self,
        registry: &proxima_core::FlavorRegistryFrozen,
    ) -> Result<EncodedGoalPayload, McpToolError> {
        match self {
            Self::SimpleText(body) => {
                let title = body.title.trim();
                if title.is_empty() || title.chars().count() > 240 {
                    return Err(McpToolError::InvalidInput(
                        "simple text goal title must be 1..=240 chars".into(),
                    ));
                }
                let text = body.text.trim();
                if text.is_empty() || text.chars().count() > 20_000 {
                    return Err(McpToolError::InvalidInput(
                        "simple text goal text must be 1..=20000 chars".into(),
                    ));
                }
                let payload = SimpleTextGoalV1 {};
                let value = serde_json::to_value(&payload)
                    .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
                validate_payload(
                    registry,
                    SimpleTextGoalV1::SCHEMA_ID,
                    SimpleTextGoalV1::SCHEMA_VERSION,
                    &value,
                )?;
                let mut bytes = Vec::new();
                ciborium::ser::into_writer(&payload, &mut bytes)
                    .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
                Ok(EncodedGoalPayload {
                    schema_id: SchemaId::new(SimpleTextGoalV1::SCHEMA_ID.into()),
                    schema_version: SchemaVersion::new(SimpleTextGoalV1::SCHEMA_VERSION),
                    title: title.to_string(),
                    text: text.to_string(),
                    bytes,
                    sidecar: GoalSidecar::SimpleText,
                })
            }
            Self::Task(body) => {
                let title = body.title.trim();
                if title.is_empty() || title.chars().count() > 240 {
                    return Err(McpToolError::InvalidInput(
                        "task goal title must be 1..=240 chars".into(),
                    ));
                }
                let text = body.text.trim();
                if text.is_empty() || text.chars().count() > 20_000 {
                    return Err(McpToolError::InvalidInput(
                        "task goal text must be 1..=20000 chars".into(),
                    ));
                }
                let due_at = body
                    .due_at
                    .as_deref()
                    .map(|raw| {
                        time::OffsetDateTime::parse(
                            raw,
                            &time::format_description::well_known::Rfc3339,
                        )
                    })
                    .transpose()
                    .map_err(|err| McpToolError::InvalidInput(format!("invalid due_at: {err}")))?;
                let priority = body.priority.map(Into::into);
                let payload = TaskGoalV1 { due_at, priority };
                let value = serde_json::to_value(&payload)
                    .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
                validate_payload(
                    registry,
                    TaskGoalV1::SCHEMA_ID,
                    TaskGoalV1::SCHEMA_VERSION,
                    &value,
                )?;
                let mut bytes = Vec::new();
                ciborium::ser::into_writer(&payload, &mut bytes)
                    .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
                Ok(EncodedGoalPayload {
                    schema_id: SchemaId::new(TaskGoalV1::SCHEMA_ID.into()),
                    schema_version: SchemaVersion::new(TaskGoalV1::SCHEMA_VERSION),
                    title: title.to_string(),
                    text: text.to_string(),
                    bytes,
                    sidecar: GoalSidecar::Task { due_at, priority },
                })
            }
        }
    }
}

impl From<TaskPriorityInput> for crate::payloads::TaskPriority {
    fn from(value: TaskPriorityInput) -> Self {
        match value {
            TaskPriorityInput::Low => Self::Low,
            TaskPriorityInput::Medium => Self::Medium,
            TaskPriorityInput::High => Self::High,
        }
    }
}

pub fn map_storage(error: sqlx::Error) -> McpToolError {
    McpToolError::Storage(StorageError::Internal(error.to_string()))
}

pub fn owner_columns(owner: &Owner) -> (OwnerPrincipalKind, uuid::Uuid, uuid::Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => (OwnerPrincipalKind::User, u.into_inner()),
        Principal::Group(g) => (OwnerPrincipalKind::Group, g.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

pub async fn target_personality_root(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    handle: &str,
) -> Result<MemoryId, McpToolError> {
    let instance_id = ctx.resolve_personality(handle)?;
    personality_root_in_owner(tx, &ctx.owner, instance_id).await
}

pub async fn personality_root_in_owner(
    tx: &mut sqlx::PgConnection,
    owner: &Owner,
    instance_id: PersonalityInstanceId,
) -> Result<MemoryId, McpToolError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(owner);
    let row = sqlx::query_scalar!(
        r#"SELECT current_root_perspective_memory_id
             FROM proxima_core.personality
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND personality_instance_id = $3
               AND status <> 'tombstoned'::proxima_core.personality_status"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        instance_id.into_inner(),
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_storage)?;
    row.map(MemoryId::new).ok_or_else(|| {
        McpToolError::Other(format!(
            "personality {} has no root perspective",
            instance_id.into_inner()
        ))
    })
}

pub async fn append_inspires_edge(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    goal_id: uuid::Uuid,
    self_memory_id: MemoryId,
    authorship_kind: EdgeAuthorshipKind,
) -> Result<uuid::Uuid, McpToolError> {
    let edge_id = uuid::Uuid::now_v7();
    let relation = ctx
        .registry
        .resolve_relation(proxima_core::relation::CORE_INSPIRES_RELATION)
        .ok_or_else(|| {
            McpToolError::Other(format!(
                "relation {} not registered",
                proxima_core::relation::CORE_INSPIRES_RELATION
            ))
        })?;
    let self_memory_uuid = self_memory_id.into_inner();
    let draft = EdgeDraft {
        edge_id,
        relation,
        source_kind: EntityKind::Goal,
        source_memory_id: None,
        source_goal_id: Some(goal_id),
        target_kind: EntityKind::Perspective,
        target_memory_id: Some(self_memory_uuid),
        target_goal_id: None,
        authorship_kind,
        authorship_owner_memory_id: Some(self_memory_uuid),
        owner: &ctx.owner,
    };
    append_edge_in_tx(tx, &draft, None)
        .await
        .map_err(McpToolError::Storage)?;
    Ok(edge_id)
}

pub async fn validate_evidence_in_owner(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    evidence: &[String],
) -> Result<Vec<EvidenceRef>, McpToolError> {
    let mut out = Vec::with_capacity(evidence.len());
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(&ctx.owner);
    for handle in evidence {
        let memory_id = ctx.resolve_memory(handle)?;
        let row = sqlx::query!(
            r#"SELECT kind AS "kind: EntityKind",
                      owner_principal_kind AS "owner_principal_kind: OwnerPrincipalKind",
                      owner_principal_id
                 FROM proxima_core.memories
                 WHERE memory_id = $1"#,
            memory_id.into_inner(),
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_storage)?;
        let Some(row) = row else {
            return Err(McpToolError::InvalidInput(format!(
                "evidence not found for owner: {handle}"
            )));
        };
        if row.owner_principal_kind != owner_kind || row.owner_principal_id != owner_principal_id {
            return Err(McpToolError::LayeringViolation(format!(
                "evidence {handle} crosses Owner boundary"
            )));
        }
        let target_kind = match row.kind {
            Some(EntityKind::Abstraction) => EntityKind::Abstraction,
            // NULL kind on memories indicates a Fact (memories_variant_chk enforces invariant).
            None => EntityKind::Fact,
            Some(_) => {
                return Err(McpToolError::LayeringViolation(format!(
                    "evidence {handle} must be Fact or Abstraction"
                )));
            }
        };
        out.push(EvidenceRef {
            handle: handle.clone(),
            target_kind,
            target_memory_id: Some(memory_id.into_inner()),
            target_goal_id: None,
        });
    }
    Ok(out)
}

#[derive(Debug)]
pub struct EvidenceRef {
    pub handle: String,
    pub target_kind: EntityKind,
    pub target_memory_id: Option<uuid::Uuid>,
    pub target_goal_id: Option<uuid::Uuid>,
}

pub async fn insert_goal_in_tx(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    draft: &GoalDraft,
    encoded: &EncodedGoalPayload,
) -> Result<uuid::Uuid, McpToolError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&ctx.owner);
    let goal_id = uuid::Uuid::now_v7();
    let (authorship_kind, authorship_origin, authorship_tool_id): (
        GoalAuthorshipKind,
        Option<GoalAuthorshipOrigin>,
        Option<String>,
    ) = match &draft.authorship {
        GoalAuthorship::User => (GoalAuthorshipKind::User, None, None),
        GoalAuthorship::External => (GoalAuthorshipKind::External, None, None),
        GoalAuthorship::System(SystemOrigin::Tool { tool_id }) => (
            GoalAuthorshipKind::System,
            Some(GoalAuthorshipOrigin::Tool),
            Some(tool_id.as_str().to_string()),
        ),
        GoalAuthorship::System(SystemOrigin::Operator { .. }) => {
            return Err(McpToolError::InvalidInput(
                "goal MCP tools do not write System/Operator-authored goals".into(),
            ));
        }
    };

    sqlx::query!(
        r#"INSERT INTO proxima_core.goals
            (goal_id, schema_id, schema_version, owner_principal_kind,
             owner_principal_id, owner_org_id, title, text, payload, state, supersedes,
             authorship_kind, authorship_origin, authorship_tool_id, request_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)"#,
        goal_id,
        draft.schema_id.as_str(),
        draft.schema_version.into_inner().cast_signed(),
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        &draft.title,
        &draft.text,
        &draft.payload,
        draft.state as _,
        draft.supersedes_goal_id.map(GoalId::into_inner),
        authorship_kind as GoalAuthorshipKind,
        authorship_origin as Option<GoalAuthorshipOrigin>,
        authorship_tool_id,
        &draft.request_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(map_storage)?;

    insert_goal_sidecar(tx, goal_id, &encoded.sidecar).await?;
    insert_goal_parents(tx, goal_id, &draft.parent_goal_ids).await?;

    sqlx::query!(
        r#"INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id, owner_org_id,
             kind, entity_kind, entity_goal_id, entity_schema_id,
             entity_schema_version, supersedes_goal_id)
         VALUES ($1, $2, $3, $4, 'EntityAppend', 'Goal', $5, $6, $7, $8)"#,
        uuid::Uuid::now_v7(),
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        goal_id,
        draft.schema_id.as_str(),
        draft.schema_version.into_inner().cast_signed(),
        draft.supersedes_goal_id.map(GoalId::into_inner),
    )
    .execute(&mut *tx)
    .await
    .map_err(map_storage)?;

    Ok(goal_id)
}

async fn insert_goal_parents(
    tx: &mut sqlx::PgConnection,
    goal_id: uuid::Uuid,
    parent_goal_ids: &[GoalId],
) -> Result<(), McpToolError> {
    for parent_id in parent_goal_ids {
        sqlx::query(
            "INSERT INTO proxima_core.goal_parents (goal_id, parent_goal_id)
             VALUES ($1, $2)",
        )
        .bind(goal_id)
        .bind(parent_id.into_inner())
        .execute(&mut *tx)
        .await
        .map_err(map_storage)?;
    }
    Ok(())
}

pub async fn insert_motivated_by_edges(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    goal_id: uuid::Uuid,
    evidence: &[EvidenceRef],
    authorship_kind: EdgeAuthorshipKind,
) -> Result<Vec<uuid::Uuid>, McpToolError> {
    let relation = ctx
        .registry
        .resolve_relation(MOTIVATED_BY_RELATION)
        .ok_or_else(|| {
            McpToolError::Other(format!("relation {MOTIVATED_BY_RELATION} not registered"))
        })?;
    let mut edge_ids = Vec::with_capacity(evidence.len());
    for ev in evidence {
        let edge_id = uuid::Uuid::now_v7();
        let draft = EdgeDraft {
            edge_id,
            relation,
            source_kind: EntityKind::Goal,
            source_memory_id: None,
            source_goal_id: Some(goal_id),
            target_kind: ev.target_kind,
            target_memory_id: ev.target_memory_id,
            target_goal_id: ev.target_goal_id,
            authorship_kind,
            authorship_owner_memory_id: None,
            owner: &ctx.owner,
        };
        append_edge_in_tx(tx, &draft, None)
            .await
            .map_err(McpToolError::Storage)?;
        edge_ids.push(edge_id);
    }
    Ok(edge_ids)
}

pub async fn emit_goal_proposed_fact(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    goal_id: uuid::Uuid,
    encoded: &EncodedGoalPayload,
) -> Result<MemoryId, McpToolError> {
    let payload = GoalProposedV1 {
        goal_id,
        schema_id: encoded.schema_id.as_str().to_string(),
        title: encoded.title.clone(),
    };
    let outcome = ingest_lifecycle_fact(tx, ctx, &payload).await?;
    if !outcome.idempotent_replay {
        insert_goal_proposed_sidecar(tx, outcome.memory_id, &payload).await?;
    }
    let memory_id = outcome.memory_id;
    if let Some(self_id) = ctx.caller_self_perspective {
        insert_lifecycle_authored_edge(tx, ctx, self_id, memory_id).await?;
    }
    Ok(memory_id)
}

pub async fn emit_goal_activated_fact(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    goal_id: uuid::Uuid,
    encoded: &EncodedGoalPayload,
    accepted_at: time::OffsetDateTime,
    evidence_count: usize,
) -> Result<MemoryId, McpToolError> {
    let payload = GoalActivatedV1 {
        goal_id,
        schema_id: encoded.schema_id.as_str().to_string(),
        title: encoded.title.clone(),
        accepted_at,
        evidence_count: u32::try_from(evidence_count).unwrap_or(u32::MAX),
    };
    let outcome = ingest_lifecycle_fact(tx, ctx, &payload).await?;
    if !outcome.idempotent_replay {
        insert_goal_activated_sidecar(tx, outcome.memory_id, &payload).await?;
    }
    let memory_id = outcome.memory_id;
    if let Some(self_id) = ctx.caller_self_perspective {
        insert_lifecycle_authored_edge(tx, ctx, self_id, memory_id).await?;
    }
    Ok(memory_id)
}

pub async fn emit_goal_achieved_fact(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    goal_id: uuid::Uuid,
    encoded: &EncodedGoalPayload,
    achieved_at: time::OffsetDateTime,
    evidence_count: usize,
) -> Result<MemoryId, McpToolError> {
    let payload = GoalAchievedV1 {
        goal_id,
        schema_id: encoded.schema_id.as_str().to_string(),
        title: encoded.title.clone(),
        achieved_at,
        evidence_count: u32::try_from(evidence_count).unwrap_or(u32::MAX),
    };
    let outcome = ingest_lifecycle_fact(tx, ctx, &payload).await?;
    if !outcome.idempotent_replay {
        insert_goal_achieved_sidecar(tx, outcome.memory_id, &payload).await?;
    }
    let memory_id = outcome.memory_id;
    if let Some(self_id) = ctx.caller_self_perspective {
        insert_lifecycle_authored_edge(tx, ctx, self_id, memory_id).await?;
    }
    Ok(memory_id)
}

pub async fn append_lifecycle_derived_from_edges(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    source_memory_id: MemoryId,
    evidence: &[EvidenceRef],
) -> Result<Vec<uuid::Uuid>, McpToolError> {
    let relation = ctx
        .registry
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| {
            McpToolError::Other(format!(
                "relation {CORE_DERIVED_FROM_RELATION} not registered"
            ))
        })?;
    let mut edge_ids = Vec::with_capacity(evidence.len());
    for ev in evidence {
        if ev.target_kind != EntityKind::Fact {
            continue;
        }
        let edge_id = uuid::Uuid::now_v7();
        let draft = EdgeDraft {
            edge_id,
            relation,
            source_kind: EntityKind::Fact,
            source_memory_id: Some(source_memory_id.into_inner()),
            source_goal_id: None,
            target_kind: ev.target_kind,
            target_memory_id: ev.target_memory_id,
            target_goal_id: ev.target_goal_id,
            authorship_kind: EdgeAuthorshipKind::Engine,
            authorship_owner_memory_id: None,
            owner: &ctx.owner,
        };
        append_edge_in_tx(tx, &draft, None)
            .await
            .map_err(McpToolError::Storage)?;
        edge_ids.push(edge_id);
    }
    Ok(edge_ids)
}

pub async fn outgoing_motivated_by_evidence(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    goal_id: GoalId,
) -> Result<Vec<EvidenceRef>, McpToolError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(&ctx.owner);
    let rows = sqlx::query!(
        r#"SELECT edge_id,
                  target_kind AS "target_kind: EntityKind",
                  target_memory_id,
                  target_goal_id
             FROM proxima_core.edges
             WHERE relation = $1
               AND source_goal_id = $2
               AND owner_principal_kind = $3
               AND owner_principal_id = $4
             ORDER BY created_at ASC"#,
        MOTIVATED_BY_RELATION,
        goal_id.into_inner(),
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(map_storage)?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let handle = ctx.format_edge(proxima_core::EdgeId::new(row.edge_id));
        let target_kind = match row.target_kind {
            EntityKind::Fact => EntityKind::Fact,
            EntityKind::Abstraction => EntityKind::Abstraction,
            other => {
                return Err(McpToolError::LayeringViolation(format!(
                    "stored MotivatedBy target must be Fact or Abstraction, got {other:?}"
                )));
            }
        };
        out.push(EvidenceRef {
            handle,
            target_kind,
            target_memory_id: row.target_memory_id,
            target_goal_id: row.target_goal_id,
        });
    }
    Ok(out)
}

pub async fn load_goal_payload(
    tx: &mut sqlx::PgConnection,
    goal_id: GoalId,
) -> Result<GoalPayloadInput, McpToolError> {
    let row = sqlx::query!(
        "SELECT schema_id, title, text, payload FROM proxima_core.goals WHERE goal_id = $1",
        goal_id.into_inner(),
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(map_storage)?;
    match row.schema_id.as_str() {
        SimpleTextGoalV1::SCHEMA_ID => {
            let _: SimpleTextGoalV1 = ciborium::de::from_reader(&row.payload[..])
                .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
            Ok(GoalPayloadInput::SimpleText(SimpleTextGoalBody {
                title: row.title,
                text: row.text,
            }))
        }
        TaskGoalV1::SCHEMA_ID => {
            let payload: TaskGoalV1 = ciborium::de::from_reader(&row.payload[..])
                .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
            Ok(GoalPayloadInput::Task(TaskGoalBody {
                title: row.title,
                text: row.text,
                due_at: payload.due_at.map(|dt| {
                    dt.format(&time::format_description::well_known::Rfc3339)
                        .expect("Rfc3339 formatting succeeds for OffsetDateTime")
                }),
                priority: payload.priority.map(|priority| match priority {
                    crate::payloads::TaskPriority::Low => TaskPriorityInput::Low,
                    crate::payloads::TaskPriority::Medium => TaskPriorityInput::Medium,
                    crate::payloads::TaskPriority::High => TaskPriorityInput::High,
                }),
            }))
        }
        other => Err(McpToolError::InvalidInput(format!(
            "unsupported goal payload schema {other}"
        ))),
    }
}

pub fn request_id(prefix: &str, idempotency_key: Option<String>) -> String {
    idempotency_key.unwrap_or_else(|| format!("{prefix}:{}", uuid::Uuid::now_v7()))
}

fn validate_payload(
    registry: &proxima_core::FlavorRegistryFrozen,
    schema: &str,
    version: u32,
    value: &serde_json::Value,
) -> Result<(), McpToolError> {
    let schema_id = SchemaId::new(schema.to_string());
    let schema_version = SchemaVersion::new(version);
    match registry.lookup(&schema_id, schema_version) {
        Some(info) if info.kind == PayloadKind::Goal => {}
        _ => {
            return Err(McpToolError::InvalidInput(format!(
                "unregistered GoalPayload schema {schema} v{version}"
            )));
        }
    }
    registry
        .validate_payload(&schema_id, schema_version, PayloadKind::Goal, value)
        .map_err(McpToolError::InvalidInput)
}

async fn ingest_lifecycle_fact<T>(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    payload: &T,
) -> Result<EventIngestOutcome, McpToolError>
where
    T: FactPayload,
{
    let value =
        serde_json::to_value(payload).map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
    validate_fact_payload(ctx, T::SCHEMA_ID, T::SCHEMA_VERSION, &value)?;
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes)
        .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
    let content_hash = blake3::hash(&payload_bytes);
    let observed_at = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new(LIFECYCLE_SOURCE_ID),
        source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
        owner: ctx.owner.clone(),
        schema_id: SchemaId::new(T::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(T::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at,
        occurred_at: observed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(LIFECYCLE_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(LIFECYCLE_CITATION_MAPPING_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    };

    ingest_event_in_tx(tx, &draft)
        .await
        .map_err(McpToolError::Storage)
}

fn validate_fact_payload(
    ctx: &McpToolCtx,
    schema: &str,
    version: u32,
    value: &serde_json::Value,
) -> Result<(), McpToolError> {
    let schema_id = SchemaId::new(schema.to_string());
    let schema_version = SchemaVersion::new(version);
    match ctx.registry.lookup(&schema_id, schema_version) {
        Some(info) if info.kind == PayloadKind::Fact => {}
        _ => {
            return Err(McpToolError::InvalidInput(format!(
                "unregistered FactPayload schema {schema} v{version}"
            )));
        }
    }
    ctx.registry
        .validate_payload(&schema_id, schema_version, PayloadKind::Fact, value)
        .map_err(McpToolError::InvalidInput)
}

async fn insert_goal_proposed_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &GoalProposedV1,
) -> Result<(), McpToolError> {
    sqlx::query!(
        "INSERT INTO proxima_goal.goal_proposed_v1
            (memory_id, goal_id, schema_id, title)
         VALUES ($1, $2, $3, $4)",
        memory_id.into_inner(),
        payload.goal_id,
        &payload.schema_id,
        &payload.title,
    )
    .execute(&mut **tx)
    .await
    .map_err(map_storage)?;
    Ok(())
}

async fn insert_goal_activated_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &GoalActivatedV1,
) -> Result<(), McpToolError> {
    sqlx::query!(
        "INSERT INTO proxima_goal.goal_activated_v1
            (memory_id, goal_id, schema_id, title, accepted_at, evidence_count)
         VALUES ($1, $2, $3, $4, $5, $6)",
        memory_id.into_inner(),
        payload.goal_id,
        &payload.schema_id,
        &payload.title,
        payload.accepted_at,
        i32::try_from(payload.evidence_count).unwrap_or(i32::MAX),
    )
    .execute(&mut **tx)
    .await
    .map_err(map_storage)?;
    Ok(())
}

async fn insert_goal_achieved_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &GoalAchievedV1,
) -> Result<(), McpToolError> {
    sqlx::query!(
        "INSERT INTO proxima_goal.goal_achieved_v1
            (memory_id, goal_id, schema_id, title, achieved_at, evidence_count)
         VALUES ($1, $2, $3, $4, $5, $6)",
        memory_id.into_inner(),
        payload.goal_id,
        &payload.schema_id,
        &payload.title,
        payload.achieved_at,
        i32::try_from(payload.evidence_count).unwrap_or(i32::MAX),
    )
    .execute(&mut **tx)
    .await
    .map_err(map_storage)?;
    Ok(())
}

async fn insert_lifecycle_authored_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    self_id: MemoryId,
    fact_id: MemoryId,
) -> Result<(), McpToolError> {
    let relation = ctx
        .registry
        .resolve_relation(CORE_AUTHORED_RELATION)
        .ok_or_else(|| {
            McpToolError::Other(format!("relation {CORE_AUTHORED_RELATION} not registered"))
        })?;
    let draft = EdgeDraft {
        edge_id: uuid::Uuid::now_v7(),
        relation,
        source_kind: EntityKind::Perspective,
        source_memory_id: Some(self_id.into_inner()),
        source_goal_id: None,
        target_kind: EntityKind::Fact,
        target_memory_id: Some(fact_id.into_inner()),
        target_goal_id: None,
        authorship_kind: EdgeAuthorshipKind::Engine,
        authorship_owner_memory_id: None,
        owner: &ctx.owner,
    };
    append_edge_in_tx(tx, &draft, None)
        .await
        .map_err(McpToolError::Storage)
}

async fn insert_goal_sidecar(
    tx: &mut sqlx::PgConnection,
    goal_id: uuid::Uuid,
    sidecar: &GoalSidecar,
) -> Result<(), McpToolError> {
    match sidecar {
        GoalSidecar::SimpleText => {
            sqlx::query!(
                "INSERT INTO proxima_goal.simple_text_goal_v1 (goal_id)
                 VALUES ($1)",
                goal_id,
            )
            .execute(&mut *tx)
            .await
            .map_err(map_storage)?;
        }
        GoalSidecar::Task { due_at, priority } => {
            sqlx::query!(
                r#"INSERT INTO proxima_goal.task_goal_v1 (goal_id, due_at, priority)
                 VALUES ($1, $2, $3)"#,
                goal_id,
                due_at.as_ref(),
                priority.as_ref() as Option<&crate::payloads::TaskPriority>,
            )
            .execute(&mut *tx)
            .await
            .map_err(map_storage)?;
        }
    }
    Ok(())
}

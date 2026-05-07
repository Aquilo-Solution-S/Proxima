#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value
)]

use proxima_core::mcp::{EntityRef, McpToolCtx, McpToolError};
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, GoalState};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{GoalId, GoalPayload, Owner, Principal, SchemaId, SchemaVersion, StorageError};
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};

use crate::payloads::{SimpleTextGoalV1, TaskGoalV1};
use crate::relations::MOTIVATED_BY_RELATION;

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
    pub title: String,
    pub text: String,
}

#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
pub struct TaskGoalBody {
    pub title: String,
    pub text: String,
    pub due_at: Option<String>,
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
        priority: Option<&'static str>,
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
                    sidecar: GoalSidecar::Task {
                        due_at,
                        priority: priority.map(task_priority_str),
                    },
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

pub fn owner_columns(owner: &Owner) -> (&'static str, uuid::Uuid, uuid::Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

pub async fn validate_evidence_in_owner(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    evidence: &[String],
) -> Result<Vec<EvidenceRef>, McpToolError> {
    let mut out = Vec::with_capacity(evidence.len());
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(&ctx.owner);
    for handle in evidence {
        let entity = ctx
            .handles
            .resolve(handle)
            .ok_or_else(|| McpToolError::UnknownHandle(handle.clone()))?;
        match entity {
            EntityRef::Memory(memory_id) => {
                let row: Option<(String, String, uuid::Uuid)> = sqlx::query_as(
                    "SELECT kind, owner_principal_kind, owner_principal_id
                     FROM proxima_core.memories
                     WHERE memory_id = $1",
                )
                .bind(memory_id.into_inner())
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_storage)?;
                let Some((kind, row_owner_kind, row_owner_principal_id)) = row else {
                    return Err(McpToolError::UnknownHandle(handle.clone()));
                };
                if row_owner_kind != owner_kind || row_owner_principal_id != owner_principal_id {
                    return Err(McpToolError::LayeringViolation(format!(
                        "evidence {handle} crosses Owner boundary"
                    )));
                }
                let target_kind = match kind.as_str() {
                    "Fact" => "Fact",
                    "Abstraction" => "Abstraction",
                    _ => {
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
            EntityRef::Goal(_) | EntityRef::Edge(_) => {
                return Err(McpToolError::LayeringViolation(format!(
                    "evidence {handle} must resolve to Fact or Abstraction"
                )));
            }
        }
    }
    Ok(out)
}

#[derive(Debug)]
pub struct EvidenceRef {
    pub handle: String,
    pub target_kind: &'static str,
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
    let state = goal_state_str(draft.state);
    let (authorship_kind, authorship_origin, authorship_tool_id): (
        &'static str,
        Option<&'static str>,
        Option<&'static str>,
    ) = match &draft.authorship {
        GoalAuthorship::User => ("User", None, None),
        GoalAuthorship::External => ("External", None, None),
        GoalAuthorship::System(_) => {
            return Err(McpToolError::InvalidInput(
                "goal MCP tools do not write System-authored goals".into(),
            ));
        }
    };

    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, schema_id, schema_version, owner_principal_kind,
             owner_principal_id, owner_org_id, title, text, payload, state, supersedes,
             authorship_kind, authorship_origin, authorship_tool_id, request_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
    )
    .bind(goal_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&draft.title)
    .bind(&draft.text)
    .bind(&draft.payload)
    .bind(state)
    .bind(draft.supersedes_goal_id.map(GoalId::into_inner))
    .bind(authorship_kind)
    .bind(authorship_origin)
    .bind(authorship_tool_id)
    .bind(&draft.request_id)
    .execute(&mut *tx)
    .await
    .map_err(map_storage)?;

    insert_goal_sidecar(tx, goal_id, &encoded.sidecar).await?;

    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id, owner_org_id,
             kind, entity_kind, entity_goal_id, entity_schema_id,
             entity_schema_version, supersedes_goal_id)
         VALUES ($1, $2, $3, $4, 'EntityAppend', 'Goal', $5, $6, $7, $8)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(goal_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .bind(draft.supersedes_goal_id.map(GoalId::into_inner))
    .execute(&mut *tx)
    .await
    .map_err(map_storage)?;

    Ok(goal_id)
}

pub async fn insert_motivated_by_edges(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    goal_id: uuid::Uuid,
    evidence: &[EvidenceRef],
    authorship_kind: &'static str,
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
            source_kind: "Goal",
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

pub async fn outgoing_motivated_by_evidence(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    goal_id: GoalId,
) -> Result<Vec<EvidenceRef>, McpToolError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(&ctx.owner);
    let rows: Vec<(uuid::Uuid, String, Option<uuid::Uuid>, Option<uuid::Uuid>)> = sqlx::query_as(
        "SELECT edge_id, target_kind, target_memory_id, target_goal_id
         FROM proxima_core.edges
         WHERE relation = $1
           AND source_goal_id = $2
           AND owner_principal_kind = $3
           AND owner_principal_id = $4
         ORDER BY created_at ASC",
    )
    .bind(MOTIVATED_BY_RELATION)
    .bind(goal_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_storage)?;

    let mut out = Vec::with_capacity(rows.len());
    for (edge_id, target_kind, target_memory_id, target_goal_id) in rows {
        let handle = ctx
            .handles
            .assign_edge(proxima_core::EdgeId::new(edge_id))
            .as_str()
            .to_string();
        let target_kind = match target_kind.as_str() {
            "Fact" => "Fact",
            "Abstraction" => "Abstraction",
            _ => {
                return Err(McpToolError::LayeringViolation(format!(
                    "stored MotivatedBy target must be Fact or Abstraction, got {target_kind}"
                )));
            }
        };
        out.push(EvidenceRef {
            handle,
            target_kind,
            target_memory_id,
            target_goal_id,
        });
    }
    Ok(out)
}

pub async fn load_goal_payload(
    tx: &mut sqlx::PgConnection,
    goal_id: GoalId,
) -> Result<GoalPayloadInput, McpToolError> {
    let row: (String, String, String, Vec<u8>) = sqlx::query_as(
        "SELECT schema_id, title, text, payload FROM proxima_core.goals WHERE goal_id = $1",
    )
    .bind(goal_id.into_inner())
    .fetch_one(tx)
    .await
    .map_err(map_storage)?;
    match row.0.as_str() {
        SimpleTextGoalV1::SCHEMA_ID => {
            let _: SimpleTextGoalV1 = ciborium::de::from_reader(&row.3[..])
                .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
            Ok(GoalPayloadInput::SimpleText(SimpleTextGoalBody {
                title: row.1,
                text: row.2,
            }))
        }
        TaskGoalV1::SCHEMA_ID => {
            let payload: TaskGoalV1 = ciborium::de::from_reader(&row.3[..])
                .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
            Ok(GoalPayloadInput::Task(TaskGoalBody {
                title: row.1,
                text: row.2,
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

fn goal_state_str(state: GoalState) -> &'static str {
    match state {
        GoalState::Proposed => "Proposed",
        GoalState::Active => "Active",
        GoalState::Paused => "Paused",
        GoalState::Achieved => "Achieved",
        GoalState::Abandoned => "Abandoned",
        GoalState::Rejected => "Rejected",
    }
}

async fn insert_goal_sidecar(
    tx: &mut sqlx::PgConnection,
    goal_id: uuid::Uuid,
    sidecar: &GoalSidecar,
) -> Result<(), McpToolError> {
    match sidecar {
        GoalSidecar::SimpleText => {
            sqlx::query(
                "INSERT INTO proxima_goal.simple_text_goal_v1 (goal_id)
                 VALUES ($1)",
            )
            .bind(goal_id)
            .execute(tx)
            .await
            .map_err(map_storage)?;
        }
        GoalSidecar::Task { due_at, priority } => {
            sqlx::query(
                "INSERT INTO proxima_goal.task_goal_v1 (goal_id, due_at, priority)
                 VALUES ($1, $2, $3)",
            )
            .bind(goal_id)
            .bind(due_at)
            .bind(priority)
            .execute(tx)
            .await
            .map_err(map_storage)?;
        }
    }
    Ok(())
}

fn task_priority_str(priority: crate::payloads::TaskPriority) -> &'static str {
    match priority {
        crate::payloads::TaskPriority::Low => "Low",
        crate::payloads::TaskPriority::Medium => "Medium",
        crate::payloads::TaskPriority::High => "High",
    }
}

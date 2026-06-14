use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::{
    AbstractionPayload, CORE_DERIVED_FROM_RELATION, EdgeAuthorshipKind, EdgeId, MemoryId, SchemaId,
    SchemaVersion,
};
use proxima_storage_pg::verbs::derive_append::{DerivedDraft, append_derived_in_tx};
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::AgentDerivationV1;

use super::util::{
    map_storage, memory_kind_for_edge, normalize_tags, owner_columns, owner_principal,
};

const DERIVED_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x9d, 0xc1, 0x37, 0x10, 0x4f, 0xa6, 0x4c, 0x4e, 0x95, 0x73, 0xc8, 0x18, 0x9d, 0xfb, 0xa7, 0x40,
]);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeriveArgs {
    #[schemars(description = "Derived memory kind to author: Abstraction or Perspective.")]
    pub kind: DerivedKind,
    #[schemars(description = "Short title for the derived memory, 1 to 160 chars.")]
    pub title: String,
    #[schemars(description = "Body text for the derived memory, 1 to 20000 chars.")]
    pub body: String,
    #[serde(default)]
    #[schemars(
        description = "Optional normalized tags for later search. Use `[]` when no tags are needed."
    )]
    pub tags: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional source memory handles (`F...`, `A...`, or `P...`) this derivation is based on. Use `[]` only when there is no concrete memory provenance."
    )]
    pub source_handles: Vec<String>,
    #[schemars(
        description = "Model identifier or external agent label used as operator provenance, 1 to 120 chars."
    )]
    pub model_id: String,
    #[schemars(
        description = "Optional stable idempotency key. Omit or null to derive one from model_id and body."
    )]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
pub enum DerivedKind {
    Abstraction,
    Perspective,
}

impl DerivedKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Abstraction => "Abstraction",
            Self::Perspective => "Perspective",
        }
    }

    fn to_entity_kind(self) -> proxima_core::EntityKind {
        match self {
            Self::Abstraction => proxima_core::EntityKind::Abstraction,
            Self::Perspective => proxima_core::EntityKind::Perspective,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DeriveOutput {
    pub handle: String,
    pub idempotent_replay: bool,
    pub provenance_edge_handles: Vec<String>,
}

#[derive(Debug)]
pub struct DeriveTool;

impl McpTool for DeriveTool {
    const NAME: &'static str = "proxima-agent-memory/proxima_derive";
    const DESCRIPTION: &'static str =
        "Author an Abstraction or Perspective derived from existing memory handles.";
    type Args = DeriveArgs;
    type Output = DeriveOutput;

    #[allow(clippy::too_many_lines)]
    fn call(
        ctx: McpToolCtx,
        args: DeriveArgs,
    ) -> futures::future::BoxFuture<'static, Result<DeriveOutput, McpToolError>> {
        Box::pin(async move {
            let title = args.title.trim();
            let body = args.body.trim();
            if title.is_empty() || title.chars().count() > 160 {
                return Err(McpToolError::InvalidInput(
                    "title must be 1..=160 chars".into(),
                ));
            }
            if body.is_empty() || body.chars().count() > 20_000 {
                return Err(McpToolError::InvalidInput(
                    "body must be 1..=20000 chars".into(),
                ));
            }
            if args.model_id.trim().is_empty() || args.model_id.chars().count() > 120 {
                return Err(McpToolError::InvalidInput(
                    "model_id required, 1..=120 chars".into(),
                ));
            }
            let tags = normalize_tags(args.tags)?;

            let mut source_uuids = Vec::with_capacity(args.source_handles.len());
            for handle in &args.source_handles {
                let memory_id = ctx.resolve_memory(handle)?;
                source_uuids.push(memory_id.into_inner());
            }

            let source_kinds = load_source_kinds(&ctx.pool, &ctx.owner, &source_uuids).await?;

            // Pre-validate provenance edge shapes against the relation's kind
            // masks so layering failures surface as LayeringViolation instead
            // of a storage constraint error from the edge append.
            let relation = if source_uuids.is_empty() {
                None
            } else {
                let relation = ctx
                    .registry
                    .resolve_relation(CORE_DERIVED_FROM_RELATION)
                    .ok_or_else(|| {
                        McpToolError::Other(format!(
                            "relation {CORE_DERIVED_FROM_RELATION} not registered"
                        ))
                    })?;
                for source_kind in &source_kinds {
                    relation
                        .descriptor
                        .validate_edge_shape(
                            args.kind.to_entity_kind().as_str(),
                            memory_kind_for_edge(*source_kind).as_str(),
                            EdgeAuthorshipKind::ExternalAgent.as_str(),
                        )
                        .map_err(McpToolError::LayeringViolation)?;
                }
                Some(relation)
            };

            let key = args.idempotency_key.clone().unwrap_or_else(|| {
                format!(
                    "{}:{}",
                    args.model_id,
                    blake3::hash(body.as_bytes()).to_hex()
                )
            });
            let memory_id = derived_memory_id(&ctx.owner, args.kind.as_str(), &key);
            let sidecar = serde_json::to_value(AgentDerivationV1 {
                title: title.to_string(),
                body: body.to_string(),
                tags: tags.clone(),
                idempotency_key: args.idempotency_key.clone(),
                source_memory_ids: source_uuids.clone(),
                model_id: args.model_id.clone(),
                client_name: ctx.author.client_name.clone(),
                client_version: ctx.author.client_version.clone(),
            })
            .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;

            let draft = DerivedDraft {
                memory_id,
                owner: ctx.owner.clone(),
                kind: args.kind.to_entity_kind(),
                author_personality_instance_id: ctx.author.personality_instance_id,
                schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
                schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
                text: body.to_string(),
                operator_kind: proxima_core::MemoryOperatorKind::ExternalAgent,
                model_id: &args.model_id,
                prompt_version: "mcp-agent-v1",
                sidecar_table: Some("proxima_agent_memory.agent_derivation_v1"),
                sidecar_payload: Some(sidecar),
            };

            let mut tx = ctx.pool.begin().await.map_err(map_storage)?;
            let outcome = append_derived_in_tx(&mut tx, &draft)
                .await
                .map_err(McpToolError::Storage)?;

            let mut provenance_edge_handles = Vec::new();
            if !outcome.idempotent_replay
                && let Some(relation) = relation
            {
                for (source_id, source_kind) in source_uuids.iter().zip(source_kinds) {
                    let edge_id = provenance_edge_id(memory_id, *source_id);
                    let edge_draft = EdgeDraft {
                        edge_id,
                        relation,
                        source_kind: args.kind.to_entity_kind(),
                        source_memory_id: Some(memory_id),
                        source_goal_id: None,
                        target_kind: memory_kind_for_edge(source_kind),
                        target_memory_id: Some(*source_id),
                        target_goal_id: None,
                        authorship_kind: EdgeAuthorshipKind::ExternalAgent,
                        authorship_owner_memory_id: ctx
                            .caller_self_perspective
                            .map(MemoryId::into_inner),
                        owner: &ctx.owner,
                    };
                    append_edge_in_tx(&mut tx, &edge_draft, None)
                        .await
                        .map_err(McpToolError::Storage)?;
                    provenance_edge_handles.push(ctx.format_edge(EdgeId::new(edge_id)));
                }
            }
            tx.commit().await.map_err(map_storage)?;

            Ok(DeriveOutput {
                handle: match args.kind {
                    DerivedKind::Abstraction => {
                        ctx.format_abstraction_memory(MemoryId::new(memory_id))
                    }
                    DerivedKind::Perspective => {
                        ctx.format_perspective_memory(MemoryId::new(memory_id))
                    }
                },
                idempotent_replay: outcome.idempotent_replay,
                provenance_edge_handles,
            })
        })
    }
}

async fn load_source_kinds(
    pool: &sqlx::PgPool,
    owner: &proxima_core::Owner,
    memory_ids: &[uuid::Uuid],
) -> Result<Vec<Option<proxima_core::EntityKind>>, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(owner);
    let mut out = Vec::with_capacity(memory_ids.len());
    for memory_id in memory_ids {
        let kind: Option<Option<proxima_core::EntityKind>> = sqlx::query_scalar(
            "SELECT kind
             FROM proxima_core.memories
             WHERE memory_id = $1
               AND owner_principal_kind = $2
               AND owner_principal_id = $3",
        )
        .bind(memory_id)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .fetch_optional(pool)
        .await
        .map_err(map_storage)?;
        out.push(kind.ok_or_else(|| {
            McpToolError::InvalidInput(format!("source memory {memory_id} not found for owner"))
        })?);
    }
    Ok(out)
}

fn derived_memory_id(owner: &proxima_core::Owner, kind: &str, key: &str) -> uuid::Uuid {
    let (principal_kind, principal_id, org_id) = owner_columns(owner);
    let mut buf = Vec::with_capacity(96 + key.len());
    buf.extend_from_slice(principal_kind.as_str().as_bytes());
    buf.push(0);
    buf.extend_from_slice(principal_id.as_bytes());
    buf.push(0);
    buf.extend_from_slice(org_id.as_bytes());
    buf.push(0);
    buf.extend_from_slice(kind.as_bytes());
    buf.push(0);
    buf.extend_from_slice(key.as_bytes());
    uuid::Uuid::new_v5(&DERIVED_NAMESPACE, &buf)
}

fn provenance_edge_id(source: uuid::Uuid, target: uuid::Uuid) -> uuid::Uuid {
    let mut key = Vec::with_capacity(64);
    key.extend_from_slice(CORE_DERIVED_FROM_RELATION.as_bytes());
    key.push(0);
    key.extend_from_slice(source.as_bytes());
    key.push(0);
    key.extend_from_slice(target.as_bytes());
    uuid::Uuid::new_v5(&DERIVED_NAMESPACE, &key)
}

use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::{
    AbstractionPayload, AuthorDerivedEdgeInput, AuthorDerivedRequestInput,
    CORE_DERIVED_FROM_RELATION, EdgeAuthorshipKind, EndpointBinding, MemoryId, SchemaId,
    SchemaVersion, SidecarPayload,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::AgentDerivationV1;

use super::util::{memory_kind_for_edge, normalize_tags};

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

    fn to_entity_kind(self) -> crate::EntityKind {
        match self {
            Self::Abstraction => crate::EntityKind::Abstraction,
            Self::Perspective => crate::EntityKind::Perspective,
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
    const NAME: &'static str = "core/derive";
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

            let source_kinds = load_source_kinds(&ctx, &source_uuids).await?;

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
                            EndpointBinding::Pin,
                            memory_kind_for_edge(*source_kind).as_str(),
                            EndpointBinding::Pin,
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
            let sidecar = AgentDerivationV1 {
                title: title.to_string(),
                body: body.to_string(),
                tags: tags.clone(),
                idempotency_key: args.idempotency_key.clone(),
                source_memory_ids: source_uuids.clone(),
                model_id: args.model_id.clone(),
                client_name: ctx.author.client_name.clone(),
                client_version: ctx.author.client_version.clone(),
            };

            let memory_id = MemoryId::new(memory_id);
            let edges: Vec<_> = relation.map_or_else(Vec::new, |relation| {
                source_uuids
                    .iter()
                    .zip(source_kinds.iter().copied())
                    .map(|(source_id, source_kind)| AuthorDerivedEdgeInput {
                        relation,
                        source_kind: args.kind.to_entity_kind(),
                        source_memory_id: memory_id,
                        target_kind: memory_kind_for_edge(source_kind),
                        target_memory_id: MemoryId::new(*source_id),
                        authorship_kind: EdgeAuthorshipKind::ExternalAgent,
                        authorship_owner_memory_id: ctx.caller_self_perspective,
                    })
                    .collect()
            });
            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::InvalidInput("engine required".into()))?;
            let outcome = engine
                .author_derived(AuthorDerivedRequestInput {
                    memory_id,
                    owner: ctx.owner.clone(),
                    kind: args.kind.to_entity_kind(),
                    text: body.to_string(),
                    schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
                    schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
                    operator_kind: crate::MemoryOperatorKind::ExternalAgent,
                    model_id: &args.model_id,
                    prompt_version: "mcp-agent-v1",
                    author_personality_instance_id: ctx.author.personality_instance_id,
                    sidecar_payload: match args.kind {
                        DerivedKind::Abstraction => SidecarPayload::abstraction(sidecar),
                        DerivedKind::Perspective => SidecarPayload::perspective(sidecar),
                    },
                    supersedes: None,
                    edges: &edges,
                })
                .await?;

            let provenance_edge_handles = if outcome.idempotent_replay || edges.is_empty() {
                Vec::new()
            } else {
                load_provenance_edge_handles(&ctx, memory_id, &source_uuids).await?
            };

            Ok(DeriveOutput {
                handle: match args.kind {
                    DerivedKind::Abstraction => ctx.format_abstraction_memory(memory_id),
                    DerivedKind::Perspective => ctx.format_perspective_memory(memory_id),
                },
                idempotent_replay: outcome.idempotent_replay,
                provenance_edge_handles,
            })
        })
    }
}

async fn load_source_kinds(
    ctx: &McpToolCtx,
    memory_ids: &[uuid::Uuid],
) -> Result<Vec<Option<crate::EntityKind>>, McpToolError> {
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    let ids = memory_ids
        .iter()
        .copied()
        .map(MemoryId::new)
        .collect::<Vec<_>>();
    let rows = storage.load_memory_kinds(&ctx.owner, &ids).await?;
    let by_id = rows
        .into_iter()
        .map(|row| (row.memory_id.into_inner(), row.kind))
        .collect::<std::collections::HashMap<_, _>>();
    let mut out = Vec::with_capacity(ids.len());
    for memory_id in memory_ids {
        let kind = by_id.get(memory_id).copied().ok_or_else(|| {
            McpToolError::InvalidInput(format!("source memory {memory_id} not found for owner"))
        })?;
        out.push(kind);
    }
    Ok(out)
}

fn derived_memory_id(owner: &crate::Owner, kind: &str, key: &str) -> uuid::Uuid {
    let (principal_kind, principal_id) = owner.columns();
    let mut buf = Vec::with_capacity(96 + key.len());
    buf.extend_from_slice(principal_kind.as_str().as_bytes());
    buf.push(0);
    buf.extend_from_slice(principal_id.as_bytes());
    buf.push(0);
    buf.extend_from_slice(kind.as_bytes());
    buf.push(0);
    buf.extend_from_slice(key.as_bytes());
    uuid::Uuid::new_v5(&DERIVED_NAMESPACE, &buf)
}

async fn load_provenance_edge_handles(
    ctx: &McpToolCtx,
    memory_id: MemoryId,
    source_ids: &[uuid::Uuid],
) -> Result<Vec<String>, McpToolError> {
    if source_ids.is_empty() {
        return Ok(Vec::new());
    }
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    let targets = source_ids
        .iter()
        .copied()
        .map(MemoryId::new)
        .collect::<Vec<_>>();
    let rows = storage
        .load_memory_edge_ids(&ctx.owner, CORE_DERIVED_FROM_RELATION, memory_id, &targets)
        .await?;
    Ok(rows
        .into_iter()
        .map(|edge_id| ctx.format_edge(edge_id))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::derived_memory_id;
    use crate::{Principal, UserId};
    use uuid::Uuid;

    /// Pins the org-free deterministic `derive` `MemoryId` against drift.
    /// Track B / S0: the v5 key folds principal kind/id ‖ kind ‖ key — no
    /// org. A fixed input must reproduce exactly this uuid.
    #[test]
    fn derived_memory_id_golden_is_org_free() {
        let owner = Principal::User(UserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid literal"),
        ));
        let id = derived_memory_id(&owner, "Abstraction", "golden-key");
        assert_eq!(
            id,
            Uuid::parse_str("cb6d3947-82cc-52be-b0f2-2368ec9c7288").expect("uuid literal")
        );
    }
}

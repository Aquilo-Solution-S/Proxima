use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::{
    AbstractionPayload, AuthorDerivedEdgeInput, AuthorDerivedRequestInput,
    CORE_DERIVED_FROM_RELATION, EdgeAuthorshipKind, MemoryId, SchemaId, SchemaVersion,
    SidecarPayload,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::AgentDerivationV1;

use super::util::normalize_tags;

const MAX_SOURCE_HANDLES: usize = 256;
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
        description = "Your model/agent label, recorded as operator provenance (e.g. `claude-opus-4-8`), 1 to 120 chars."
    )]
    pub model_id: String,
    #[schemars(
        description = "Optional stable idempotency key. Omit or null to derive one from model_id and body."
    )]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Memory space key from core_memory_spaces. The new memory is authored in this space; source handles may be in other readable spaces."
    )]
    pub space: Option<String>,
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
    const NAME: &'static str = "core_derive";
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

            if args.source_handles.len() > MAX_SOURCE_HANDLES {
                return Err(McpToolError::InvalidInput(format!(
                    "source_handles must contain at most {MAX_SOURCE_HANDLES} handles"
                )));
            }
            let space = super::super::memory_spaces::resolve_space_owner(
                &ctx,
                args.space.as_deref(),
                super::super::memory_spaces::SpaceDefault::Current,
            )?;
            let mut seen_sources = std::collections::HashSet::with_capacity(
                args.source_handles.len().min(MAX_SOURCE_HANDLES),
            );
            let mut source_uuids = Vec::with_capacity(args.source_handles.len());
            for handle in &args.source_handles {
                let memory_id = ctx.resolve_memory(handle)?;
                let source_uuid = memory_id.into_inner();
                if seen_sources.insert(source_uuid) {
                    source_uuids.push(source_uuid);
                }
            }

            let relation = if source_uuids.is_empty() {
                None
            } else {
                Some(
                    ctx.registry
                        .resolve_relation(CORE_DERIVED_FROM_RELATION)
                        .ok_or_else(|| {
                            McpToolError::Other(format!(
                                "relation {CORE_DERIVED_FROM_RELATION} not registered"
                            ))
                        })?,
                )
            };

            let key = args.idempotency_key.clone().unwrap_or_else(|| {
                format!(
                    "{}:{}",
                    args.model_id,
                    blake3::hash(body.as_bytes()).to_hex()
                )
            });
            let memory_id = derived_memory_id(&space.owner, args.kind.as_str(), &key);
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
                    .map(|source_id| AuthorDerivedEdgeInput {
                        relation,
                        source_kind: args.kind.to_entity_kind(),
                        source_memory_id: memory_id,
                        target_kind: crate::EntityKind::Fact,
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
                .author_derived_authorized(
                    &ctx.authz,
                    AuthorDerivedRequestInput {
                        memory_id,
                        owner: space.owner,
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
                    },
                )
                .await
                .map_err(map_derive_authoring_error)?;

            let provenance_edge_handles = outcome
                .edge_ids
                .into_iter()
                .map(|edge_id| ctx.format_edge(edge_id))
                .collect();

            Ok(DeriveOutput {
                handle: match args.kind {
                    DerivedKind::Abstraction => ctx.format_abstraction_memory(outcome.memory_id),
                    DerivedKind::Perspective => ctx.format_perspective_memory(outcome.memory_id),
                },
                idempotent_replay: outcome.idempotent_replay,
                provenance_edge_handles,
            })
        })
    }
}

fn map_derive_authoring_error(err: crate::error::ProtocolError) -> McpToolError {
    if err.code == crate::error::ErrorCode::InvalidArgument
        && err.message.contains("relation core/derived-from")
    {
        return McpToolError::LayeringViolation(
            err.message
                .strip_prefix("invalid argument edges: ")
                .unwrap_or(err.message.as_str())
                .to_string(),
        );
    }
    err.into()
}

fn derived_memory_id(owner: &crate::Owner, kind: &str, key: &str) -> uuid::Uuid {
    let principal_kind = crate::OwnerRefKind::of(owner);
    let principal_id = owner.stable_key_uuid();
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

#[cfg(test)]
mod tests {
    use super::{MAX_SOURCE_HANDLES, derived_memory_id};
    use crate::{OwnerRef, UserId};
    use uuid::Uuid;

    /// Pins the org-free deterministic `derive` `MemoryId` against drift.
    /// Track B / S0: the v5 key folds principal kind/id ‖ kind ‖ key — no
    /// org. A fixed input must reproduce exactly this uuid.
    #[test]
    fn derived_memory_id_golden_is_org_free() {
        let owner = OwnerRef::Personal(UserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid literal"),
        ));
        let id = derived_memory_id(&owner, "Abstraction", "golden-key");
        assert_eq!(
            id,
            Uuid::parse_str("b12eb286-ac4d-5eea-9854-ff88dd16e42c").expect("uuid literal")
        );
    }

    #[test]
    fn source_handle_cap_is_pinned() {
        assert_eq!(MAX_SOURCE_HANDLES, 256);
    }
}

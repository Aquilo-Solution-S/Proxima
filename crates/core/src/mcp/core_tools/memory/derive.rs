use crate::mcp::{McpTool, McpToolCtx, McpToolError, MemoryHandleClass};
use crate::protocol::tool as protocol_tool;
use crate::{
    AbstractionPayload, AuthorDerivedEdgeInput, AuthorDerivedRequestInput,
    CORE_DERIVED_FROM_RELATION, InputContractId, MemoryId, OperatorId, SchemaId, SchemaVersion,
    SidecarPayload,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::AgentDerivationV1;

use super::util::{normalize_idempotency_key, normalize_tags};

const MAX_SOURCE_HANDLES: usize = 256;
const DERIVED_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x9d, 0xc1, 0x37, 0x10, 0x4f, 0xa6, 0x4c, 0x4e, 0x95, 0x73, 0xc8, 0x18, 0x9d, 0xfb, 0xa7, 0x40,
]);
const CORE_DERIVE_OPERATOR_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x37, 0x20, 0xd3, 0x2a, 0xa1, 0x92, 0x49, 0x8d, 0x8c, 0x46, 0x3f, 0xad, 0x61, 0x2d, 0x9a, 0x04,
]);
const CORE_DERIVE_INPUT_CONTRACT_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x3d, 0x1a, 0x60, 0x6d, 0x1c, 0xb9, 0x47, 0x0d, 0x9a, 0x49, 0x17, 0xb9, 0xe3, 0xef, 0xde, 0xaa,
]);

fn core_derive_operator_id(kind: DerivedKind) -> OperatorId {
    OperatorId::new(uuid::Uuid::new_v5(
        &CORE_DERIVE_OPERATOR_NAMESPACE,
        format!("{}:{}", protocol_tool::CORE_DERIVE, kind.as_str()).as_bytes(),
    ))
}

fn core_derive_input_contract_id(kind: DerivedKind) -> InputContractId {
    InputContractId::new(uuid::Uuid::new_v5(
        &CORE_DERIVE_INPUT_CONTRACT_NAMESPACE,
        format!(
            "{}:{}:source-memories-v1",
            protocol_tool::CORE_DERIVE,
            kind.as_str()
        )
        .as_bytes(),
    ))
}

fn resolve_source_memory(
    ctx: &McpToolCtx,
    handle: &str,
) -> Result<(MemoryId, MemoryHandleClass), McpToolError> {
    if let Ok(memory_id) = ctx.resolve_fact_memory(handle) {
        return Ok((memory_id, MemoryHandleClass::Fact));
    }
    if let Ok(memory_id) = ctx.resolve_abstraction_memory(handle) {
        return Ok((memory_id, MemoryHandleClass::Abstraction));
    }
    if let Ok(memory_id) = ctx.resolve_perspective_memory(handle) {
        return Ok((memory_id, MemoryHandleClass::Perspective));
    }
    ctx.resolve_memory(handle)
        .map(|memory_id| (memory_id, MemoryHandleClass::Fact))
}

fn operator_shape(
    kind: DerivedKind,
    sources: &[(MemoryId, MemoryHandleClass)],
) -> Result<(crate::MemoryOperatorKind, crate::EntityKind), McpToolError> {
    let first = sources
        .first()
        .map(|(_memory_id, class)| *class)
        .ok_or_else(|| McpToolError::InvalidInput("source_handles must be nonempty".into()))?;
    if sources.iter().any(|(_memory_id, class)| *class != first) {
        return Err(McpToolError::InvalidInput(
            "source_handles must have one memory layer per operator invocation".into(),
        ));
    }
    match (kind, first) {
        (DerivedKind::Abstraction, MemoryHandleClass::Fact) => {
            Ok((crate::MemoryOperatorKind::FtoA, crate::EntityKind::Fact))
        }
        (DerivedKind::Abstraction, MemoryHandleClass::Abstraction) => Ok((
            crate::MemoryOperatorKind::AtoA,
            crate::EntityKind::Abstraction,
        )),
        (DerivedKind::Perspective, MemoryHandleClass::Abstraction) => Ok((
            crate::MemoryOperatorKind::AtoP,
            crate::EntityKind::Abstraction,
        )),
        (DerivedKind::Abstraction | DerivedKind::Perspective, MemoryHandleClass::Perspective)
        | (DerivedKind::Perspective, MemoryHandleClass::Fact) => {
            Err(McpToolError::LayeringViolation(format!(
                "{} cannot be derived from {first} sources",
                kind.as_str()
            )))
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeriveArgs {
    #[schemars(description = "Derived memory kind to author: Abstraction or Perspective.")]
    pub kind: DerivedKind,
    #[schemars(
        description = "Short title for the derived memory, 1 to 240 chars. Leading and trailing whitespace is removed before the length check."
    )]
    pub title: String,
    #[schemars(
        description = "Body text for the derived memory, 1 to 20000 chars. Leading and trailing whitespace is removed before the length check."
    )]
    pub body: String,
    #[serde(default)]
    #[schemars(
        description = "Optional normalized tags for later search. Use `[]` when no tags are needed."
    )]
    pub tags: Vec<String>,
    #[schemars(
        description = "Required source memory handles for the operator proof. Use only one input layer per call: Facts for F→A, Abstractions for A→A/A→P."
    )]
    pub source_handles: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional model/agent label recorded as operator provenance (e.g. `claude-opus-4-8`), 1 to 120 chars. Defaults to the reserved `model_id` request-context field when omitted."
    )]
    pub model_id: Option<String>,
    #[schemars(
        description = "Optional stable idempotency key. Omit or null to derive one from model_id and body."
    )]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Memory space key from core_memory_spaces. The new memory is authored in this space; source handles may be in other readable spaces."
    )]
    pub space: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional lexical language of the derived text: a PostgreSQL text-search configuration name (e.g. 'german'), an ISO 639 / BCP-47 code (e.g. 'de', 'de-DE'), or 'auto' to detect it from title+body (an unreliable detection falls back to the database default). Affects lexical search tokenisation only; embeddings are language-agnostic. Omit for the database default."
    )]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
pub enum DerivedKind {
    #[serde(alias = "abstraction", alias = "ABSTRACTION")]
    Abstraction,
    #[serde(alias = "perspective", alias = "PERSPECTIVE")]
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
    const NAME: &'static str = protocol_tool::CORE_DERIVE;
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
            // 240 matches the goal-title cap: same-named field, same bound
            // on every authoring surface.
            if title.is_empty() || title.chars().count() > 240 {
                return Err(McpToolError::InvalidInput(
                    "title must be 1..=240 chars".into(),
                ));
            }
            if body.is_empty() || body.chars().count() > 20_000 {
                return Err(McpToolError::InvalidInput(
                    "body must be 1..=20000 chars".into(),
                ));
            }
            let idempotency_key = normalize_idempotency_key(args.idempotency_key)?;
            // `model_id` is the reserved operator label. It may arrive as an
            // explicit arg or via the request-context `model_id` (which the MCP
            // server strips into `ctx.author.model_id`); fall back to the latter.
            let model_id = args
                .model_id
                .clone()
                .unwrap_or_else(|| ctx.author.model_id.clone());
            if model_id.trim().is_empty() || model_id.chars().count() > 120 {
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
            let mut sources = Vec::with_capacity(args.source_handles.len());
            for handle in &args.source_handles {
                let (memory_id, class) = resolve_source_memory(&ctx, handle)?;
                let source_uuid = memory_id.into_inner();
                if seen_sources.insert(source_uuid) {
                    sources.push((memory_id, class));
                }
            }

            if sources.is_empty() {
                return Err(McpToolError::InvalidInput(
                    "source_handles must be nonempty for operator derivation".into(),
                ));
            }
            let relation = Some(
                ctx.registry
                    .resolve_relation(CORE_DERIVED_FROM_RELATION)
                    .ok_or_else(|| {
                        McpToolError::Other(format!(
                            "relation {CORE_DERIVED_FROM_RELATION} not registered"
                        ))
                    })?,
            );

            let lexical_language = crate::lexical_language::resolve_lexical_language(
                args.language.as_deref(),
                &format!("{title}\n{body}"),
            )
            .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
            let key = idempotency_key.clone().unwrap_or_else(|| {
                format!("{}:{}", model_id, blake3::hash(body.as_bytes()).to_hex())
            });
            let memory_id = derived_memory_id(&space.owner, args.kind.as_str(), &key);
            let sidecar = AgentDerivationV1 {
                title: title.to_string(),
                body: body.to_string(),
                tags: tags.clone(),
                idempotency_key: idempotency_key.clone(),
                source_memory_ids: sources
                    .iter()
                    .map(|(memory_id, _class)| memory_id.into_inner())
                    .collect(),
                model_id: model_id.clone(),
                client_name: ctx.author.client_name.clone(),
                client_version: ctx.author.client_version.clone(),
            };

            let memory_id = MemoryId::new(memory_id);
            let (operator_kind, target_kind) = operator_shape(args.kind, &sources)?;
            let edge_authorship = operator_kind.edge_authorship();
            let edges: Vec<_> = relation.map_or_else(Vec::new, |relation| {
                sources
                    .iter()
                    .map(|(source_id, _class)| AuthorDerivedEdgeInput {
                        relation,
                        source_kind: args.kind.to_entity_kind(),
                        source_memory_id: memory_id,
                        target_kind,
                        target_memory_id: *source_id,
                        authorship_kind: edge_authorship,
                        authorship_owner_memory_id: ctx.caller_self_perspective,
                    })
                    .collect()
            });
            let engine = ctx.require_engine()?;
            // Consolidating a keyed source batch (grouped core_remember
            // writes) is what completes it: close the F→A input batch if
            // it is still open so the closed-batch gate passes.
            if matches!(operator_kind, crate::MemoryOperatorKind::FtoA) {
                let source_ids: Vec<MemoryId> = sources
                    .iter()
                    .map(|(memory_id, _class)| *memory_id)
                    .collect();
                engine
                    .close_ftoa_source_batch_if_open(&ctx.authz, space.owner, &source_ids)
                    .await?;
            }
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
                        operator_kind,
                        operator_id: core_derive_operator_id(args.kind),
                        input_contract_id: core_derive_input_contract_id(args.kind),
                        source_batch_id: None,
                        model_id: &model_id,
                        prompt_version: "mcp-agent-v1",
                        sidecar_payload: match args.kind {
                            DerivedKind::Abstraction => SidecarPayload::abstraction(sidecar),
                            DerivedKind::Perspective => SidecarPayload::perspective(sidecar),
                        },
                        supersedes: None,
                        lexical_language: lexical_language.as_deref(),
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
    use super::{DerivedKind, MAX_SOURCE_HANDLES, derived_memory_id};
    use crate::{OwnerRef, UserId};
    use uuid::Uuid;

    #[test]
    fn derived_kind_accepts_mixed_case() {
        assert!(matches!(
            serde_json::from_value::<DerivedKind>(serde_json::json!("abstraction")).unwrap(),
            DerivedKind::Abstraction
        ));
        assert!(matches!(
            serde_json::from_value::<DerivedKind>(serde_json::json!("PERSPECTIVE")).unwrap(),
            DerivedKind::Perspective
        ));
        assert!(matches!(
            serde_json::from_value::<DerivedKind>(serde_json::json!("Abstraction")).unwrap(),
            DerivedKind::Abstraction
        ));
    }

    /// Pins the org-free deterministic `derive` `MemoryId` against drift.
    /// Org-free: the v5 key folds principal kind/id ‖ kind ‖ key — no
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

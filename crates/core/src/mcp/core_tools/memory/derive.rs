use crate::mcp::{McpTool, McpToolCtx, McpToolError, MemoryHandleClass};
use crate::protocol::tool as protocol_tool;
use crate::tool::validate_trimmed_len;
use crate::{
    AbstractionPayload, AuthorDerivedRequestInput, EdgeEndpoint, InputContractId, MemoryId,
    OperatorId, SchemaId, SchemaVersion, SidecarPayload,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::AgentDerivationV1;

use super::util::{normalize_idempotency_key, normalize_tags};

pub(crate) const MAX_SOURCE_HANDLES: usize = 256;
const DERIVED_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x9d, 0xc1, 0x37, 0x10, 0x4f, 0xa6, 0x4c, 0x4e, 0x95, 0x73, 0xc8, 0x18, 0x9d, 0xfb, 0xa7, 0x40,
]);
const CORE_DERIVE_OPERATOR_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x37, 0x20, 0xd3, 0x2a, 0xa1, 0x92, 0x49, 0x8d, 0x8c, 0x46, 0x3f, 0xad, 0x61, 0x2d, 0x9a, 0x04,
]);
const CORE_DERIVE_INPUT_CONTRACT_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x3d, 0x1a, 0x60, 0x6d, 0x1c, 0xb9, 0x47, 0x0d, 0x9a, 0x49, 0x17, 0xb9, 0xe3, 0xef, 0xde, 0xaa,
]);

pub(crate) fn core_derive_operator_id(kind: DerivedKind) -> OperatorId {
    OperatorId::new(uuid::Uuid::new_v5(
        &CORE_DERIVE_OPERATOR_NAMESPACE,
        format!("{}:{}", protocol_tool::CORE_DERIVE, kind.as_str()).as_bytes(),
    ))
}

pub(crate) fn core_derive_input_contract_id(kind: DerivedKind) -> InputContractId {
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

pub(crate) fn resolve_source_memory(
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

pub(crate) fn operator_shape(
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
        length(max = 240),
        description = "Short title for the derived memory, 1 to 240 chars. Leading and trailing whitespace is removed before the length check."
    )]
    pub title: String,
    #[schemars(
        length(max = 20000),
        description = "Body text for the derived memory, 1 to 20000 chars. Leading and trailing whitespace is removed before the length check."
    )]
    pub body: String,
    #[serde(default)]
    #[schemars(
        length(max = 16),
        description = "Optional tags for later search, at most 16. Each is stored trimmed and lowercased, so `Rust` is stored and matched as `rust`. Use `[]` when no tags are needed."
    )]
    pub tags: Vec<String>,
    #[schemars(
        description = "Required source memory handles for the operator proof. Use only one input layer per call: Facts for F→A, Abstractions for A→A/A→P."
    )]
    pub source_handles: Vec<String>,
    #[serde(default)]
    #[schemars(
        length(max = 120),
        description = "Optional model/agent label recorded as operator provenance (e.g. `example-model`), 1 to 120 chars. Defaults to the reserved `model_id` request-context field when omitted."
    )]
    pub model_id: Option<String>,
    #[schemars(
        description = "Optional stable idempotency key. Omit or null to derive one from model_id and the authored content (title, body, tags), so two derivations that differ in any of them are two writes. Supplying one asserts that calls sharing it are the same derivation even if the text differs; the first body written under a key is the one kept."
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
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Abstraction => "Abstraction",
            Self::Perspective => "Perspective",
        }
    }

    pub(crate) fn to_entity_kind(self) -> crate::EntityKind {
        match self {
            Self::Abstraction => crate::EntityKind::Abstraction,
            Self::Perspective => crate::EntityKind::Perspective,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DeriveOutput {
    pub handle: String,
    pub idempotent_replay: bool,
    /// Number of index rows this write asserted. An edge has no handle
    /// to return: the row is `(source, target, kind)` and re-running the
    /// same derivation re-asserts the same rows.
    pub edge_count: usize,
    /// Present only when the memory landed without a vector because the
    /// embedding provider refused its text. The memory is written and
    /// lexically findable; a pending embedding job will give it a vector
    /// (in bisected pieces if the text was over the provider's limit), so
    /// semantic search will not find it until a drain runs.
    ///
    /// Skipped when false so the ordinary response is byte-identical to
    /// what it has always been.
    #[serde(skip_serializing_if = "core::ops::Not::not")]
    pub embedding_deferred: bool,
}

#[derive(Debug)]
pub struct DeriveTool;

impl McpTool for DeriveTool {
    const NAME: &'static str = protocol_tool::CORE_DERIVE;
    const DESCRIPTION: &'static str =
        "Author an Abstraction or Perspective derived from existing memory handles.";
    type Args = DeriveArgs;
    type Output = DeriveOutput;

    fn call(
        ctx: McpToolCtx,
        args: DeriveArgs,
    ) -> futures::future::BoxFuture<'static, Result<DeriveOutput, McpToolError>> {
        Box::pin(async move {
            let authored = authored_derivation(
                &ctx,
                DerivationFields {
                    title: &args.title,
                    body: &args.body,
                    tags: &args.tags,
                    model_id: args.model_id.as_deref(),
                    idempotency_key: args.idempotency_key.as_deref(),
                },
            )?;

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
            let sources = resolve_source_handles(&ctx, &args.source_handles)?;
            let lexical_language = crate::lexical_language::resolve_lexical_language(
                args.language.as_deref(),
                &format!("{}\n{}", authored.title, authored.body),
            )
            .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
            let DerivationPlan {
                memory_id,
                operator_kind,
                derived_from,
                sidecar,
                authored,
            } = plan_derivation(&ctx, &space.owner, args.kind, authored, &sources)?;
            let engine = ctx.require_engine()?;
            let outcome = engine
                .author_derived_authorized(
                    &ctx.authz,
                    AuthorDerivedRequestInput {
                        memory_id,
                        owner: space.owner,
                        kind: args.kind.to_entity_kind(),
                        text: authored.body.clone(),
                        schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
                        schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
                        operator_kind,
                        operator_id: core_derive_operator_id(args.kind),
                        input_contract_id: core_derive_input_contract_id(args.kind),
                        model_id: &authored.model_id,
                        sidecar_payload: match args.kind {
                            DerivedKind::Abstraction => SidecarPayload::abstraction(sidecar),
                            DerivedKind::Perspective => SidecarPayload::perspective(sidecar),
                        },
                        derived_from: &derived_from,
                        extra_refs: &[],
                        supersedes: None,
                        lexical_language: Some(lexical_language.as_str()),
                    },
                )
                .await
                .map_err(map_derive_authoring_error)?;

            Ok(DeriveOutput {
                handle: match args.kind {
                    DerivedKind::Abstraction => ctx.format_abstraction_memory(outcome.memory_id),
                    DerivedKind::Perspective => ctx.format_perspective_memory(outcome.memory_id),
                },
                idempotent_replay: outcome.idempotent_replay,
                edge_count: outcome.edge_count,
                embedding_deferred: outcome.embedding_deferred,
            })
        })
    }
}

/// What a `core_derive` request carries once it is validated: the authored
/// text, the operator label, and the tags, each in the exact form the write
/// stores and the idempotency key is derived from.
pub(crate) struct AuthoredDerivation {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) model_id: String,
    pub(crate) tags: Vec<String>,
    pub(crate) idempotency_key: Option<String>,
}

/// The raw, still-unvalidated authoring fields every derived-memory surface
/// carries. Borrowed rather than owned so a caller can hand over an arg
/// struct's fields without cloning them first; the arg structs themselves
/// stay per-surface because they are the `JsonSchema`-derived tool contract.
#[derive(Clone, Copy)]
pub(crate) struct DerivationFields<'a> {
    pub(crate) title: &'a str,
    pub(crate) body: &'a str,
    pub(crate) tags: &'a [String],
    pub(crate) model_id: Option<&'a str>,
    pub(crate) idempotency_key: Option<&'a str>,
}

/// Validate and normalize the authoring fields of a derived-memory write.
///
/// # Errors
///
/// Returns [`McpToolError::InvalidInput`] when the title, body, operator
/// label, an idempotency key, or a tag is blank or over its cap.
pub(crate) fn authored_derivation(
    ctx: &McpToolCtx,
    fields: DerivationFields<'_>,
) -> Result<AuthoredDerivation, McpToolError> {
    // 240 matches the goal-title cap: same-named field, same bound
    // on every authoring surface.
    let title = validate_trimmed_len("title", fields.title, 240)?.to_string();
    let body = validate_trimmed_len("body", fields.body, 20_000)?.to_string();
    let idempotency_key =
        normalize_idempotency_key(fields.idempotency_key.map(ToString::to_string))?;
    // `model_id` is the reserved operator label. It may arrive as an
    // explicit arg or via the request-context `model_id` (which the MCP
    // server strips into `ctx.author.model_id`); fall back to the latter.
    let raw_model_id = fields
        .model_id
        .map_or_else(|| ctx.author.model_id.clone(), ToString::to_string);
    // Trimmed before it is *used*, not just before it is checked: the
    // stored label and the idempotency key derived from it must be
    // the same string, or `" example "` and `"example"` are one label
    // to the validator and two to the dedup key.
    let model_id = validate_trimmed_len("model_id", &raw_model_id, 120)?.to_string();
    let tags = normalize_tags(fields.tags.to_vec())?;
    Ok(AuthoredDerivation {
        title,
        body,
        model_id,
        tags,
        idempotency_key,
    })
}

/// Everything a derived-memory write needs that follows from the authored
/// content plus the declared sources, and nothing that follows from the
/// call site: the port (`Engine` vs `UnitOfWork`), `extra_refs`,
/// `supersedes`, the lexical language and the rendered output stay where
/// they are, because those genuinely differ between `core_derive` and
/// `core_episode_commit`.
pub(crate) struct DerivationPlan {
    pub(crate) memory_id: MemoryId,
    pub(crate) operator_kind: crate::MemoryOperatorKind,
    pub(crate) derived_from: Vec<EdgeEndpoint>,
    pub(crate) sidecar: AgentDerivationV1,
    pub(crate) authored: AuthoredDerivation,
}

/// Fold the authored content and the resolved sources into the identity,
/// operator shape, sidecar and declared inputs of one derived write.
///
/// # Errors
///
/// Returns [`McpToolError`] when `sources` are empty, mix memory layers, or
/// name a layer this `kind` cannot be derived from — see [`operator_shape`].
pub(crate) fn plan_derivation(
    ctx: &McpToolCtx,
    owner: &crate::Owner,
    kind: DerivedKind,
    authored: AuthoredDerivation,
    sources: &[(MemoryId, MemoryHandleClass)],
) -> Result<DerivationPlan, McpToolError> {
    let key = authored
        .idempotency_key
        .clone()
        .unwrap_or_else(|| content_idempotency_key(&authored));
    let memory_id = MemoryId::new(derived_memory_id(owner, kind.as_str(), &key));
    let sidecar = derivation_sidecar(ctx, &authored, sources);
    let (operator_kind, target_kind) = operator_shape(kind, sources)?;
    // The declaration is a list of targets. Its kind — `origin` —
    // follows from what this operation IS, so there is nothing
    // here for the caller to pick.
    let derived_from: Vec<EdgeEndpoint> = sources
        .iter()
        .map(|(source_id, _class)| EdgeEndpoint::memory(target_kind, *source_id))
        .collect();
    Ok(DerivationPlan {
        memory_id,
        operator_kind,
        derived_from,
        sidecar,
        authored,
    })
}

/// The declared sources, resolved to memories in request order with repeats
/// dropped: a handle named twice is one input, and no input at all is not a
/// derivation.
fn resolve_source_handles(
    ctx: &McpToolCtx,
    handles: &[String],
) -> Result<Vec<(MemoryId, MemoryHandleClass)>, McpToolError> {
    let mut seen_sources =
        std::collections::HashSet::with_capacity(handles.len().min(MAX_SOURCE_HANDLES));
    let mut sources = Vec::with_capacity(handles.len());
    for handle in handles {
        let (memory_id, class) = resolve_source_memory(ctx, handle)?;
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
    Ok(sources)
}

/// The auto-derived idempotency key, which must cover everything this write
/// stores that the storage-side replay proof does not.
///
/// That proof (`validate_derived_replay_equivalent`) compares the
/// `memories` row: text, kind, operator, model, owner. `text` is
/// the body alone — the title and tags live in the
/// `agent_derivation_v1` sidecar, which the proof never reads and
/// which the replay path never even writes. Hashing the body alone
/// would make two derivations with one body and different titles a
/// single write: the second caller gets `idempotent_replay: true`
/// and a handle to somebody else's Abstraction, with their own
/// title silently discarded. A success flag over dropped content
/// is the worst shape a write can fail in.
///
/// An *explicit* key is deliberately left alone. There the caller
/// is asserting "this is the same derivation" — that is what the
/// parameter is for, and a re-generated body differing by a space
/// must still replay.
pub(crate) fn content_idempotency_key(authored: &AuthoredDerivation) -> String {
    let mut content = blake3::Hasher::new();
    content.update(authored.title.as_bytes());
    content.update(b"\0");
    content.update(authored.body.as_bytes());
    for tag in &authored.tags {
        content.update(b"\0");
        content.update(tag.as_bytes());
    }
    format!("{}:{}", authored.model_id, content.finalize().to_hex())
}

/// The typed sidecar this write stores beside the memory row.
pub(crate) fn derivation_sidecar(
    ctx: &McpToolCtx,
    authored: &AuthoredDerivation,
    sources: &[(MemoryId, MemoryHandleClass)],
) -> AgentDerivationV1 {
    AgentDerivationV1 {
        title: authored.title.clone(),
        body: authored.body.clone(),
        tags: authored.tags.clone(),
        idempotency_key: authored.idempotency_key.clone(),
        source_memory_ids: sources
            .iter()
            .map(|(memory_id, _class)| memory_id.into_inner())
            .collect(),
        model_id: authored.model_id.clone(),
        client_name: ctx.author.client_name.clone(),
        client_version: ctx.author.client_version.clone(),
    }
}

pub(crate) fn map_derive_authoring_error(err: crate::error::ProtocolError) -> McpToolError {
    if err.code == crate::error::ErrorCode::InvalidArgument
        && err.message.contains("layering violation")
    {
        return McpToolError::LayeringViolation(
            err.message
                .strip_prefix("invalid argument derived_from: ")
                .unwrap_or(err.message.as_str())
                .to_string(),
        );
    }
    err.into()
}

pub(crate) fn derived_memory_id(owner: &crate::Owner, kind: &str, key: &str) -> uuid::Uuid {
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
    use super::super::super::memory_spaces::test_ctx::ctx_for;
    use super::{
        AuthoredDerivation, DerivedKind, MAX_SOURCE_HANDLES, content_idempotency_key,
        derivation_sidecar, derived_memory_id,
    };
    use crate::mcp::MemoryHandleClass;
    use crate::{AgentDerivationV1, MemoryId, OwnerRef, UserId};
    use uuid::Uuid;

    fn golden_authored(idempotency_key: Option<&str>) -> AuthoredDerivation {
        AuthoredDerivation {
            title: "Golden title".into(),
            body: "Golden body".into(),
            model_id: "example-model".into(),
            tags: vec!["alpha".into(), "beta".into()],
            idempotency_key: idempotency_key.map(ToString::to_string),
        }
    }

    /// Pins the auto-derived idempotency key byte for byte: blake3 over
    /// `title \0 body (\0 tag)*`, rendered `"{model_id}:{hex}"`. Two
    /// authoring surfaces compute this key, and a drift in either one
    /// silently re-points every future write's `derived_memory_id` — a
    /// replay that is no longer a replay, or a replay that swallows a
    /// different body.
    #[test]
    fn content_idempotency_key_golden() {
        assert_eq!(
            content_idempotency_key(&golden_authored(None)),
            "example-model:47dcfe4f790ea0580ce015da6dd0f56ed2332cb328f37edf37e4c00e14581f1b",
        );
    }

    /// The tags participate in the hash and their order is the normalized
    /// (sorted, deduped) one, so a tag change is a different write.
    #[test]
    fn content_idempotency_key_covers_title_body_and_tags() {
        let base = content_idempotency_key(&golden_authored(None));
        let mut other_title = golden_authored(None);
        other_title.title = "Other title".into();
        let mut other_body = golden_authored(None);
        other_body.body = "Other body".into();
        let mut other_tags = golden_authored(None);
        other_tags.tags = vec!["alpha".into()];
        let mut other_model = golden_authored(None);
        other_model.model_id = "other-model".into();
        for variant in [other_title, other_body, other_tags, other_model] {
            assert_ne!(content_idempotency_key(&variant), base);
        }
    }

    /// Pins the exact sidecar field set. `idempotency_key` holds the
    /// *explicit* key only: storing the auto-derived one instead would
    /// change the sidecar bytes of every key-less derivation.
    #[test]
    fn derivation_sidecar_stores_only_an_explicit_key() {
        let ctx = ctx_for(
            UserId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid")),
            Vec::new(),
        );
        let source = MemoryId::new(
            Uuid::parse_str("00000000-0000-0000-0000-0000000000aa").expect("uuid literal"),
        );
        let sources = [(source, MemoryHandleClass::Fact)];

        assert_eq!(
            derivation_sidecar(&ctx, &golden_authored(None), &sources),
            AgentDerivationV1 {
                title: "Golden title".into(),
                body: "Golden body".into(),
                tags: vec!["alpha".into(), "beta".into()],
                idempotency_key: None,
                source_memory_ids: vec![source.into_inner()],
                model_id: "example-model".into(),
                client_name: "test".into(),
                client_version: "0".into(),
            },
        );
        assert_eq!(
            derivation_sidecar(&ctx, &golden_authored(Some("explicit-key")), &sources)
                .idempotency_key,
            Some("explicit-key".to_string()),
        );
    }

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

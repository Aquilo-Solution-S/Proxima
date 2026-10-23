use crate::mcp::{McpTool, McpToolCtx, McpToolError, MemoryHandleClass};
use crate::mcp::{Replay, ToolEffect};
use crate::protocol::tool as protocol_tool;
use crate::tool::validate_trimmed_len;
use crate::{
    DerivationIdentity, DerivedMemory, InputContractId, MemoryId, OperatorId, SeriesHandle,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::AgentDerivationV1;

use super::super::other_stores::{self, ResolvedMemory};

use super::util::{normalize_idempotency_key, normalize_tags, operator_label};

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

pub(crate) async fn resolve_source_memory(
    ctx: &McpToolCtx,
    handle: &str,
) -> Result<(MemoryId, MemoryHandleClass), McpToolError> {
    ctx.resolve_memory_with_kind(handle).await
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
        description = "Short title, 1 to 240 chars after trimming."
    )]
    pub title: String,
    #[schemars(
        length(max = 20000),
        description = "Body text, 1 to 20000 chars after trimming."
    )]
    pub body: String,
    #[serde(default)]
    #[schemars(
        length(max = 16),
        description = "Search tags, 1 to 48 chars each; stored and matched lowercase."
    )]
    pub tags: Vec<String>,
    #[schemars(
        description = "At least one handle the new memory is made from, all of one layer: Facts (`<uuid>`) or Abstractions (`<uuid>`) for an Abstraction, Abstractions for a Perspective."
    )]
    pub source_handles: Vec<String>,
    #[serde(default)]
    #[schemars(
        length(max = 120),
        description = "Label of the model/agent authoring this, e.g. `example-model`. Omit for the host-supplied model id."
    )]
    pub model_id: Option<String>,
    #[schemars(
        description = "Optional key, 1 to 180 chars, naming this one request: same key and content replays; changed content is `idempotency_conflict`. Omit to key on title, body, tags and sources. Use `supersedes` for a new version."
    )]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional memory handle of a live Abstraction or Perspective of the same kind to revise. Requires a new `idempotency_key`."
    )]
    pub supersedes: Option<String>,
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
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DeriveOutput {
    pub handle: String,
    pub idempotent_replay: bool,
    /// Present (true) only on a replay of a memory that has been forgotten:
    /// the write was not repeated and the memory stays forgotten.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub forgotten: bool,
    /// Number of index rows this write asserted. An edge has no handle
    /// to return: the row is `(source, target, kind)` and re-running the
    /// same derivation re-asserts the same rows.
    pub edge_count: usize,
    /// Present when this admission queued embedding work in its memory
    /// transaction. Admission never calls the provider; the memory is
    /// lexically findable before a drain makes its vectors available.
    /// Omitted for nonembeddable schemas, routes with no clients, and
    /// idempotent replays, which queue no new work.
    #[serde(skip_serializing_if = "core::ops::Not::not")]
    pub embedding_deferred: bool,
}

#[derive(Debug)]
pub struct DeriveTool;

impl McpTool for DeriveTool {
    const NAME: &'static str = protocol_tool::CORE_DERIVE;
    const TITLE: &'static str = "Derive a memory";
    const DESCRIPTION: &'static str = "Author a memory made from others: an Abstraction \
         over Facts or Abstractions (a pattern), or a Perspective over Abstractions (a stance or \
         self-model). `source_handles` become its provenance.";
    // Idempotent by content: the derived memory id is folded from the
    // claim, so re-asserting it lands on one memory.
    const EFFECT: Option<ToolEffect> = Some(ToolEffect::Additive(Replay::Idempotent));
    type Args = DeriveArgs;
    type Output = DeriveOutput;

    fn call(
        ctx: McpToolCtx,
        args: DeriveArgs,
    ) -> futures::future::BoxFuture<'static, Result<DeriveOutput, McpToolError>> {
        Box::pin(derive_call(ctx, args))
    }
}

async fn resolve_derive_sources(
    ctx: &McpToolCtx,
    source_handles: &[String],
) -> Result<
    (
        Vec<(crate::MemoryId, MemoryHandleClass)>,
        Vec<ResolvedMemory>,
    ),
    McpToolError,
> {
    if source_handles.len() > MAX_SOURCE_HANDLES {
        return Err(McpToolError::InvalidInput(format!(
            "source_handles must contain at most {MAX_SOURCE_HANDLES} handles"
        )));
    }
    let resolved = other_stores::resolve_references(ctx, source_handles).await?;
    let sources = resolved
        .iter()
        .map(ResolvedMemory::classified)
        .collect::<Result<Vec<_>, _>>()?;
    if !resolved
        .iter()
        .any(|source| matches!(source, ResolvedMemory::Local(..)))
        && !resolved.is_empty()
    {
        return Err(McpToolError::InvalidInput(
            "source_handles must include at least one memory of this store; memories of other stores only join local ones".into(),
        ));
    }
    Ok((sources, resolved))
}

async fn derive_call(ctx: McpToolCtx, args: DeriveArgs) -> Result<DeriveOutput, McpToolError> {
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

    let space = super::super::memory_spaces::resolve_space_owner(
        &ctx,
        args.space.as_deref(),
        super::super::memory_spaces::SpaceDefault::Current,
    )?;
    let (sources, resolved) = resolve_derive_sources(&ctx, &args.source_handles).await?;
    let lexical_language = crate::lexical_language::resolve_lexical_language(
        args.language.as_deref(),
        &format!("{}\n{}", authored.title, authored.body),
    )
    .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
    let engine = ctx.require_engine()?;
    let origins = sources
        .iter()
        .map(|(id, _class)| *id)
        .filter(|id| crate::memory_store_number(id.into_inner()) == engine.store_number())
        .collect::<Vec<_>>();
    let external = resolved
        .into_iter()
        .filter_map(|source| match source {
            ResolvedMemory::Local(..) => None,
            ResolvedMemory::Remote(origin) => Some(origin),
        })
        .collect::<Vec<_>>();
    let external_ids: Vec<uuid::Uuid> = external
        .iter()
        .map(crate::storage_ports::other_stores::ReadOrigin::handle)
        .collect();
    let identity = DerivationIdentity::new(
        core_derive_operator_id(args.kind),
        core_derive_input_contract_id(args.kind),
    );
    let supersedes = resolve_supersedes(
        &ctx,
        args.kind,
        args.idempotency_key.as_deref(),
        args.supersedes.as_deref(),
    )
    .await?;
    let request = derive_request_key(
        args.kind,
        &authored,
        &origins,
        &external_ids,
        supersedes.map(MemoryId::into_inner),
    )?;
    let DerivationPlan {
        memory_id,
        sidecar,
        authored,
    } = plan_derivation(&ctx, &space.owner, args.kind, authored, &sources, &request)?;
    let target = supersedes.map_or(
        crate::MemoryTarget::Series(SeriesHandle::new(memory_id.into_inner())),
        crate::MemoryTarget::Revision,
    );
    let memory = match args.kind {
        DerivedKind::Abstraction => DerivedMemory::abstraction(
            target,
            space.owner,
            authored.body.clone(),
            sidecar,
            origins,
            identity,
        )?,
        DerivedKind::Perspective => DerivedMemory::perspective(
            target,
            space.owner,
            authored.body.clone(),
            sidecar,
            origins,
            identity,
        )?,
    }
    .lexical_language(lexical_language)
    .external_origins(external)
    .request_key(request);
    let outcome = engine
        .derive_memory(&ctx.authz, memory)
        .await
        .map_err(map_derive_authoring_error)?;

    Ok(DeriveOutput {
        handle: outcome.memory_id.into_inner().to_string(),
        idempotent_replay: outcome.idempotent_replay,
        forgotten: outcome.forgotten,
        edge_count: outcome.edge_count,
        embedding_deferred: outcome.embedding_deferred,
    })
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
    let model_id = operator_label(ctx, fields.model_id)?;
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
    pub(crate) sidecar: AgentDerivationV1,
    pub(crate) authored: AuthoredDerivation,
}

/// Fold the authored content and the resolved sources into the identity,
/// operator shape and sidecar of one derived write. The series is named by
/// the request key ([`derive_request_key`]): the caller's key, or for a
/// keyless write the content fingerprint the ledger judges it by, so the
/// series and the ledger cannot disagree about which write this is.
///
/// # Errors
///
/// Returns [`McpToolError`] when `sources` are empty, mix memory layers, or
/// name a layer this `kind` cannot be derived from — see [`operator_shape`].
pub(crate) fn plan_derivation(
    ctx: &McpToolCtx,
    owner: &crate::OwnerId,
    kind: DerivedKind,
    authored: AuthoredDerivation,
    sources: &[(MemoryId, MemoryHandleClass)],
    request: &crate::RequestKey,
) -> Result<DerivationPlan, McpToolError> {
    operator_shape(kind, sources)?;
    let memory_id = MemoryId::new(derived_memory_id(
        owner,
        kind.as_str(),
        request.key().as_str(),
    ));
    let sidecar = derivation_sidecar(ctx, &authored, sources);
    Ok(DerivationPlan {
        memory_id,
        sidecar,
        authored,
    })
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

pub(crate) async fn resolve_supersedes(
    ctx: &McpToolCtx,
    kind: DerivedKind,
    idempotency_key: Option<&str>,
    supersedes: Option<&str>,
) -> Result<Option<MemoryId>, McpToolError> {
    let Some(raw) = supersedes else {
        return Ok(None);
    };
    if idempotency_key.is_none() {
        return Err(McpToolError::InvalidInput(
            "supersedes requires a new idempotency_key".into(),
        ));
    }
    let (prior, class) = ctx.resolve_memory_with_kind(raw).await?;
    let expected = match kind {
        DerivedKind::Abstraction => MemoryHandleClass::Abstraction,
        DerivedKind::Perspective => MemoryHandleClass::Perspective,
    };
    if class != expected {
        return Err(McpToolError::InvalidInput(format!(
            "supersedes must name a live {} memory",
            kind.as_str()
        )));
    }
    Ok(Some(prior))
}

pub(crate) fn derive_request_key(
    kind: DerivedKind,
    authored: &AuthoredDerivation,
    origins: &[MemoryId],
    external_origins: &[uuid::Uuid],
    supersedes_id: Option<uuid::Uuid>,
) -> Result<crate::RequestKey, McpToolError> {
    let scope = match kind {
        DerivedKind::Abstraction => crate::KeyScope::Abstraction,
        DerivedKind::Perspective => crate::KeyScope::Perspective,
    };
    let fp = fingerprint_derivation(scope, authored, origins, external_origins, supersedes_id);
    let key = authored
        .idempotency_key
        .clone()
        .unwrap_or_else(|| fp.to_hex());
    let key = crate::IdempotencyKey::new(key).map_err(McpToolError::InvalidInput)?;
    Ok(crate::RequestKey::new(scope, key, fp))
}

/// Everything a derived write stores that a replay must match: title, body
/// and tags (the sidecar, which the storage replay proof never reads), the
/// kind, local and cross-store origins and `supersedes`. A keyless write is
/// keyed on it, so one body under two titles stays two writes.
fn fingerprint_derivation(
    scope: crate::KeyScope,
    authored: &AuthoredDerivation,
    origins: &[MemoryId],
    external_origins: &[uuid::Uuid],
    supersedes_id: Option<uuid::Uuid>,
) -> crate::ContentFingerprint {
    let mut fp = crate::ContentFingerprint::builder(scope)
        .text("title", &authored.title)
        .text("body", &authored.body)
        .tags(authored.tags.iter().map(String::as_str))
        .bytes("kind", scope.as_str().as_bytes())
        .uuid_set("origins", origins.iter().map(|id| id.into_inner()))
        .uuid_set("external_origins", external_origins.iter().copied());
    fp = match supersedes_id {
        Some(id) => fp.uuid_set("supersedes", [id]),
        None => fp.absent_uuid_set("supersedes"),
    };
    fp.absent_uuid_set("references").build()
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

pub(crate) fn derived_memory_id(owner: &crate::OwnerId, kind: &str, key: &str) -> uuid::Uuid {
    let principal_id = owner.into_inner();
    let mut buf = Vec::with_capacity(96 + key.len());
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
    use super::{AuthoredDerivation, derivation_sidecar};
    use crate::mcp::MemoryHandleClass;
    use crate::{AgentDerivationV1, MemoryId, UserId};
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
}

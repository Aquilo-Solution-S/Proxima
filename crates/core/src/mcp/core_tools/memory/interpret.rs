//! `core_interpret` — author an interpretation Perspective.
//!
//! A claim with a reason and a confidence is a judgment (docs/16
//! §Motivation). Connections to the subjects are index rows derived from
//! the Perspective payload — nobody writes them except by writing the node.

use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::protocol::tool as protocol_tool;
use crate::tool::validate_trimmed_len;
use crate::{
    AuthorDerivedRequestInput, InputContractId, InterpretationSubjectKind, InterpretationV1,
    MemoryId, OperatorId, PerspectivePayload, SchemaId, SchemaVersion, SidecarPayload,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(crate) const MAX_CLAIM_CHARS: usize = 1000;
pub(crate) const MAX_SUBJECTS: usize = 64;

const INTERPRET_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x1b, 0x6a, 0x4c, 0x9e, 0x2d, 0x77, 0x4f, 0x0b, 0xa5, 0x31, 0x8e, 0x24, 0x6c, 0x0d, 0x91, 0xf3,
]);
const INTERPRET_OPERATOR_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x52, 0x18, 0xc7, 0x3f, 0x9a, 0x40, 0x4d, 0x86, 0xb1, 0x0c, 0x7f, 0x5e, 0x33, 0xa8, 0x62, 0x1d,
]);

pub(crate) fn default_confidence() -> u8 {
    80
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InterpretArgs {
    #[schemars(
        length(max = 1000),
        description = "The claim being made about the subjects — what you take them to mean, and why. 1 to 1000 chars, trimmed before the length check."
    )]
    pub claim: String,
    #[serde(default = "default_confidence")]
    #[schemars(
        range(max = 100),
        description = "Confidence in the claim, 0 to 100. Defaults to 80."
    )]
    pub confidence: u8,
    #[schemars(
        length(max = 64),
        description = "Memory handles the claim is about (`F...`, `A...`, or `P...`), at most 64. Any layer may be a subject: the interpretation is the source, so layering is satisfied by construction."
    )]
    pub subjects: Vec<String>,
    #[serde(default)]
    #[schemars(
        length(max = 120),
        description = "Optional model/agent label recorded as operator provenance. Defaults to the reserved `model_id` request-context field."
    )]
    pub model_id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Memory space key from core_memory_spaces. The interpretation is authored in this space; subjects may live in other readable spaces."
    )]
    pub space: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InterpretOutput {
    /// `P:<uuid>` handle of the interpretation Perspective.
    pub handle: String,
    pub idempotent_replay: bool,
    /// Number of index rows the write asserted — one `reference` per
    /// distinct subject. Not handles: an edge has no id.
    pub edge_count: usize,
}

#[derive(Debug)]
pub struct InterpretTool;

impl McpTool for InterpretTool {
    const NAME: &'static str = protocol_tool::CORE_INTERPRET;
    const DESCRIPTION: &'static str =
        "Author an interpretation Perspective: a claim about existing memories, with a confidence.";
    type Args = InterpretArgs;
    type Output = InterpretOutput;

    fn call(
        ctx: McpToolCtx,
        args: InterpretArgs,
    ) -> futures::future::BoxFuture<'static, Result<InterpretOutput, McpToolError>> {
        Box::pin(async move {
            let claim = validate_trimmed_len("claim", &args.claim, MAX_CLAIM_CHARS)?.to_string();
            // Not clamped: a caller who says 140 has said something the
            // scale cannot express, and silently rewriting it to 100
            // would store a number nobody meant.
            if args.confidence > 100 {
                return Err(McpToolError::InvalidInput(
                    "confidence must be 0..=100".into(),
                ));
            }
            if args.subjects.is_empty() {
                return Err(McpToolError::InvalidInput(
                    "an interpretation must be about at least one memory".into(),
                ));
            }
            if args.subjects.len() > MAX_SUBJECTS {
                return Err(McpToolError::InvalidInput(format!(
                    "subjects must contain at most {MAX_SUBJECTS} handles"
                )));
            }
            let raw_model_id = args
                .model_id
                .clone()
                .unwrap_or_else(|| ctx.author.model_id.clone());
            let model_id = validate_trimmed_len("model_id", &raw_model_id, 120)?.to_string();

            let space = super::super::memory_spaces::resolve_space_owner(
                &ctx,
                args.space.as_deref(),
                super::super::memory_spaces::SpaceDefault::Current,
            )?;

            let mut subject_memory_ids = Vec::with_capacity(args.subjects.len());
            let mut subject_kinds = Vec::with_capacity(args.subjects.len());
            for handle in &args.subjects {
                let (memory_id, kind) = resolve_subject(&ctx, handle)?;
                if subject_memory_ids.contains(&memory_id.into_inner()) {
                    continue;
                }
                subject_memory_ids.push(memory_id.into_inner());
                subject_kinds.push(kind);
            }

            let memory_id = MemoryId::new(interpretation_memory_id(
                &space.owner,
                &model_id,
                &claim,
                args.confidence,
                &subject_memory_ids,
            ));
            reject_self_subject(memory_id, &subject_memory_ids)?;

            let payload = InterpretationV1 {
                claim: claim.clone(),
                confidence: args.confidence,
                subject_memory_ids,
                subject_kinds,
                model_id: model_id.clone(),
                client_name: ctx.author.client_name.clone(),
                client_version: ctx.author.client_version.clone(),
            };

            let engine = ctx.require_engine()?;
            let outcome = engine
                .author_derived_authorized(
                    &ctx.authz,
                    AuthorDerivedRequestInput {
                        memory_id,
                        owner: space.owner,
                        kind: crate::EntityKind::Perspective,
                        text: claim,
                        schema_id: SchemaId::new(
                            <InterpretationV1 as PerspectivePayload>::SCHEMA_ID.into(),
                        ),
                        schema_version: SchemaVersion::new(
                            <InterpretationV1 as PerspectivePayload>::SCHEMA_VERSION,
                        ),
                        operator_kind: crate::MemoryOperatorKind::AtoP,
                        operator_id: interpret_operator_id(),
                        input_contract_id: interpret_input_contract_id(),
                        model_id: &model_id,
                        sidecar_payload: SidecarPayload::perspective(payload),
                        // An interpretation consumes nothing. It grounds
                        // through the references its payload carries, so
                        // it declares no derivation and writes no
                        // `origin` rows.
                        derived_from: &[],
                        extra_refs: &[],
                        supersedes: None,
                        lexical_language: None,
                    },
                )
                .await?;

            Ok(InterpretOutput {
                handle: ctx.format_perspective_memory(outcome.memory_id),
                idempotent_replay: outcome.idempotent_replay,
                edge_count: outcome.edge_count,
            })
        })
    }
}

/// A node cannot be about itself: the reference would be a self-loop,
/// which is not a connection between two things.
///
/// Defensive by construction — the interpretation's id folds its own
/// subject list, so naming itself changes the id it was named for — and
/// kept anyway, because "unreachable" is a property of today's id
/// derivation rather than of the rule.
pub(crate) fn reject_self_subject(
    memory_id: MemoryId,
    subjects: &[uuid::Uuid],
) -> Result<(), McpToolError> {
    if subjects.contains(&memory_id.into_inner()) {
        return Err(McpToolError::InvalidInput(
            "an interpretation cannot take itself as a subject".into(),
        ));
    }
    Ok(())
}

pub(crate) fn resolve_subject(
    ctx: &McpToolCtx,
    handle: &str,
) -> Result<(MemoryId, InterpretationSubjectKind), McpToolError> {
    if let Ok(memory_id) = ctx.resolve_fact_memory(handle) {
        return Ok((memory_id, InterpretationSubjectKind::Fact));
    }
    if let Ok(memory_id) = ctx.resolve_abstraction_memory(handle) {
        return Ok((memory_id, InterpretationSubjectKind::Abstraction));
    }
    if let Ok(memory_id) = ctx.resolve_perspective_memory(handle) {
        return Ok((memory_id, InterpretationSubjectKind::Perspective));
    }
    // An unprefixed handle is a Fact by the same convention every other
    // memory-resolving tool uses.
    let memory_id = ctx.resolve_memory(handle)?;
    Ok((memory_id, InterpretationSubjectKind::Fact))
}

pub(crate) fn interpret_operator_id() -> OperatorId {
    OperatorId::new(uuid::Uuid::new_v5(
        &INTERPRET_OPERATOR_NAMESPACE,
        protocol_tool::CORE_INTERPRET.as_bytes(),
    ))
}

pub(crate) fn interpret_input_contract_id() -> InputContractId {
    InputContractId::new(uuid::Uuid::new_v5(
        &INTERPRET_OPERATOR_NAMESPACE,
        format!("{}:subjects-v1", protocol_tool::CORE_INTERPRET).as_bytes(),
    ))
}

/// Deterministic id folded from owner, model, claim, confidence and
/// subjects: re-asserting the same interpretation is one memory, not a
/// growing pile of identical judgments.
pub(crate) fn interpretation_memory_id(
    owner: &crate::Owner,
    model_id: &str,
    claim: &str,
    confidence: u8,
    subjects: &[uuid::Uuid],
) -> uuid::Uuid {
    let principal_kind = crate::OwnerRefKind::of(owner);
    let principal_id = owner.stable_key_uuid();
    let mut buf = Vec::with_capacity(96 + claim.len() + subjects.len() * 16);
    buf.extend_from_slice(principal_kind.as_str().as_bytes());
    buf.push(0);
    buf.extend_from_slice(principal_id.as_bytes());
    buf.push(0);
    buf.extend_from_slice(model_id.as_bytes());
    buf.push(0);
    buf.extend_from_slice(claim.as_bytes());
    buf.push(0);
    buf.push(confidence);
    for subject in subjects {
        buf.push(0);
        buf.extend_from_slice(subject.as_bytes());
    }
    uuid::Uuid::new_v5(&INTERPRET_NAMESPACE, &buf)
}

#[cfg(test)]
mod tests {
    use super::{
        InterpretArgs, InterpretTool, MAX_SUBJECTS, default_confidence, interpretation_memory_id,
        reject_self_subject,
    };
    use crate::mcp::{McpAuthorContext, McpTool, McpToolCtx, McpToolError};
    use crate::{AuthPath, AuthzContext, FlavorRegistry, FlavorServices, OwnerRef, UserId};
    use std::sync::Arc;

    fn ctx(owner: OwnerRef) -> McpToolCtx {
        McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            author: McpAuthorContext {
                model_id: "test-model".into(),
                client_name: "test".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            services: FlavorServices::default(),
            engine: None,
        }
    }

    fn args(subjects: Vec<String>, confidence: u8) -> InterpretArgs {
        InterpretArgs {
            claim: "the outage followed the deploy".into(),
            confidence,
            subjects,
            model_id: Some("test-model".into()),
            space: None,
        }
    }

    fn subject() -> String {
        format!("F:{}", uuid::Uuid::now_v7())
    }

    fn owner() -> OwnerRef {
        OwnerRef::Personal(UserId::new(
            uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid literal"),
        ))
    }

    #[test]
    fn the_default_confidence_is_the_documented_one() {
        assert_eq!(default_confidence(), 80);
        assert_eq!(MAX_SUBJECTS, 64);
    }

    /// The id is a function of what the interpretation SAYS. Two callers
    /// asserting the same claim about the same subjects land on one
    /// memory; a different confidence is a different judgment.
    #[test]
    fn the_memory_id_folds_the_whole_claim() {
        let subject = uuid::Uuid::now_v7();
        let base = interpretation_memory_id(&owner(), "m", "same claim", 80, &[subject]);
        assert_eq!(
            base,
            interpretation_memory_id(&owner(), "m", "same claim", 80, &[subject])
        );
        assert_ne!(
            base,
            interpretation_memory_id(&owner(), "m", "same claim", 81, &[subject])
        );
        assert_ne!(
            base,
            interpretation_memory_id(&owner(), "m", "other claim", 80, &[subject])
        );
        assert_ne!(
            base,
            interpretation_memory_id(&owner(), "m", "same claim", 80, &[uuid::Uuid::now_v7()])
        );
    }

    /// 140 is not a confidence. Rejected rather than clamped: silently
    /// rewriting it to 100 would store a number nobody meant.
    #[tokio::test]
    async fn confidence_past_the_scale_is_refused_not_clamped() {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let err = InterpretTool::call(ctx(owner), args(vec![subject()], 140))
            .await
            .expect_err("140 is not a confidence");
        assert!(
            matches!(&err, McpToolError::InvalidInput(message) if message.contains("0..=100")),
            "unexpected error: {err:?}"
        );

        // The ends of the scale are legal; only past them is not.
        for confidence in [0, 100, default_confidence()] {
            let err = InterpretTool::call(ctx(owner), args(vec![subject()], confidence))
                .await
                .expect_err("no engine is wired, so the write cannot land");
            assert!(
                !matches!(&err, McpToolError::InvalidInput(message) if message.contains("0..=100")),
                "confidence {confidence} must be inside the scale: {err:?}"
            );
        }
    }

    /// An interpretation that took itself as a subject would ask for an
    /// index row from a node to itself, which is not a connection between
    /// two things.
    #[test]
    fn an_interpretation_cannot_take_itself_as_a_subject() {
        let memory_id = crate::MemoryId::new(uuid::Uuid::now_v7());
        let other = uuid::Uuid::now_v7();
        reject_self_subject(memory_id, &[other]).expect("a distinct subject is fine");
        let err = reject_self_subject(memory_id, &[other, memory_id.into_inner()])
            .expect_err("a self-subject must be refused");
        assert!(
            matches!(&err, McpToolError::InvalidInput(message)
                if message.contains("cannot take itself")),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn an_interpretation_must_be_about_something() {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let err = InterpretTool::call(ctx(owner), args(Vec::new(), 80))
            .await
            .expect_err("an empty subject list is refused");
        assert!(
            matches!(&err, McpToolError::InvalidInput(message)
                if message.contains("at least one memory")),
            "unexpected error: {err:?}"
        );
    }
}

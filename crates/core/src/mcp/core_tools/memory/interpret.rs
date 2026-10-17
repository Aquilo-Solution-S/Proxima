//! `core_interpret` — author an interpretation Perspective.
//!
//! A claim with a reason and a confidence is a judgment (docs/concepts.md
//! §The Model). Connections to the subjects are index rows derived from
//! the Perspective payload — nobody writes them except by writing the node.

use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::mcp::{Replay, ToolEffect};
use crate::protocol::tool as protocol_tool;
use crate::tool::validate_trimmed_len;
use crate::{DerivedMemory, InterpretationSubjectKind, InterpretationV1, MemoryId, SeriesHandle};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(crate) const MAX_CLAIM_CHARS: usize = 1000;
pub(crate) const MAX_SUBJECTS: usize = 64;

const INTERPRET_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x1b, 0x6a, 0x4c, 0x9e, 0x2d, 0x77, 0x4f, 0x0b, 0xa5, 0x31, 0x8e, 0x24, 0x6c, 0x0d, 0x91, 0xf3,
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
    #[schemars(range(max = 100), description = "Confidence in the claim.")]
    pub confidence: u8,
    #[schemars(
        length(max = 64),
        description = "At least one memory the claim is about, of any layer: memory UUIDs."
    )]
    pub subjects: Vec<String>,
    #[serde(default)]
    #[schemars(
        length(max = 120),
        description = "Label of the model/agent authoring this. Omit for the host-supplied model id."
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
    /// `<uuid>` handle of the interpretation Perspective.
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
    const DESCRIPTION: &'static str = "Author a Perspective that interprets memories of any \
         layer: a claim with a confidence that points at its subjects rather than being made from \
         them. For a stance made from Abstractions, use core_derive kind=Perspective.";
    // Idempotent by content: the interpretation's memory id is folded from
    // the claim, so re-asserting it lands on one memory.
    const EFFECT: Option<ToolEffect> = Some(ToolEffect::Additive(Replay::Idempotent));
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
            let model_id = super::util::operator_label(&ctx, args.model_id.as_deref())?;

            let space = super::super::memory_spaces::resolve_space_owner(
                &ctx,
                args.space.as_deref(),
                super::super::memory_spaces::SpaceDefault::Current,
            )?;

            let subjects =
                super::super::other_stores::resolve_references(&ctx, &args.subjects).await?;
            let mut local_subjects = Vec::new();
            let mut read_subjects = Vec::new();
            for subject in subjects {
                let (id, kind) = subject.classified()?;
                match subject {
                    super::super::other_stores::ResolvedMemory::Local(..) => {
                        local_subjects.push((id, subject_kind(kind)));
                    }
                    super::super::other_stores::ResolvedMemory::Remote(origin) => {
                        read_subjects.push(origin);
                    }
                }
            }
            if local_subjects.is_empty() {
                return Err(McpToolError::InvalidInput(
                    "subjects must include at least one memory of this store; memories of other stores only join local ones".into(),
                ));
            }
            let external = read_subjects
                .iter()
                .map(crate::storage_ports::other_stores::ReadOrigin::handle)
                .collect::<Vec<_>>();
            let (subject_memory_ids, subject_kinds): (Vec<uuid::Uuid>, Vec<_>) = local_subjects
                .into_iter()
                .map(|(id, kind)| (id.into_inner(), kind))
                .unzip();

            let memory_id = MemoryId::new(interpretation_memory_id(
                &space.owner,
                &model_id,
                &claim,
                args.confidence,
                &subject_memory_ids,
            ));
            let memory_id =
                MemoryId::new(fold_external_subjects(memory_id.into_inner(), &external));
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
                .derive_memory(
                    &ctx.authz,
                    DerivedMemory::interpretation(
                        crate::MemoryTarget::Series(SeriesHandle::new(memory_id.into_inner())),
                        space.owner,
                        claim,
                        payload,
                    )
                    .lexical_language(crate::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT)
                    .external_references(read_subjects),
                )
                .await?;

            Ok(InterpretOutput {
                handle: outcome.memory_id.into_inner().to_string(),
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

pub(crate) async fn resolve_subject(
    ctx: &McpToolCtx,
    handle: &str,
) -> Result<(MemoryId, InterpretationSubjectKind), McpToolError> {
    let (id, class) = ctx.resolve_memory_with_kind(handle).await?;
    Ok((id, subject_kind(class)))
}

fn subject_kind(class: crate::mcp::MemoryHandleClass) -> InterpretationSubjectKind {
    match class {
        crate::mcp::MemoryHandleClass::Fact => InterpretationSubjectKind::Fact,
        crate::mcp::MemoryHandleClass::Abstraction => InterpretationSubjectKind::Abstraction,
        crate::mcp::MemoryHandleClass::Perspective => InterpretationSubjectKind::Perspective,
    }
}

/// Deterministic id folded from owner, model, claim, confidence and
/// subjects: re-asserting the same interpretation is one memory, not a
/// growing pile of identical judgments.
/// Fold other-store subjects into an interpretation's id, so the same
/// claim about different remote memories is a different memory. With none
/// the id is the one [`interpretation_memory_id`] always minted.
pub(crate) fn fold_external_subjects(id: uuid::Uuid, external: &[uuid::Uuid]) -> uuid::Uuid {
    if external.is_empty() {
        return id;
    }
    let mut handles: Vec<String> = external.iter().map(ToString::to_string).collect();
    handles.sort_unstable();
    let mut buf = id.as_bytes().to_vec();
    for handle in handles {
        buf.push(0);
        buf.extend_from_slice(handle.as_bytes());
    }
    uuid::Uuid::new_v5(&INTERPRET_NAMESPACE, &buf)
}

pub(crate) fn interpretation_memory_id(
    owner: &crate::OwnerId,
    model_id: &str,
    claim: &str,
    confidence: u8,
    subjects: &[uuid::Uuid],
) -> uuid::Uuid {
    let principal_id = owner.into_inner();
    let mut buf = Vec::with_capacity(96 + claim.len() + subjects.len() * 16);
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
    use super::{InterpretArgs, InterpretTool, default_confidence, reject_self_subject};
    use crate::mcp::{McpAuthorContext, McpTool, McpToolCtx, McpToolError};
    use crate::{AuthPath, AuthzContext, FlavorRegistry, FlavorServices, OwnerId, UserId};
    use std::sync::Arc;

    fn ctx(owner: OwnerId) -> McpToolCtx {
        McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            author: McpAuthorContext {
                model_id: "test-model".into(),
                trusted_model_id: None,
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
        format!("{}", uuid::Uuid::now_v7())
    }

    /// 140 is not a confidence. Rejected rather than clamped: silently
    /// rewriting it to 100 would store a number nobody meant.
    #[tokio::test]
    async fn confidence_past_the_scale_is_refused_not_clamped() {
        let owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
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
        let owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
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

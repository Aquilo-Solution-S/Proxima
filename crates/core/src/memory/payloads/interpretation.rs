use crate::{EntityKind, MemoryId, PayloadReference, PerspectivePayload};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// An agent's claim about existing nodes.
///
/// A claim with a reason and a confidence is a judgment — a Perspective
/// (docs/16 §Motivation). Subjects are schema-declared reference fields;
/// index rows are re-derivable from this payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InterpretationV1 {
    /// The claim being made about the subjects.
    pub claim: String,
    /// How strongly the author holds the claim, 0..=100.
    pub confidence: u8,
    /// Memories the claim is about. Each becomes a `Reference` index
    /// entry sourced at this Perspective.
    pub subject_memory_ids: Vec<uuid::Uuid>,
    /// Entity kind of each subject, positionally aligned with
    /// `subject_memory_ids`. Carried because a reference declaration
    /// needs its target's kind and this payload is the only place the
    /// resolved kinds survive re-derivation.
    pub subject_kinds: Vec<InterpretationSubjectKind>,
    pub model_id: String,
    pub client_name: String,
    pub client_version: String,
}

/// Memory layer of an interpretation subject. A Perspective may
/// interpret any layer — the layering rule is satisfied because the
/// Perspective, not the subject, is the source.
///
/// Closed here and closed in the database: the discriminators match the
/// SQL enum `proxima_core.interpretation_subject_kind`, so the column
/// cannot hold a value this type is unable to represent. It is
/// deliberately not [`EntityKind`], which carries `Goal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, sqlx::Type)]
#[sqlx(type_name = "proxima_core.interpretation_subject_kind")]
pub enum InterpretationSubjectKind {
    Fact,
    Abstraction,
    Perspective,
}

impl From<InterpretationSubjectKind> for EntityKind {
    fn from(value: InterpretationSubjectKind) -> Self {
        match value {
            InterpretationSubjectKind::Fact => Self::Fact,
            InterpretationSubjectKind::Abstraction => Self::Abstraction,
            InterpretationSubjectKind::Perspective => Self::Perspective,
        }
    }
}

impl InterpretationSubjectKind {
    /// `None` for Goal, which is not a memory and cannot be an
    /// interpretation subject on this payload.
    #[must_use]
    pub const fn from_entity_kind(kind: EntityKind) -> Option<Self> {
        match kind {
            EntityKind::Fact => Some(Self::Fact),
            EntityKind::Abstraction => Some(Self::Abstraction),
            EntityKind::Perspective => Some(Self::Perspective),
            EntityKind::Goal => None,
        }
    }
}

impl PerspectivePayload for InterpretationV1 {
    const SCHEMA_ID: &'static str = "core/interpretation-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.interpretation_v1"
    }

    /// The subjects, as schema-declared reference fields. A subject
    /// whose kind is missing from `subject_kinds` is skipped rather than
    /// guessed: an index row with the wrong endpoint kind would be a
    /// wrong answer, and a missing one is a visible absence.
    fn references(&self) -> Vec<PayloadReference> {
        self.subject_memory_ids
            .iter()
            .zip(self.subject_kinds.iter())
            .map(|(memory_id, kind)| {
                PayloadReference::memory(
                    "subject_memory_ids",
                    EntityKind::from(*kind),
                    MemoryId::new(*memory_id),
                )
            })
            .collect()
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("InterpretationV1 schema serializes"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{InterpretationSubjectKind, InterpretationV1};
    use crate::{EntityKind, PerspectivePayload, ReferenceBinding};

    #[test]
    fn subjects_become_pinned_reference_declarations() {
        let subject = uuid::Uuid::now_v7();
        let payload = InterpretationV1 {
            claim: "the outage followed the deploy".into(),
            confidence: 80,
            subject_memory_ids: vec![subject],
            subject_kinds: vec![InterpretationSubjectKind::Fact],
            model_id: "test-model".into(),
            client_name: "test".into(),
            client_version: "1".into(),
        };
        let references = payload.references();
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].binding, ReferenceBinding::Pin);
        assert_eq!(references[0].target.kind, EntityKind::Fact);
        assert_eq!(
            references[0]
                .target
                .memory_id()
                .map(crate::MemoryId::into_inner),
            Some(subject)
        );
        references[0]
            .validate()
            .expect("a pinned memory reference is well formed");
    }

    /// The edge set must be a function of node content: the same payload
    /// re-read yields the same reference declarations, which is what
    /// makes the index rebuildable.
    #[test]
    fn references_are_a_function_of_payload_content() {
        let payload = InterpretationV1 {
            claim: "same".into(),
            confidence: 1,
            subject_memory_ids: vec![uuid::Uuid::now_v7(), uuid::Uuid::now_v7()],
            subject_kinds: vec![
                InterpretationSubjectKind::Abstraction,
                InterpretationSubjectKind::Perspective,
            ],
            model_id: "m".into(),
            client_name: "c".into(),
            client_version: "1".into(),
        };
        assert_eq!(payload.references(), payload.references());
        assert_eq!(payload.references().len(), 2);
    }
}

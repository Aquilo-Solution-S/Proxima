//! Phase-2 consolidation operator surface (docs/04 §"Phase 2 —
//! Personality embedding"). M5 ships F→A only; A→P / A→Goal /
//! Edge land in M6+ as their operators arrive.
//!
//! The trait surface is deliberately minimal: an operator declares
//! its identity (operator_id, output schema, prompt_version), the
//! Fact schemas it consumes, and a `run` closure that takes a
//! pre-loaded batch's Facts and emits zero-or-more typed
//! Abstractions with provenance and embeddings.
//!
//! The dispatcher (Engine::close_batch in M5) is responsible for
//! filtering the batch's Facts via `consumes()`, providing
//! LLM/embed clients, and persisting outputs atomically through
//! `Storage::consolidate_batch_f2a`.

use std::sync::Arc;

use async_trait::async_trait;
use time::OffsetDateTime;

use crate::{
    LlmCaps, MemoryId, ModelTier, Owner, PersonalityId, PersonalityStateHash, RegisteredRelation,
    SchemaId, SchemaVersion, SourceBatchId,
};

/// Personality state captured at operator-invocation start (docs/04
/// §"Personality state"). M5 pins a constant default snapshot since
/// the Personality flavor is post-M7; the schema columns
/// (`personality_id`, `personality_state_hash` on `proxima_core.memories`)
/// are populated with these values.
#[derive(Debug, Clone)]
pub struct PersonalitySnapshot {
    pub personality_id: PersonalityId,
    pub state_hash: PersonalityStateHash,
    pub captured_at: OffsetDateTime,
}

impl PersonalitySnapshot {
    /// Default snapshot used until Personality flavor lands.
    /// `personality_id = "default"`, `state_hash = [0; 32]`.
    #[must_use]
    pub fn default_snapshot() -> Self {
        Self {
            personality_id: PersonalityId::new("default"),
            state_hash: PersonalityStateHash::new([0; 32]),
            captured_at: OffsetDateTime::now_utc(),
        }
    }
}

/// One Fact in a source batch, hydrated for operator consumption.
/// The dispatcher loads `payload_json` by joining the substrate row
/// against the schema's sidecar table; the operator deserialises it
/// into the concrete `FactPayload` type the schema_id resolves to.
#[derive(Debug, Clone)]
pub struct FactRow {
    pub memory_id: MemoryId,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub payload_json: serde_json::Value,
}

/// Output of a single F→A invocation: zero or more typed Abstractions
/// with provenance set and embedding pre-computed by the operator.
/// The engine validates `provenance ⊆ batch_facts` before persistence.
#[derive(Debug, Clone)]
pub struct NewAbstraction {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub text: String,
    pub typed_payload: serde_json::Value,
    pub provenance: Vec<MemoryId>,
    pub embedding: Vec<f32>,
    pub embedding_model_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum OperatorError {
    #[error("LLM call failed: {0}")]
    Llm(String),
    #[error("embedding call failed: {0}")]
    Embed(String),
    #[error("output validation failed: {0}")]
    OutputValidation(String),
    #[error("internal: {0}")]
    Internal(String),
}

/// Minimal LLM client surface. Concrete impl in `proxima-llm`.
/// The contract is JSON-mode: the caller supplies system + user
/// prompts; the model is expected to return a single JSON object
/// the operator can deserialise into a known shape.
#[async_trait]
pub trait LlmClient: Send + Sync + std::fmt::Debug {
    async fn complete_json(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<serde_json::Value, OperatorError>;

    fn model_id(&self) -> &str;
}

/// Embedding client surface. Concrete impl in `proxima-embed`.
#[async_trait]
pub trait EmbeddingClient: Send + Sync + std::fmt::Debug {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, OperatorError>;

    fn model_id(&self) -> &str;
    fn dim(&self) -> usize;
}

/// Context passed to F→A operators. Borrowed for the duration of
/// `run()`; the dispatcher owns the lifetimes.
#[derive(Debug)]
pub struct F2AContext<'a> {
    pub batch_id: SourceBatchId,
    pub owner: Owner,
    pub facts: &'a [FactRow],
    pub personality: &'a PersonalitySnapshot,
    pub llm: &'a dyn LlmClient,
    pub embed: &'a dyn EmbeddingClient,
}

/// F→A operator trait (docs/04 §"F→A — Fact to Abstraction").
#[async_trait]
pub trait F2AOperator: Send + Sync + std::fmt::Debug {
    /// Stable identifier — used as the dedup key in
    /// `proxima_core.source_batch_f2a` and as the `operator_kind`
    /// column on resulting Abstractions. v1 convention:
    /// `"<flavor>/<short-name>"`, e.g. `"proxima-code/commit-summary"`.
    fn operator_id(&self) -> &'static str;

    fn output_schema_id(&self) -> &'static str;
    fn output_schema_version(&self) -> u32;

    /// Versioned prompt; bumping is part of the F→A invocation key
    /// (docs/04 §"Idempotence and reproducibility"). A new
    /// `prompt_version` over the same closed batch produces a new
    /// Abstraction superseding the prior.
    fn prompt_version(&self) -> &'static str;

    /// Filter predicate. The dispatcher calls this against each
    /// batch fact; only matching ones reach `run()`.
    fn consumes(&self, schema_id: &SchemaId) -> bool;

    /// Run the operator on the prepared context. May return an
    /// empty vec — not every batch yields a defensible Abstraction
    /// (docs/04: "the operator does not force it").
    async fn run(&self, ctx: F2AContext<'_>) -> Result<Vec<NewAbstraction>, OperatorError>;

    /// Routing class for the LLM backing this operator. Default
    /// `Standard`. Operators that legitimately need a deeper model
    /// (e.g. multi-step plan synthesis) override to `Deep`; small
    /// classification-style ops may override to `Fast`.
    ///
    /// Runtime config binds each tier to a `(vendor, model_id)`; the
    /// dispatcher resolves `op.tier()` to the bound model when wiring
    /// the `LlmClient` into `F2AContext`.
    fn tier(&self) -> ModelTier {
        ModelTier::Standard
    }

    /// LLM capabilities this operator demands from whatever model the
    /// runtime binds to its `tier()`. Default `LlmCaps::none()` — no
    /// caps required, any registered model satisfies. Override when the
    /// operator's prompt strategy actually needs `tool_use`,
    /// `json_mode`, `long_context`, or `vision`.
    ///
    /// Validated at credential-write time: a model's claimed caps must
    /// satisfy the union of `requires()` over operators using that
    /// tier (see `Engine::tier_requires_union`).
    fn requires(&self) -> LlmCaps {
        LlmCaps::none()
    }
}

/// Operator registry. M5 ships F→A only; A→P / A→Goal / Edge slots
/// land as the operators do. Cloned by the engine; `Arc<dyn _>`
/// keeps registration cheap.
#[derive(Debug, Default, Clone)]
pub struct OperatorRegistry {
    f2a: Vec<Arc<dyn F2AOperator>>,
}

impl OperatorRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_f2a<O: F2AOperator + 'static>(&mut self, op: O) {
        self.f2a.push(Arc::new(op));
    }

    #[must_use]
    pub fn f2a_operators(&self) -> &[Arc<dyn F2AOperator>] {
        &self.f2a
    }
}

/// Storage-side request for `Storage::consolidate_batch_f2a`. Carries
/// the operator's invocation metadata, the validated abstractions to
/// persist, and the sidecar-table identifier the engine resolved from
/// the abstraction's `schema_id` via the registry.
#[derive(Debug)]
pub struct ConsolidateBatchF2ARequest<'a> {
    pub batch_id: SourceBatchId,
    pub owner: Owner,
    pub operator_id: &'a str,
    pub provenance_relation: RegisteredRelation<'a>,
    pub model_id: &'a str,
    pub prompt_version: &'a str,
    pub personality: &'a PersonalitySnapshot,
    pub abstractions: &'a [NewAbstraction],
    /// Qualified sidecar-table identifier the abstractions write into
    /// (e.g. `"proxima_code.commit_summary_v1"`). Resolved by the engine
    /// from the abstraction's schema. Single value because each F→A
    /// operator emits one output schema.
    pub output_sidecar_table: &'a str,
}

/// Idempotency key for one F→A invocation over a source batch. The
/// batch id is supplied separately by storage queries; this struct is
/// the operator/runtime half of the key.
#[derive(Debug, Clone, Copy)]
pub struct F2AInvocationKey<'a> {
    pub operator_id: &'a str,
    pub prompt_version: &'a str,
    pub model_id: &'a str,
    pub personality_id: &'a str,
    pub personality_state_hash: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsolidateBatchF2AOutcome {
    pub abstraction_ids: Vec<MemoryId>,
    /// True iff the full F→A invocation key was already in
    /// `source_batch_f2a` — no work was done. The caller treats this
    /// as success.
    pub already_consolidated: bool,
}

/// Sidecar resolution hint for `Storage::load_batch_facts`. The
/// engine builds this from its `SchemaRegistry` (Fact schemas with a
/// declared `sidecar_table`) and hands it to storage so the verb can
/// emit per-schema `row_to_json(s.*)` joins.
#[derive(Debug, Clone)]
pub struct SidecarSpec {
    pub schema_id: SchemaId,
    pub sidecar_table: String,
}

#[cfg(test)]
mod tier_requires_tests {
    use super::*;
    use async_trait::async_trait;

    #[test]
    fn defaults_are_standard_and_none() {
        // Bare operator with neither tier() nor requires() overridden.
        #[derive(Debug)]
        struct Bare;
        #[async_trait]
        impl F2AOperator for Bare {
            fn operator_id(&self) -> &'static str {
                "test/bare"
            }
            fn output_schema_id(&self) -> &'static str {
                "test/out"
            }
            fn output_schema_version(&self) -> u32 {
                1
            }
            fn prompt_version(&self) -> &'static str {
                "v1"
            }
            fn consumes(&self, _: &SchemaId) -> bool {
                true
            }
            async fn run(&self, _: F2AContext<'_>) -> Result<Vec<NewAbstraction>, OperatorError> {
                Ok(Vec::new())
            }
        }
        let bare = Bare;
        assert_eq!(bare.tier(), ModelTier::Standard);
        assert_eq!(bare.requires(), LlmCaps::none());
    }
}

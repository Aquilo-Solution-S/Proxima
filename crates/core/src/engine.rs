//! Engine composite — wires SchemaRegistry, MemoryStore, and
//! an AuthResolver behind the typed verb surfaces of
//! docs/14-protocol-surface.md.

use std::collections::HashSet;
use std::sync::Arc;

use crate::GoalId;
use crate::Owner;
use crate::SourceBatchId;
use crate::auth::{AuthResolver, Credentials};
use crate::error::ProtocolError;
use crate::operators::{
    ConsolidateBatchF2ARequest, EmbeddingClient, F2AContext, FactRow, LlmClient, OperatorError,
    OperatorRegistry, PersonalitySnapshot, SidecarSpec,
};
use crate::storage::{NoopStorage, StorageError, StorageHandle};
use crate::verbs::close_batch::CloseBatchOutcome;
use crate::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use crate::verbs::goal_write::{GoalDraft, GoalWriteOutcome};
use crate::verbs::query::{MemoryStore, QueryRequest, QueryResponse, SupersessionStatus};
use crate::verbs::schema::{PayloadKind, SchemaRegistry, SchemaRequest, SchemaResponse};
use crate::verbs::subscribe::{ChangeEventStream, SubscribeRequest};
use crate::{CORE_DERIVED_FROM_RELATION, LlmCaps, MemoryId, ModelTier, SchemaId, SchemaVersion};

pub struct Engine {
    registry: SchemaRegistry,
    // TODO(M3.B): remove MemoryStore
    memories: MemoryStore,
    auth: Box<dyn AuthResolver>,
    storage: StorageHandle,
    operators: OperatorRegistry,
    llm: Option<Arc<dyn LlmClient>>,
    embed: Option<Arc<dyn EmbeddingClient>>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("registry", &self.registry)
            .field("memories", &self.memories)
            .field("auth", &"<dyn AuthResolver>")
            .field("storage", &"<dyn Storage>")
            .finish()
    }
}

impl Engine {
    pub fn new(
        registry: SchemaRegistry,
        memories: MemoryStore,
        auth: Box<dyn AuthResolver>,
    ) -> Self {
        Self {
            registry,
            memories,
            auth,
            storage: Arc::new(NoopStorage),
            operators: OperatorRegistry::new(),
            llm: None,
            embed: None,
        }
    }

    /// Get a reference to the schema registry.
    #[must_use]
    pub fn registry(&self) -> &SchemaRegistry {
        &self.registry
    }

    #[must_use]
    pub fn with_storage(mut self, storage: StorageHandle) -> Self {
        self.storage = storage;
        self
    }

    /// Register operators (M5: F→A only). Bare-Engine without
    /// operators behaves identically to M4 — `close_batch` flips
    /// `closed_at` and returns. With operators registered AND an
    /// LLM + embed client wired in, `close_batch` also runs F→A
    /// consolidation inline (docs/04 §"F→A").
    #[must_use]
    pub fn with_operators(mut self, registry: OperatorRegistry) -> Self {
        self.operators = registry;
        self
    }

    /// Union of `requires()` over all F→A operators bound to `tier`.
    /// Returns `LlmCaps::none()` if no operator uses that tier — the
    /// caller (runtime credential-write validation) treats that as
    /// "any model satisfies".
    ///
    /// Walks `self.operators.f2a_operators()`. As A→P / A→Goal /
    /// Edge operator slots land, this method extends to walk those
    /// too — for now, F→A is the only populated slot.
    #[must_use]
    pub fn tier_requires_union(&self, tier: ModelTier) -> LlmCaps {
        let mut acc = LlmCaps::none();
        for op in self.operators.f2a_operators() {
            if op.tier() == tier {
                let r = op.requires();
                acc.tool_use |= r.tool_use;
                acc.json_mode |= r.json_mode;
                acc.long_context |= r.long_context;
                acc.vision |= r.vision;
            }
        }
        acc
    }

    #[must_use]
    pub fn with_llm(mut self, llm: Arc<dyn LlmClient>) -> Self {
        self.llm = Some(llm);
        self
    }

    #[must_use]
    pub fn with_embed(mut self, embed: Arc<dyn EmbeddingClient>) -> Self {
        self.embed = Some(embed);
        self
    }

    /// docs/14 §"Schema" — binary-scoped, unauthenticated by
    /// default. Owner is not consulted.
    pub fn schema(&self, req: &SchemaRequest) -> SchemaResponse {
        self.registry.handle(req)
    }

    /// docs/14 §"Query" — Owner-scoped. Caller passes the
    /// transport-extracted credentials; engine resolves and
    /// gates `req.owner.principal ∈ resolved.accessible_principals`.
    ///
    /// For heads-only requests targeting a stateful Fact schema (one
    /// whose `FactPayload::natural_key_columns()` is non-empty), the
    /// engine populates `QueryRequest::stateful_heads` from the
    /// registry before dispatch. Storage emits the per-NK head SQL
    /// when the field is `Some`; otherwise the existing
    /// `supersedes`-based head scan applies (A/P).
    pub async fn query(
        &self,
        creds: &Credentials,
        req: &QueryRequest,
    ) -> Result<QueryResponse, ProtocolError> {
        let resolved = self
            .auth
            .resolve(creds)
            .map_err(|_| ProtocolError::auth_required())?;
        if !resolved.can_access_owner(&req.owner) {
            return Err(ProtocolError::forbidden(
                "principal cannot access requested owner",
            ));
        }
        let mut effective = req.clone();
        if matches!(effective.supersession, SupersessionStatus::HeadsOnly)
            && effective.stateful_heads.is_none()
            && let Some(sid) = effective.schema_id.as_ref()
        {
            effective.stateful_heads = self.registry.stateful_filter_for(sid);
        }
        self.storage
            .query_memories(&effective, self.registry.list().as_slice())
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }

    /// docs/14 §"EventIngest" — Owner-scoped write. Validates
    /// schemas and delegates to storage.
    pub async fn event_ingest(
        &self,
        creds: &Credentials,
        draft: EventDraft,
    ) -> Result<EventIngestOutcome, ProtocolError> {
        let resolved = self
            .auth
            .resolve(creds)
            .map_err(|_| ProtocolError::auth_required())?;
        if !resolved.can_access_owner(&draft.owner) {
            return Err(ProtocolError::forbidden(
                "principal cannot access requested owner",
            ));
        }
        // Three schema validations: fact, cited_object, citation_mapping.
        for (sid, ver) in [
            (&draft.schema_id, draft.schema_version),
            (
                &draft.cited_object.schema_id,
                draft.cited_object.schema_version,
            ),
            (
                &draft.citation_mapping.schema_id,
                draft.citation_mapping.schema_version,
            ),
        ] {
            if self.registry.lookup(sid, ver).is_none() {
                return Err(ProtocolError::unknown_schema(
                    sid.as_str(),
                    ver.into_inner(),
                ));
            }
        }
        self.storage
            .ingest_event_atomic(&draft)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }

    /// docs/14 §"GoalWrite" — Owner-scoped write. Validates
    /// schema is registered as PayloadKind::Goal and delegates to
    /// storage.
    pub async fn write_goal(
        &self,
        creds: &Credentials,
        draft: GoalDraft,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        let resolved = self
            .auth
            .resolve(creds)
            .map_err(|_| ProtocolError::auth_required())?;
        if !resolved.can_access_owner(&draft.owner) {
            return Err(ProtocolError::forbidden(
                "principal cannot access requested owner",
            ));
        }
        // Validate goal schema is registered AND has PayloadKind::Goal.
        match self.registry.lookup(&draft.schema_id, draft.schema_version) {
            Some(info) if info.kind == PayloadKind::Goal => {}
            _ => {
                return Err(ProtocolError::unknown_schema(
                    draft.schema_id.as_str(),
                    draft.schema_version.into_inner(),
                ));
            }
        }
        self.storage
            .write_goal_atomic(&draft)
            .await
            .map_err(map_storage_err_for_goal_write(&draft.request_id))
    }

    /// docs/14 §"GoalWrite" — supersede path. Same auth and schema
    /// validation as write_goal, plus validates prior exists and
    /// belongs to the same owner.
    pub async fn supersede_goal(
        &self,
        creds: &Credentials,
        prior: GoalId,
        draft: GoalDraft,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        let resolved = self
            .auth
            .resolve(creds)
            .map_err(|_| ProtocolError::auth_required())?;
        if !resolved.can_access_owner(&draft.owner) {
            return Err(ProtocolError::forbidden(
                "principal cannot access requested owner",
            ));
        }
        // Validate goal schema is registered AND has PayloadKind::Goal.
        match self.registry.lookup(&draft.schema_id, draft.schema_version) {
            Some(info) if info.kind == PayloadKind::Goal => {}
            _ => {
                return Err(ProtocolError::unknown_schema(
                    draft.schema_id.as_str(),
                    draft.schema_version.into_inner(),
                ));
            }
        }
        self.storage
            .supersede_goal_atomic(prior, &draft)
            .await
            .map_err(map_storage_err_for_goal_write(&draft.request_id))
    }

    /// docs/01 §"The contract" — Owner-scoped, idempotent batch close.
    /// Sources call this after a successful poll once they consider the
    /// batch complete. F→A consolidation (M5+) gates on
    /// `closed_at IS NOT NULL`.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` when the batch doesn't exist or belongs to a
    /// different owner; `Forbidden` when the principal cannot access
    /// `owner`; `AuthRequired` on resolver failure.
    pub async fn close_batch(
        &self,
        creds: &Credentials,
        owner: crate::Owner,
        source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, ProtocolError> {
        let resolved = self
            .auth
            .resolve(creds)
            .map_err(|_| ProtocolError::auth_required())?;
        if !resolved.can_access_owner(&owner) {
            return Err(ProtocolError::forbidden(
                "principal cannot access requested owner",
            ));
        }
        let outcome = self
            .storage
            .close_batch(&owner, source_batch_id)
            .await
            .map_err(|e| match e {
                StorageError::NotFound => ProtocolError::not_found("source batch not found"),
                other => ProtocolError::internal(other.to_string()),
            })?;

        // Run registered F→A operators against the just-closed batch.
        // Skipped for already-closed batches (idempotent re-close) —
        // dedup at the storage layer would short-circuit the persist
        // step anyway, but skipping here avoids redundant LLM calls.
        if !outcome.already_closed
            && !self.operators.f2a_operators().is_empty()
            && self.llm.is_some()
            && self.embed.is_some()
        {
            self.run_f2a_for_batch(&owner, source_batch_id).await?;
        }
        Ok(outcome)
    }

    /// Run F→A operators against any closed-but-unconsolidated batches
    /// for `owner`. Returns the set of `(operator_id, batch_id)` pairs
    /// that produced at least one Abstraction this call.
    ///
    /// This is the catch-up entrypoint: M4-era `LocalGitSource` closes
    /// batches at the storage layer (bypassing
    /// `Engine::close_batch`), so a binary that wants F→A consolidation
    /// invokes this after each poll. Idempotent on the
    /// `source_batch_f2a` dedup row — already-consolidated batches are
    /// invisible to this query.
    ///
    /// No-op if no F→A operators are registered or LLM/embed clients
    /// are missing.
    pub async fn run_pending_f2a(
        &self,
        owner: &Owner,
    ) -> Result<Vec<(&'static str, SourceBatchId)>, ProtocolError> {
        let mut consolidated = Vec::new();
        if self.operators.f2a_operators().is_empty() || self.llm.is_none() || self.embed.is_none() {
            return Ok(consolidated);
        }

        // Snapshot the per-operator pending list — we don't mutate
        // mid-iteration.
        let mut pending: Vec<(&'static str, Vec<SourceBatchId>)> =
            Vec::with_capacity(self.operators.f2a_operators().len());
        for op in self.operators.f2a_operators() {
            let batches = self
                .storage
                .list_unconsolidated_batches(owner, op.operator_id())
                .await
                .map_err(|e| {
                    ProtocolError::internal(format!("list_unconsolidated_batches: {e}"))
                })?;
            pending.push((op.operator_id(), batches));
        }

        // Walk operator-by-operator, batch-by-batch.
        for op in self.operators.f2a_operators() {
            let Some((_, batches)) = pending.iter().find(|(id, _)| *id == op.operator_id()) else {
                continue;
            };
            for &batch_id in batches {
                if self
                    .run_f2a_op_on_batch(owner, op.as_ref(), batch_id)
                    .await?
                {
                    consolidated.push((op.operator_id(), batch_id));
                }
            }
        }
        Ok(consolidated)
    }

    /// Run every registered F→A operator against the just-closed
    /// batch. Synchronous in M5 — bounded queues + worker pools land
    /// in M6 (per Beyond v1 in ROADMAP).
    async fn run_f2a_for_batch(
        &self,
        owner: &Owner,
        batch_id: SourceBatchId,
    ) -> Result<(), ProtocolError> {
        for op in self.operators.f2a_operators() {
            self.run_f2a_op_on_batch(owner, op.as_ref(), batch_id)
                .await?;
        }
        Ok(())
    }

    /// Single-operator, single-batch dispatch. Returns whether an
    /// Abstraction was actually persisted (false on empty input or
    /// empty operator output — neither writes to `source_batch_f2a`).
    async fn run_f2a_op_on_batch(
        &self,
        owner: &Owner,
        op: &dyn crate::operators::F2AOperator,
        batch_id: SourceBatchId,
    ) -> Result<bool, ProtocolError> {
        let llm = self.llm.as_ref().expect("guarded by caller");
        let embed = self.embed.as_ref().expect("guarded by caller");

        // Fact schemas registered with a sidecar — the input universe.
        let sidecars: Vec<SidecarSpec> = self
            .registry
            .list()
            .iter()
            .filter(|s| s.kind == PayloadKind::Fact && s.sidecar_table.is_some())
            .map(|s| SidecarSpec {
                schema_id: s.schema_id.clone(),
                sidecar_table: s.sidecar_table.clone().unwrap(),
            })
            .collect();

        let facts = self
            .storage
            .load_batch_facts(owner, batch_id, &sidecars)
            .await
            .map_err(|e| ProtocolError::internal(format!("load_batch_facts: {e}")))?;

        let batch_memory_ids: HashSet<MemoryId> = facts.iter().map(|f| f.memory_id).collect();
        let personality = PersonalitySnapshot::default_snapshot();

        let filtered: Vec<FactRow> = facts
            .iter()
            .filter(|f| op.consumes(&f.schema_id))
            .cloned()
            .collect();

        if filtered.is_empty() {
            tracing::debug!(
                operator = op.operator_id(),
                batch_id = %batch_id.into_inner(),
                "F→A skipped: no matching facts"
            );
            return Ok(false);
        }

        let ctx = F2AContext {
            batch_id,
            owner: owner.clone(),
            facts: &filtered,
            personality: &personality,
            llm: llm.as_ref(),
            embed: embed.as_ref(),
        };

        let abstractions = op.run(ctx).await.map_err(map_operator_err)?;

        if abstractions.is_empty() {
            tracing::info!(
                operator = op.operator_id(),
                batch_id = %batch_id.into_inner(),
                "F→A returned no Abstractions; not recording a run"
            );
            return Ok(false);
        }

        let output_schema_id = SchemaId::new(op.output_schema_id().to_string());
        let output_schema_version = SchemaVersion::new(op.output_schema_version());
        let output_info = self
            .registry
            .lookup(&output_schema_id, output_schema_version)
            .filter(|s| s.kind == PayloadKind::Abstraction)
            .ok_or_else(|| {
                ProtocolError::internal(format!(
                    "operator {} output schema {} v{} is not a registered Abstraction",
                    op.operator_id(),
                    op.output_schema_id(),
                    op.output_schema_version()
                ))
            })?;
        let output_sidecar = output_info.sidecar_table.clone().ok_or_else(|| {
            ProtocolError::internal(format!(
                "operator {} output schema {} v{} has no registered Abstraction sidecar",
                op.operator_id(),
                op.output_schema_id(),
                op.output_schema_version()
            ))
        })?;

        for abs in &abstractions {
            if abs.schema_id != output_schema_id || abs.schema_version != output_schema_version {
                return Err(ProtocolError::internal(format!(
                    "operator {} returned schema {} v{} but declares {} v{}",
                    op.operator_id(),
                    abs.schema_id.as_str(),
                    abs.schema_version.into_inner(),
                    op.output_schema_id(),
                    op.output_schema_version()
                )));
            }

            self.registry
                .validate_payload(
                    &abs.schema_id,
                    abs.schema_version,
                    PayloadKind::Abstraction,
                    &abs.typed_payload,
                )
                .map_err(|e| {
                    ProtocolError::internal(format!(
                        "operator {} returned invalid {} v{} payload: {e}",
                        op.operator_id(),
                        abs.schema_id.as_str(),
                        abs.schema_version.into_inner()
                    ))
                })?;

            for prov in &abs.provenance {
                if !batch_memory_ids.contains(prov) {
                    return Err(ProtocolError::internal(format!(
                        "operator {} returned provenance {:?} not in batch",
                        op.operator_id(),
                        prov
                    )));
                }
            }
        }

        let provenance_relation = self
            .registry
            .resolve_relation(CORE_DERIVED_FROM_RELATION)
            .ok_or_else(|| {
                ProtocolError::internal(format!(
                    "missing registered relation {CORE_DERIVED_FROM_RELATION}"
                ))
            })?;

        let req = ConsolidateBatchF2ARequest {
            batch_id,
            owner: owner.clone(),
            operator_id: op.operator_id(),
            provenance_relation,
            model_id: llm.model_id(),
            prompt_version: op.prompt_version(),
            personality: &personality,
            abstractions: &abstractions,
            output_sidecar_table: &output_sidecar,
        };

        self.storage
            .consolidate_batch_f2a(&req)
            .await
            .map_err(|e| ProtocolError::internal(format!("consolidate_batch_f2a: {e}")))?;
        Ok(true)
    }

    /// docs/14 §"Subscribe" — Owner-scoped stream with optional
    /// `since` cursor for resume.
    pub async fn subscribe(
        &self,
        creds: &Credentials,
        req: SubscribeRequest,
    ) -> Result<ChangeEventStream, ProtocolError> {
        let resolved = self
            .auth
            .resolve(creds)
            .map_err(|_| ProtocolError::auth_required())?;
        if !resolved.can_access_owner(&req.owner) {
            return Err(ProtocolError::forbidden(
                "principal cannot access requested owner",
            ));
        }
        self.storage
            .subscribe_changes(&req.owner, req.since)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }
}

fn map_storage_err_for_goal_write(
    request_id: &str,
) -> impl FnOnce(StorageError) -> ProtocolError + '_ {
    move |e| match e {
        StorageError::ConstraintViolation(msg) if msg.starts_with("idempotency_conflict:") => {
            ProtocolError::idempotency_conflict(request_id)
        }
        StorageError::NotFound => ProtocolError::not_found("prior goal not found"),
        other => ProtocolError::internal(other.to_string()),
    }
}

fn map_operator_err(e: OperatorError) -> ProtocolError {
    ProtocolError::internal(format!("operator: {e}"))
}

#[cfg(test)]
mod tier_union_tests {
    use super::*;
    use crate::auth::NoAuth;
    use crate::ids::{OrgId, UserId};
    use crate::operators::{
        F2AContext, F2AOperator, NewAbstraction, OperatorError, OperatorRegistry,
    };
    use crate::verbs::query::MemoryStore;
    use crate::verbs::schema::SchemaRegistry;
    use crate::{Owner, Principal, SchemaId};
    use async_trait::async_trait;
    use uuid::Uuid;

    #[derive(Debug)]
    struct OpAt {
        tier: ModelTier,
        requires: LlmCaps,
    }
    #[async_trait]
    impl F2AOperator for OpAt {
        fn operator_id(&self) -> &'static str {
            "test/op"
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
        fn tier(&self) -> ModelTier {
            self.tier
        }
        fn requires(&self) -> LlmCaps {
            self.requires
        }
    }

    fn engine_with_ops(ops: Vec<OpAt>) -> Engine {
        let mut reg = OperatorRegistry::new();
        for op in ops {
            reg.register_f2a(op);
        }
        let principal = Principal::User(UserId::new(Uuid::now_v7()));
        let owner = Owner {
            principal: principal.clone(),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        Engine::new(
            SchemaRegistry::new(),
            MemoryStore::new(),
            Box::new(NoAuth::new(principal, owner)),
        )
        .with_operators(reg)
    }

    #[test]
    fn union_empty_when_no_ops_at_tier() {
        let eng = engine_with_ops(vec![OpAt {
            tier: ModelTier::Fast,
            requires: LlmCaps {
                tool_use: true,
                ..LlmCaps::none()
            },
        }]);
        assert_eq!(
            eng.tier_requires_union(ModelTier::Standard),
            LlmCaps::none()
        );
    }

    #[test]
    fn union_combines_caps_across_ops_at_same_tier() {
        let eng = engine_with_ops(vec![
            OpAt {
                tier: ModelTier::Standard,
                requires: LlmCaps {
                    tool_use: true,
                    ..LlmCaps::none()
                },
            },
            OpAt {
                tier: ModelTier::Standard,
                requires: LlmCaps {
                    json_mode: true,
                    ..LlmCaps::none()
                },
            },
            OpAt {
                tier: ModelTier::Deep,
                requires: LlmCaps {
                    vision: true,
                    ..LlmCaps::none()
                },
            },
        ]);
        let standard = eng.tier_requires_union(ModelTier::Standard);
        assert!(standard.tool_use);
        assert!(standard.json_mode);
        assert!(!standard.vision); // vision was on Deep, not Standard
    }
}

use std::collections::HashSet;

use super::{Engine, map_operator_err};
use crate::error::ProtocolError;
use crate::operators::{
    A2PContext, A2PInvocationKey, A2PLineageKey, AbstractionRow, ConsolidateA2PRequest,
    ConsolidateBatchF2ARequest, F2AContext, F2AInvocationKey, FactRow, PersonalitySnapshot,
    SidecarSpec, a2p_input_hash,
};
use crate::personality::PersonalityContext;
use crate::verbs::schema::PayloadKind;
use crate::{
    CORE_DERIVED_FROM_RELATION, CORE_SUPERSEDES_RELATION, MemoryId, Owner, SchemaId, SchemaVersion,
    SourceBatchId, canonical_json,
};

impl Engine {
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
        if self.operators.f2a_operators().is_empty() || self.embed.is_none() {
            return Ok(consolidated);
        }
        let personality = PersonalitySnapshot::default_snapshot();
        let personality_state_hash = personality.state_hash.into_inner();

        // Snapshot the per-operator pending list — we don't mutate
        // mid-iteration.
        let mut pending: Vec<(&'static str, Vec<SourceBatchId>)> =
            Vec::with_capacity(self.operators.f2a_operators().len());
        for op in self.operators.f2a_operators() {
            let Some(llm) = self.llm_for_tier(op.tier()) else {
                tracing::warn!(
                    operator = op.operator_id(),
                    tier = ?op.tier(),
                    "F→A skipped: no LLM client wired for operator tier"
                );
                continue;
            };
            let key = F2AInvocationKey {
                operator_id: op.operator_id(),
                prompt_version: op.prompt_version(),
                model_id: llm.model_id(),
                personality_id: personality.personality_id.as_str(),
                personality_state_hash: &personality_state_hash,
            };
            let batches = self
                .storage
                .list_unconsolidated_batches(owner, &key)
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

    /// Run A→P operators over current Abstraction heads for `owner`.
    /// Fan-out is per registered personality. Fixed invocation keys
    /// short-circuit through `Storage::has_a2p_invocation`.
    pub async fn run_pending_a2p(
        &self,
        owner: &Owner,
    ) -> Result<Vec<(&'static str, Vec<MemoryId>)>, ProtocolError> {
        let mut consolidated = Vec::new();
        let personalities = self.registry.list_personalities();
        if self.operators.a2p_operators().is_empty()
            || personalities.is_empty()
            || self.embed.is_none()
        {
            return Ok(consolidated);
        }

        for personality in personalities {
            let snapshot = personality
                .snapshot(&PersonalityContext { owner })
                .await
                .map_err(|e| {
                    ProtocolError::internal(format!(
                        "personality {} snapshot: {}",
                        personality.personality_id(),
                        e.message
                    ))
                })?;
            for op in self.operators.a2p_operators() {
                let ids = self.run_a2p_op(owner, op.as_ref(), &snapshot).await?;
                if !ids.is_empty() {
                    consolidated.push((op.operator_id(), ids));
                }
            }
        }
        Ok(consolidated)
    }

    /// Run every registered F→A operator against the just-closed
    /// batch. Synchronous in M5 — bounded queues + worker pools land
    /// in M6 (per Beyond v1 in ROADMAP).
    pub(crate) async fn run_f2a_for_batch(
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
        let Some(llm) = self.llm_for_tier(op.tier()) else {
            tracing::warn!(
                operator = op.operator_id(),
                tier = ?op.tier(),
                "F→A skipped: no LLM client wired for operator tier"
            );
            return Ok(false);
        };
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

    async fn run_a2p_op(
        &self,
        owner: &Owner,
        op: &dyn crate::operators::A2POperator,
        personality: &PersonalitySnapshot,
    ) -> Result<Vec<MemoryId>, ProtocolError> {
        let Some(llm) = self.llm_for_tier(op.tier()) else {
            tracing::warn!(
                operator = op.operator_id(),
                tier = ?op.tier(),
                "A→P skipped: no LLM client wired for operator tier"
            );
            return Ok(Vec::new());
        };
        let embed = self.embed.as_ref().expect("guarded by caller");

        let sidecars: Vec<SidecarSpec> = self
            .registry
            .list()
            .iter()
            .filter(|s| {
                s.kind == PayloadKind::Abstraction
                    && s.sidecar_table.is_some()
                    && op.consumes(&s.schema_id)
            })
            .map(|s| SidecarSpec {
                schema_id: s.schema_id.clone(),
                sidecar_table: s.sidecar_table.clone().unwrap(),
            })
            .collect();

        if sidecars.is_empty() {
            return Ok(Vec::new());
        }

        let abstractions = self
            .storage
            .load_a2p_abstractions(owner, &sidecars, op.input_limit())
            .await
            .map_err(|e| ProtocolError::internal(format!("load_a2p_abstractions: {e}")))?;

        let filtered: Vec<AbstractionRow> = abstractions
            .into_iter()
            .filter(|a| op.consumes(&a.schema_id))
            .take(op.input_limit())
            .collect();

        if filtered.is_empty() {
            tracing::debug!(
                operator = op.operator_id(),
                "A→P skipped: no matching abstractions"
            );
            return Ok(Vec::new());
        }

        let context = op.context();
        let context_json = canonical_json(&context)
            .map_err(|e| ProtocolError::internal(format!("canonical_json(A2PContextSpec): {e}")))?;
        let context_hash = *blake3::hash(context_json.as_bytes()).as_bytes();
        let input_ids: Vec<MemoryId> = filtered.iter().map(|a| a.memory_id).collect();
        let input_hash = a2p_input_hash(&input_ids);
        let personality_state_hash = personality.state_hash.into_inner();

        let key = A2PInvocationKey {
            operator_id: op.operator_id(),
            prompt_version: op.prompt_version(),
            model_id: llm.model_id(),
            personality_id: personality.personality_id.as_str(),
            personality_state_hash: &personality_state_hash,
            context_hash: &context_hash,
            input_hash: &input_hash,
        };
        if self
            .storage
            .has_a2p_invocation(owner, &key)
            .await
            .map_err(|e| ProtocolError::internal(format!("has_a2p_invocation: {e}")))?
        {
            return Ok(Vec::new());
        }

        let lineage_key = A2PLineageKey {
            operator_id: op.operator_id(),
            prompt_version: op.prompt_version(),
            model_id: llm.model_id(),
            personality_id: personality.personality_id.as_str(),
            personality_state_hash: &personality_state_hash,
        };
        let prior_head = self
            .storage
            .lookup_prior_a2p_head(owner, &lineage_key)
            .await
            .map_err(|e| ProtocolError::internal(format!("lookup_prior_a2p_head: {e}")))?;

        let ctx = A2PContext {
            owner: owner.clone(),
            context: &context,
            abstractions: &filtered,
            personality,
            llm: llm.as_ref(),
            embed: embed.as_ref(),
        };
        let perspectives = op.run(ctx).await.map_err(map_operator_err)?;

        let output_schema_id = SchemaId::new(op.output_schema_id().to_string());
        let output_schema_version = SchemaVersion::new(op.output_schema_version());
        let output_info = self
            .registry
            .lookup(&output_schema_id, output_schema_version)
            .filter(|s| s.kind == PayloadKind::Perspective)
            .ok_or_else(|| {
                ProtocolError::internal(format!(
                    "operator {} output schema {} v{} is not a registered Perspective",
                    op.operator_id(),
                    op.output_schema_id(),
                    op.output_schema_version()
                ))
            })?;
        let output_sidecar = output_info.sidecar_table.clone().ok_or_else(|| {
            ProtocolError::internal(format!(
                "operator {} output schema {} v{} has no registered Perspective sidecar",
                op.operator_id(),
                op.output_schema_id(),
                op.output_schema_version()
            ))
        })?;

        let input_memory_ids: HashSet<MemoryId> = filtered.iter().map(|a| a.memory_id).collect();
        for perspective in &perspectives {
            if perspective.schema_id != output_schema_id
                || perspective.schema_version != output_schema_version
            {
                return Err(ProtocolError::internal(format!(
                    "operator {} returned schema {} v{} but declares {} v{}",
                    op.operator_id(),
                    perspective.schema_id.as_str(),
                    perspective.schema_version.into_inner(),
                    op.output_schema_id(),
                    op.output_schema_version()
                )));
            }

            self.registry
                .validate_payload(
                    &perspective.schema_id,
                    perspective.schema_version,
                    PayloadKind::Perspective,
                    &perspective.typed_payload,
                )
                .map_err(|e| {
                    ProtocolError::internal(format!(
                        "operator {} returned invalid {} v{} payload: {e}",
                        op.operator_id(),
                        perspective.schema_id.as_str(),
                        perspective.schema_version.into_inner()
                    ))
                })?;

            for prov in &perspective.provenance {
                if !input_memory_ids.contains(prov) {
                    return Err(ProtocolError::internal(format!(
                        "operator {} returned provenance {:?} not in A2P input",
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
        let supersedes_relation = self
            .registry
            .resolve_relation(CORE_SUPERSEDES_RELATION)
            .ok_or_else(|| {
                ProtocolError::internal(format!(
                    "missing registered relation {CORE_SUPERSEDES_RELATION}"
                ))
            })?;

        let req = ConsolidateA2PRequest {
            owner: owner.clone(),
            operator_id: op.operator_id(),
            provenance_relation,
            supersedes_relation,
            model_id: llm.model_id(),
            prompt_version: op.prompt_version(),
            personality,
            context_hash,
            input_hash,
            prior_head,
            perspectives: &perspectives,
            output_sidecar_table: &output_sidecar,
        };
        let outcome = self
            .storage
            .consolidate_a2p(&req)
            .await
            .map_err(|e| ProtocolError::internal(format!("consolidate_a2p: {e}")))?;
        if outcome.already_consolidated {
            Ok(Vec::new())
        } else {
            Ok(outcome.perspective_ids)
        }
    }
}

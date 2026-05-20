//! `core/emit_intervention_decision` — Wake Supervisor-authored intervention decision Fact.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;

use crate::intervention::{
    INTERVENTION_SOURCE_ID, InterventionDecisionKind, InterventionDecisionV1,
    intervention_decision_event_draft,
};
use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::{
    CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION, EdgeAuthorshipKind, EntityKind, MemoryId,
    Owner, OwnerPrincipalKind, Principal, SourceBatchId, SourceId,
};

#[derive(Debug, Default)]
pub struct EmitInterventionDecisionTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmitInterventionDecisionArgs {
    #[schemars(
        description = "`F...` intervention-requested Fact memory handle that woke the Wake Supervisor."
    )]
    pub intervention_request: String,
    #[schemars(
        description = "Decision to emit for the intervention request: continue, stop, redirect, decompose, or accept_terminal."
    )]
    pub decision: InterventionDecisionKind,
    #[schemars(
        description = "Optional extra wake rounds to grant for a continue decision. Omit or null for stop, redirect, decompose, or accept_terminal."
    )]
    #[serde(default)]
    pub grant_rounds: Option<u16>,
    #[schemars(
        description = "Optional `I...` Personality handle to redirect to. Required only for redirect; omit or null otherwise."
    )]
    #[serde(default)]
    pub redirect_personality: Option<String>,
    #[schemars(description = "Evidence-based rationale for the decision, 1 to 20000 chars.")]
    pub rationale: String,
    #[schemars(
        description = "Stable idempotency key for this intervention decision. Reuse only for exact replay."
    )]
    pub idempotency_key: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EmitInterventionDecisionOutput {
    pub intervention_decision: String,
    pub decision: InterventionDecisionKind,
}

#[derive(Debug)]
struct LoadedInterventionRequest {
    memory_id: uuid::Uuid,
    target_intervention_personality_instance_id: uuid::Uuid,
    intervention_extension_rounds: i32,
    intervention_hard_cap_rounds: i32,
}

impl McpTool for EmitInterventionDecisionTool {
    const NAME: &'static str = "core/emit_intervention_decision";
    const DESCRIPTION: &'static str = "Emit a typed InterventionDecision for a InterventionRequested Fact targeted at caller Self.";
    type Args = EmitInterventionDecisionArgs;
    type Output = EmitInterventionDecisionOutput;

    fn call(
        ctx: McpToolCtx,
        args: EmitInterventionDecisionArgs,
    ) -> BoxFuture<'static, Result<EmitInterventionDecisionOutput, McpToolError>> {
        Box::pin(async move {
            let intervention_request = ctx.resolve_fact_memory(&args.intervention_request)?;
            if args.rationale.trim().is_empty() {
                return Err(McpToolError::InvalidInput("rationale is empty".into()));
            }
            if args.idempotency_key.trim().is_empty() {
                return Err(McpToolError::InvalidInput(
                    "idempotency_key is empty".into(),
                ));
            }
            let caller_self = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput("caller_self_perspective required".into())
            })?;
            let loaded = load_intervention_request(&ctx, intervention_request).await?;
            validate_intervention_target(&ctx, caller_self, &loaded).await?;
            validate_decision_shape(&ctx, &args, &loaded).await?;
            let redirect_personality_instance_id = args
                .redirect_personality
                .as_deref()
                .map(|raw| ctx.resolve_personality(raw).map(|id| id.into_inner()))
                .transpose()?;
            let payload = InterventionDecisionV1 {
                intervention_request_memory_id: loaded.memory_id,
                decision: args.decision,
                grant_rounds: args.grant_rounds,
                redirect_personality_instance_id,
                rationale: args.rationale,
                idempotency_key: args.idempotency_key,
                decided_at: OffsetDateTime::now_utc(),
            };
            if let Some(existing) =
                existing_decision(&ctx, loaded.memory_id, &payload.idempotency_key).await?
            {
                return Ok(EmitInterventionDecisionOutput {
                    intervention_decision: ctx.format_fact_memory(existing),
                    decision: payload.decision,
                });
            }
            let mut tx = ctx.pool.begin().await.map_err(map_sql)?;
            let outcome = ingest_intervention_decision_fact(&mut tx, &ctx, &payload).await?;
            insert_intervention_decision_sidecar(&mut tx, outcome.memory_id, &payload).await?;
            append_fact_edge(
                &mut tx,
                &ctx,
                CORE_AUTHORED_RELATION,
                caller_self,
                outcome.memory_id,
                EdgeAuthorshipKind::ExternalAgent,
            )
            .await?;
            append_fact_edge(
                &mut tx,
                &ctx,
                CORE_DERIVED_FROM_RELATION,
                outcome.memory_id,
                intervention_request,
                EdgeAuthorshipKind::ExternalAgent,
            )
            .await?;
            tx.commit().await.map_err(map_sql)?;

            Ok(EmitInterventionDecisionOutput {
                intervention_decision: ctx.format_fact_memory(outcome.memory_id),
                decision: payload.decision,
            })
        })
    }
}

async fn existing_decision(
    ctx: &McpToolCtx,
    intervention_request_memory_id: uuid::Uuid,
    idempotency_key: &str,
) -> Result<Option<MemoryId>, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let row: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT d.memory_id
           FROM proxima_core.intervention_decision_v1 d
           JOIN proxima_core.memories m USING (memory_id)
          WHERE d.intervention_request_memory_id = $1
            AND d.idempotency_key = $2
            AND m.owner_principal_kind = $3
            AND m.owner_principal_id = $4
            AND m.owner_org_id = $5",
    )
    .bind(intervention_request_memory_id)
    .bind(idempotency_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_sql)?;
    Ok(row.map(MemoryId::new))
}

async fn load_intervention_request(
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<LoadedInterventionRequest, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let row: Option<(uuid::Uuid, uuid::Uuid, i32, i32)> = sqlx::query_as(
        "SELECT b.memory_id, b.target_intervention_personality_instance_id,
                b.intervention_extension_rounds, b.intervention_hard_cap_rounds
           FROM proxima_core.intervention_requested_v1 b
           JOIN proxima_core.memories m USING (memory_id)
          WHERE b.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_sql)?;
    let Some((
        memory_id,
        target_intervention_personality_instance_id,
        intervention_extension_rounds,
        intervention_hard_cap_rounds,
    )) = row
    else {
        return Err(McpToolError::InvalidInput(
            "intervention_request is not a InterventionRequested Fact for this owner".into(),
        ));
    };
    Ok(LoadedInterventionRequest {
        memory_id,
        target_intervention_personality_instance_id,
        intervention_extension_rounds,
        intervention_hard_cap_rounds,
    })
}

async fn validate_intervention_target(
    ctx: &McpToolCtx,
    caller_self: MemoryId,
    loaded: &LoadedInterventionRequest,
) -> Result<(), McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let matched: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT personality_instance_id
           FROM proxima_core.personality
          WHERE current_root_perspective_memory_id = $1
            AND personality_instance_id = $2
            AND owner_principal_kind = $3
            AND owner_principal_id = $4
            AND owner_org_id = $5
            AND status = 'active'",
    )
    .bind(caller_self.into_inner())
    .bind(loaded.target_intervention_personality_instance_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_sql)?;
    if matched.is_none() {
        return Err(McpToolError::InvalidInput(
            "caller Self is not the targeted Wake Supervisor".into(),
        ));
    }
    Ok(())
}

async fn validate_decision_shape(
    ctx: &McpToolCtx,
    args: &EmitInterventionDecisionArgs,
    loaded: &LoadedInterventionRequest,
) -> Result<(), McpToolError> {
    match args.decision {
        InterventionDecisionKind::Continue => {
            let Some(grant_rounds) = args.grant_rounds else {
                return Err(McpToolError::InvalidInput(
                    "continue requires grant_rounds".into(),
                ));
            };
            if i32::from(grant_rounds) == 0
                || i32::from(grant_rounds) > loaded.intervention_extension_rounds
            {
                return Err(McpToolError::InvalidInput(
                    "grant_rounds exceeds request extension".into(),
                ));
            }
            let prior: Option<i64> = sqlx::query_scalar(
                "SELECT COALESCE(SUM(d.grant_rounds), 0)
                   FROM proxima_core.intervention_decision_v1 d
                   JOIN proxima_core.memories m USING (memory_id)
                  WHERE d.intervention_request_memory_id = $1
                    AND d.decision = 'continue'
                    AND m.owner_principal_kind = $2
                    AND m.owner_principal_id = $3
                    AND m.owner_org_id = $4",
            )
            .bind(loaded.memory_id)
            .bind(owner_columns(&ctx.owner).0)
            .bind(owner_columns(&ctx.owner).1)
            .bind(owner_columns(&ctx.owner).2)
            .fetch_one(&ctx.pool)
            .await
            .map_err(map_sql)?;
            if prior.unwrap_or(0) + i64::from(grant_rounds)
                > i64::from(loaded.intervention_hard_cap_rounds)
            {
                return Err(McpToolError::InvalidInput(
                    "grant_rounds exceeds request hard cap".into(),
                ));
            }
        }
        InterventionDecisionKind::Redirect => {
            if args.redirect_personality.is_none() {
                return Err(McpToolError::InvalidInput(
                    "redirect requires redirect_personality".into(),
                ));
            }
        }
        _ => {
            if args.grant_rounds.is_some() {
                return Err(McpToolError::InvalidInput(
                    "grant_rounds is only valid for continue".into(),
                ));
            }
        }
    }
    Ok(())
}

async fn ingest_intervention_decision_fact(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    payload: &InterventionDecisionV1,
) -> Result<crate::verbs::event_ingest::EventIngestOutcome, McpToolError> {
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes)
        .map_err(|err| McpToolError::InvalidInput(format!("serialize payload: {err}")))?;
    let draft = intervention_decision_event_draft(
        ctx.owner.clone(),
        &payload_bytes,
        SourceBatchId::new(uuid::Uuid::now_v7()),
        SourceId::new(INTERVENTION_SOURCE_ID),
        payload.decided_at,
    );
    ingest_event_in_tx(tx, &draft).await
}

#[allow(clippy::too_many_lines)]
async fn ingest_event_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    draft: &crate::verbs::event_ingest::EventDraft,
) -> Result<crate::verbs::event_ingest::EventIngestOutcome, McpToolError> {
    let event_id = draft.event_id();
    let event_id_bytes = event_id.into_inner();
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&draft.owner);
    let existing: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT memory_id FROM proxima_core.memories WHERE event_id = $1")
            .bind(&event_id_bytes[..])
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_sql)?;
    if let Some(memory_id) = existing {
        let seq: uuid::Uuid = sqlx::query_scalar(
            "SELECT seq FROM proxima_core.change_event
             WHERE entity_memory_id = $1 ORDER BY seq ASC LIMIT 1",
        )
        .bind(memory_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sql)?;
        return Ok(crate::verbs::event_ingest::EventIngestOutcome {
            event_id,
            memory_id: MemoryId::new(memory_id),
            change_event_seq: seq,
            idempotent_replay: true,
        });
    }

    let memory_id = uuid::Uuid::now_v7();
    let citation_mapping_id = uuid::Uuid::now_v7();
    let cited_object_id = uuid::Uuid::now_v7();
    let change_seq = uuid::Uuid::now_v7();
    let cited_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.cited_objects
            (cited_object_id, schema_id, owner_principal_kind,
             owner_principal_id, owner_org_id, content_hash)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (owner_principal_kind, owner_principal_id,
                      owner_org_id, schema_id, content_hash)
         DO UPDATE SET schema_id = EXCLUDED.schema_id
         RETURNING cited_object_id",
    )
    .bind(cited_object_id)
    .bind(draft.cited_object.schema_id.as_str())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(&draft.cited_object.content_hash[..])
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sql)?;
    sqlx::query(
        "INSERT INTO proxima_core.source_batches
            (id, source_id, owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(draft.source_batch_id.into_inner())
    .bind(draft.source_id.as_str())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    sqlx::query(
        "INSERT INTO proxima_core.events
            (event_id, source_id, source_batch_id,
             owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, observed_at, occurred_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(&event_id_bytes[..])
    .bind(draft.source_id.as_str())
    .bind(draft.source_batch_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(draft.schema_id.as_str())
    .bind(i32::try_from(draft.schema_version.into_inner()).unwrap_or(i32::MAX))
    .bind(draft.observed_at)
    .bind(draft.occurred_at)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id,
             owner_org_id, schema_id, schema_version, event_id, citation_mapping_id,
             personality_instance_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8,
                 '00000000-0000-0000-0000-000000000000'::uuid)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(draft.schema_id.as_str())
    .bind(i32::try_from(draft.schema_version.into_inner()).unwrap_or(i32::MAX))
    .bind(&event_id_bytes[..])
    .bind(citation_mapping_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    sqlx::query(
        "INSERT INTO proxima_core.citation_mappings
            (citation_mapping_id, schema_id, owner_principal_kind,
             owner_principal_id, owner_org_id, memory_id, cited_object_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(citation_mapping_id)
    .bind(draft.citation_mapping.schema_id.as_str())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(memory_id)
    .bind(cited_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id, owner_org_id,
             kind, entity_memory_id, entity_kind, entity_schema_id,
             entity_schema_version)
         VALUES ($1, $2, $3, $4, 'EntityAppend', $5, 'Fact', $6, $7)",
    )
    .bind(change_seq)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(memory_id)
    .bind(draft.schema_id.as_str())
    .bind(i32::try_from(draft.schema_version.into_inner()).unwrap_or(i32::MAX))
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(crate::verbs::event_ingest::EventIngestOutcome {
        event_id,
        memory_id: MemoryId::new(memory_id),
        change_event_seq: change_seq,
        idempotent_replay: false,
    })
}

async fn insert_intervention_decision_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &InterventionDecisionV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_core.intervention_decision_v1
            (memory_id, intervention_request_memory_id, decision, grant_rounds,
             redirect_personality_instance_id, rationale, decided_at, idempotency_key)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
         ON CONFLICT (intervention_request_memory_id, idempotency_key) DO NOTHING",
    )
    .bind(memory_id.into_inner())
    .bind(payload.intervention_request_memory_id)
    .bind(payload.decision)
    .bind(payload.grant_rounds.map(i32::from))
    .bind(payload.redirect_personality_instance_id)
    .bind(&payload.rationale)
    .bind(payload.decided_at)
    .bind(&payload.idempotency_key)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(())
}

async fn append_fact_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    relation_id: &str,
    source: MemoryId,
    target: MemoryId,
    authorship_kind: EdgeAuthorshipKind,
) -> Result<(), McpToolError> {
    let relation = ctx
        .registry
        .resolve_relation(relation_id)
        .ok_or_else(|| McpToolError::Other(format!("relation {relation_id} not registered")))?;
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let source_kind = if relation_id == CORE_AUTHORED_RELATION {
        EntityKind::Perspective
    } else {
        EntityKind::Fact
    };
    relation
        .descriptor
        .validate_edge_shape(source_kind.as_str(), "Fact", authorship_kind.as_str())
        .map_err(McpToolError::LayeringViolation)?;
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_memory_id, target_kind, target_memory_id,
             authorship_kind, authorship_owner_memory_id,
             owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(relation.descriptor.relation.as_str())
    .bind(relation.descriptor.class)
    .bind(source_kind)
    .bind(source.into_inner())
    .bind(EntityKind::Fact)
    .bind(target.into_inner())
    .bind(authorship_kind)
    .bind(ctx.caller_self_perspective.map(MemoryId::into_inner))
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(())
}

fn owner_columns(owner: &Owner) -> (OwnerPrincipalKind, uuid::Uuid, uuid::Uuid) {
    let principal_id = match &owner.principal {
        Principal::User(id) => id.into_inner(),
        Principal::Group(id) => id.into_inner(),
    };
    (
        OwnerPrincipalKind::of(&owner.principal),
        principal_id,
        owner.org_id.into_inner(),
    )
}

fn map_sql(err: sqlx::Error) -> McpToolError {
    McpToolError::Storage(crate::StorageError::Internal(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervention_decision_output_is_record_only() {
        let output = EmitInterventionDecisionOutput {
            intervention_decision: "F1".into(),
            decision: InterventionDecisionKind::Continue,
        };

        let value = serde_json::to_value(output).expect("serialize output");

        assert_eq!(value["intervention_decision"], "F1");
        assert_eq!(value["decision"], "continue");
        assert!(value.get("continuation_applied").is_none());
        assert!(value.get("continuation_note").is_none());
    }
}

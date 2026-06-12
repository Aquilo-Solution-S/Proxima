#![allow(dead_code)]

use std::sync::Arc;

use proxima_core::mcp::McpAuthorContext;
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{
    AuthPath, AuthzContext, Engine, FlavorRegistry, McpToolCtx, MemoryId, ModelTier, OutputMode,
    Owner, OwnerPrincipalKind, Principal, RelationClass, WakeEntryAuthoredBy, WakeEntryDraft,
    WakeEntryExecutionMode, WakeEntryRow, WakeEntryTriggerKind,
};
use uuid::Uuid;

pub fn wake(
    personality_instance_id: proxima_core::PersonalityInstanceId,
    trigger_id: &str,
    label: &str,
    substrate_tool_palette: Vec<String>,
) -> WakeEntryDraft {
    WakeEntryDraft::new(
        Uuid::now_v7(),
        personality_instance_id,
        WakeEntryTriggerKind::OnMemory,
        trigger_id,
        label,
        WakeEntryAuthoredBy::Any,
        1000,
        ModelTier::Standard,
        None,
        substrate_tool_palette,
        4,
    )
    .expect("wake draft")
}

pub fn wake_row(draft: &WakeEntryDraft) -> WakeEntryRow {
    WakeEntryRow {
        wake_entry_id: draft.wake_entry_id,
        trigger_kind: draft.trigger_kind,
        trigger_id: draft.trigger_id.clone(),
        label: draft.label.clone(),
        enabled: draft.enabled,
        execution_mode: WakeEntryExecutionMode::SubstrateOnly,
        authored_by: draft.authored_by,
        probability_promille: draft.probability_promille,
        goal_scope: draft.goal_scope,
        instructions: draft.instructions.clone(),
        model_tier: draft.model_tier,
        inference_target_ref: draft.inference_target_ref.clone(),
        substrate_tool_palette: draft.substrate_tool_palette.clone(),
        required_produced_schema_ids: draft.required_produced_schema_ids.clone(),
        max_rounds: draft.max_rounds,
        intervention_policy: draft.intervention_policy.clone(),
        disabled_reason: None,
    }
}

pub async fn insert_test_abstraction(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    personality_instance_id: proxima_core::PersonalityInstanceId,
    schema_id: &str,
    text: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let owner_kind = OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, $4, $5, 1, 'Abstraction',
                 $6, 'Wake', 'mock-no-llm', 'chat-lifecycle-test',
                 $7, 1)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .bind(schema_id)
    .bind(text)
    .bind(personality_instance_id.into_inner())
    .execute(pg.pool())
    .await?;
    Ok(memory_id)
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_memory_edge(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    source: Uuid,
    target: Uuid,
    relation: &str,
    relation_class: RelationClass,
    source_kind: &str,
    target_kind: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let edge_id = Uuid::now_v7();
    let owner_kind = OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind, authorship_owner_memory_id,
             owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1, $2, $3,
                 $4::proxima_core.entity_kind, $5, NULL,
                 $6::proxima_core.entity_kind, $7, NULL,
                 'Engine', NULL,
                 $8, $9, $10)",
    )
    .bind(edge_id)
    .bind(relation)
    .bind(relation_class)
    .bind(source_kind)
    .bind(source)
    .bind(target_kind)
    .bind(target)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .execute(pg.pool())
    .await?;
    Ok(edge_id)
}

pub fn self_perspective(
    rows: &[proxima_core::PersonalityInstanceRow],
    instance_id: proxima_core::PersonalityInstanceId,
) -> MemoryId {
    rows.iter()
        .find(|row| row.personality_instance_id == instance_id)
        .expect("personality row")
        .current_root_perspective_memory_id
}

pub fn ctx(
    pg: &proxima_storage_pg::PgStorage,
    owner: proxima_core::Owner,
    caller_self_perspective: MemoryId,
) -> McpToolCtx {
    let registry = Arc::new(FlavorRegistry::new().freeze());
    let engine = engine(pg);
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    McpToolCtx {
        pool: pg.pool().clone(),
        owner,
        authz,
        handles: None,
        mode: OutputMode::RawIds,
        registry,
        author: McpAuthorContext {
            model_id: "test/model".into(),
            client_name: "test".into(),
            client_version: "1".into(),
            caller_self_perspective: Some(caller_self_perspective),
        },
        caller_self_perspective: Some(caller_self_perspective),
        master_token_id: None,
        engine: Some(Arc::new(engine)),
    }
}

pub fn engine(pg: &proxima_storage_pg::PgStorage) -> Engine {
    Engine::new(FlavorRegistry::new().freeze(), MemoryStore::new())
        .with_storage(pg.clone().into_handle())
}

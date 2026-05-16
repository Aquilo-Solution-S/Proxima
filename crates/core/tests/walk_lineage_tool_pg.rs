mod common;

use std::sync::Arc;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::auth::NoAuth;
use proxima_core::personality::{PersonalityInstanceId, PersonalityTool, PersonalityToolContext};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{
    Engine, FlavorRegistry, HandleTable, MemoryId, Owner, OwnerPrincipalKind, Principal,
    RelationClass, WakeChainDepth,
};
use uuid::Uuid;

#[tokio::test]
async fn walk_lineage_returns_handles_and_records_read_log()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let source = insert_memory(&pg, &owner, "source abstraction", 5).await?;
    let derived = insert_memory(&pg, &owner, "derived abstraction", 6).await?;
    insert_edge(
        &pg,
        &owner,
        derived,
        source,
        "core/derived-from",
        RelationClass::Provenance,
    )
    .await?;

    let engine = Engine::new(
        FlavorRegistry::new().freeze(),
        MemoryStore::new(),
        Box::new(NoAuth::new(owner.principal.clone(), owner.clone())),
    )
    .with_storage(pg.clone().into_handle());
    let tool = proxima_core::personality::substrate_pack()
        .iter()
        .find(|tool| tool.tool_id() == "core/walk_lineage")
        .expect("walk_lineage substrate tool");
    let handles = Arc::new(HandleTable::new());
    let start_handle = handles.assign_memory(MemoryId::new(derived));
    let read_log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let palette: Vec<Arc<dyn PersonalityTool>> = Vec::new();
    let ctx = PersonalityToolContext::new(
        &engine,
        &owner,
        "test/personality",
        PersonalityInstanceId::new(Uuid::now_v7()),
        MemoryId::new(Uuid::now_v7()),
        MemoryId::new(Uuid::now_v7()),
        WakeChainDepth::new(0),
        Vec::new(),
        Vec::new(),
        &palette,
        handles,
    )
    .with_read_log(read_log.clone());

    let result = tool
        .invoke(
            &ctx,
            serde_json::json!({
                "memory": start_handle.as_str(),
                "direction": "ancestors",
                "depth": 1,
                "limit": 10,
            }),
        )
        .await?;

    assert!(!result.is_error);
    assert_eq!(result.content["start"], start_handle.as_str());
    assert_eq!(result.content["nodes"].as_array().expect("nodes").len(), 2);
    assert_eq!(result.content["edges"].as_array().expect("edges").len(), 1);
    let logged = read_log.lock().await.clone();
    assert!(logged.contains(&(MemoryId::new(source), WakeChainDepth::new(5))));
    assert!(logged.contains(&(MemoryId::new(derived), WakeChainDepth::new(6))));

    drop(engine);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

async fn insert_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    text: &str,
    wake_chain_depth: i16,
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
         VALUES ($1, $2, $3, $4, 'test/lineage-v1', 1, 'Abstraction',
                 $5, 'Wake', 'test-model', 'test-v1',
                 '00000000-0000-0000-0000-000000000000'::uuid, $6)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .bind(text)
    .bind(wake_chain_depth)
    .execute(pg.pool())
    .await?;
    Ok(memory_id)
}

async fn insert_edge(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    source: Uuid,
    target: Uuid,
    relation: &str,
    relation_class: RelationClass,
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
                 'Abstraction', $4, NULL,
                 'Abstraction', $5, NULL,
                 'Engine', NULL,
                 $6, $7, $8)",
    )
    .bind(edge_id)
    .bind(relation)
    .bind(relation_class)
    .bind(source)
    .bind(target)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .execute(pg.pool())
    .await?;
    Ok(edge_id)
}

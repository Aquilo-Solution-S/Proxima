mod common;

use std::sync::Arc;

use common::{create_db, db_url, drop_db};
use proxima_core::mcp::McpAuthorContext;
use proxima_core::{Engine, FlavorRegistry, Owner, OwnerRef, RelationClass, UserId};
use proxima_mcp_server::{McpAuthContext, McpToolHost};
use proxima_storage_pg::PgStorage;

#[tokio::test]
async fn core_read_resources_return_prefixed_ids_and_author()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = create_db().await?;
    let database_url = db_url(&db_name);
    let pg = PgStorage::connect(&database_url).await?;
    pg.run_migrations().await?;

    let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let author = uuid::Uuid::now_v7();
    let source = insert_memory(&pg, &owner, "source lineage memory", Some(author)).await?;
    let derived = insert_memory(&pg, &owner, "derived lineage memory", Some(author)).await?;
    let edge = insert_edge(&pg, &owner, derived, source).await?;

    let registry = FlavorRegistry::new().freeze();
    let engine = Arc::new(Engine::new(registry.clone()).with_storage(pg.clone().into_handle()));
    let server =
        McpToolHost::from_pool(pg.pool().clone(), owner, Arc::new(registry)).with_engine(engine);
    // The host is now the authoritative scope chokepoint, so reads need an
    // authenticated full-scope context (production always passes Some(auth);
    // a None context is unauthenticated and correctly denied).
    let auth = McpAuthContext::for_master(uuid::Uuid::now_v7(), owner);

    let fetched = server
        .read_resource(
            &format!("proxima://memory/A:{derived}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(fetched["memory"], format!("A:{derived}"));
    assert_eq!(fetched["kind"], "Abstraction");
    assert_eq!(
        fetched["authoring_personality_instance_id"],
        format!("I:{author}")
    );
    assert_eq!(fetched["handle"], format!("A:{derived}"));
    assert_eq!(fetched["body"], "derived lineage memory");
    assert!(
        fetched.get("neighbor_edges").is_none(),
        "neighbor_edges should be omitted unless expand_neighbors is true"
    );

    let expanded = server
        .read_resource(
            &format!("proxima://memory/{derived}?expand_neighbors=true"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(expanded["memory"], format!("A:{derived}"));
    assert_eq!(expanded["neighbor_edges"][0]["handle"], format!("E:{edge}"));
    assert_eq!(
        expanded["neighbor_edges"][0]["source"],
        format!("A:{derived}")
    );
    assert_eq!(
        expanded["neighbor_edges"][0]["target"],
        format!("A:{source}")
    );

    let lineage = server
        .read_resource(
            &format!("proxima://memory/A:{derived}/lineage?direction=ancestors&depth=1"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(lineage["start"], format!("A:{derived}"));
    assert!(
        lineage["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .any(|node| node["memory"] == format!("A:{source}"))
    );
    assert_eq!(lineage["edges"][0]["edge"], format!("E:{edge}"));
    assert_eq!(lineage["edges"][0]["source"], format!("A:{derived}"));
    assert_eq!(lineage["edges"][0]["target"], format!("A:{source}"));

    drop(server);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

async fn insert_memory(
    pg: &PgStorage,
    owner: &Owner,
    text: &str,
    personality_instance_id: Option<uuid::Uuid>,
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let memory_id = uuid::Uuid::now_v7();
    let (owner_kind, owner_principal_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, 'test/core-read-v1', 1, 'Abstraction',
                 $2, 'Wake', 'test-model', 'test-v1',
                 COALESCE($3, '00000000-0000-0000-0000-000000000000'::uuid), 0)",
    )
    .bind(memory_id)
    .bind(text)
    .bind(personality_instance_id)
    .execute(pg.pool())
    .await?;
    sqlx::query(proxima_storage_pg::access::owner_ref_compat::sql(
        "INSERT INTO __PROXIMA_ENTITY_OWNER__
            (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
         VALUES ($1, $2, $3, true, $4)",
    ))
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(personality_instance_id.unwrap_or_else(uuid::Uuid::nil))
    .execute(pg.pool())
    .await?;
    Ok(memory_id)
}

async fn insert_edge(
    pg: &PgStorage,
    _owner: &Owner,
    source: uuid::Uuid,
    target: uuid::Uuid,
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let edge_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind, authorship_owner_memory_id)
         VALUES ($1, 'core/derived-from', $2,
                 'Abstraction', $3, NULL,
                 'Abstraction', $4, NULL,
                 'Engine', NULL)",
    )
    .bind(edge_id)
    .bind(RelationClass::Provenance)
    .bind(source)
    .bind(target)
    .execute(pg.pool())
    .await?;
    Ok(edge_id)
}

fn author_ctx() -> McpAuthorContext {
    McpAuthorContext {
        model_id: "codex-test".into(),
        client_name: "codex".into(),
        client_version: "1".into(),
        personality_instance_id: None,
        caller_self_perspective: None,
    }
}

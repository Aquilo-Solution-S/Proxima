mod common;

use std::sync::Arc;

use common::{create_db, db_url, drop_db};
use proxima_core::mcp::{McpAuthorContext, McpToolExtensions};
use proxima_core::{
    AuthPath, AuthzContext, Engine, FlavorRegistry, Owner, OwnerRef, RelationClass, UserId,
};
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
    let source = insert_memory(&pg, &owner, "source lineage memory").await?;
    let derived = insert_memory(&pg, &owner, "derived lineage memory").await?;
    let edge = insert_edge(&pg, &owner, derived, source).await?;

    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let engine = Arc::new(
        Engine::new(registry.clone()).with_storage_ports(Arc::new(pg.clone()).storage_ports()),
    );
    let server = McpToolHost::from_engine(engine, McpToolExtensions::default());
    // The host is now the authoritative scope chokepoint, so reads need an
    // authenticated full-scope context (production always passes Some(auth);
    // a None context is unauthenticated and correctly denied).
    let auth = McpAuthContext {
        owner,
        authz: AuthzContext::single_owner(&owner, AuthPath::MasterDev)
            .narrowed_to_owner(owner)
            .expect("personal owner narrows"),
        model_id: None,
        master_token_id: Some(uuid::Uuid::now_v7()),
    };

    let fetched = server
        .read_resource(
            &format!("proxima://memory/A:{derived}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(fetched["memory"], format!("A:{derived}"));
    assert_eq!(fetched["kind"], "Abstraction");
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
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let memory_id = uuid::Uuid::now_v7();
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'test/core-read-v1', 1, 'Abstraction',
                 $4, 'AtoA', '00000000-0000-0000-0000-000000000431'::uuid,
                 '00000000-0000-0000-0000-000000000432'::uuid, NULL,
                 'test-model', 'test-v1')"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(text)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(memory_id)
}

async fn insert_edge(
    pg: &PgStorage,
    owner: &Owner,
    source: uuid::Uuid,
    target: uuid::Uuid,
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let edge_id = uuid::Uuid::now_v7();
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, owner_kind, owner_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind, authorship_owner_memory_id)
         VALUES ($1, $2, $3, 'core/derived-from', $4,
                 'Abstraction', $5, NULL,
                 'Abstraction', $6, NULL,
                 'Engine', NULL)",
    )
    .bind(edge_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(RelationClass::Provenance)
    .bind(source)
    .bind(target)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(edge_id)
}

fn author_ctx() -> McpAuthorContext {
    McpAuthorContext {
        model_id: "codex-test".into(),
        client_name: "codex".into(),
        client_version: "1".into(),
        caller_self_perspective: None,
    }
}

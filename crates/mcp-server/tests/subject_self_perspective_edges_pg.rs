mod common;

use std::sync::Arc;

use common::{create_db, db_url, drop_db};
use proxima_core::mcp::{McpAuthorContext, PrefixedUuidClass, parse_prefixed_uuid};
use proxima_core::storage::Storage;
use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::{AuthPath, AuthzContext, Engine, FlavorRegistry, Owner, Principal, UserId};
use proxima_mcp_server::{McpAuthContext, McpToolHost};
use proxima_storage_pg::PgStorage;
use serde_json::json;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn host_bearer_agent_memory_edges_attribute_to_subject_self_perspective()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = create_db().await?;
    let database_url = db_url(&db_name);
    let pg = PgStorage::connect(&database_url).await?;
    pg.run_migrations().await?;

    let owner = Principal::User(UserId::new(Uuid::now_v7()));

    let registry = FlavorRegistry::new();
    let frozen = registry.freeze();
    let engine = Arc::new(
        Engine::new(frozen.clone())
            .with_storage(pg.clone().into_handle())
            .with_embed(Arc::new(ConstantEmbedding::prefixed(
                "test-embed",
                &[1.0, 0.0, 0.0],
            ))),
    );
    let server = McpToolHost::from_pool(pg.pool().clone(), owner.clone(), Arc::new(frozen))
        .with_engine(engine);
    let auth = host_bearer_auth(&owner);

    let remembered = server
        .call_tool(
            "core/remember",
            json!({
                "title": "Subject self source",
                "body": "A source Fact for subject self-perspective attribution.",
                "idempotency_key": "subject-self-source"
            }),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    let source_handle = remembered["handle"].as_str().expect("remember handle");
    let source_id = parse_prefixed_uuid(source_handle, PrefixedUuidClass::Fact)?;

    let derived = server
        .call_tool(
            "core/derive",
            json!({
                "kind": "Abstraction",
                "title": "Subject self derivation",
                "body": "Derived under HostBearer subject identity.",
                "source_handles": [source_handle],
                "model_id": "codex-test",
                "idempotency_key": "subject-self-derivation"
            }),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    let derived_handle = derived["handle"].as_str().expect("derive handle");
    let derived_id = parse_prefixed_uuid(derived_handle, PrefixedUuidClass::Abstraction)?;
    let provenance_edge_handle = derived["provenance_edge_handles"][0]
        .as_str()
        .expect("provenance edge handle");
    let provenance_edge_id = parse_prefixed_uuid(provenance_edge_handle, PrefixedUuidClass::Edge)?;

    let linked = server
        .call_tool(
            "core/link",
            json!({
                "source": derived_handle,
                "target": source_handle,
                "reason": "The abstraction summarizes the source Fact.",
                "confidence": 90
            }),
            author_ctx(),
            Some(auth),
        )
        .await?;
    let link_edge_handle = linked["edge_handle"].as_str().expect("link edge handle");
    let link_edge_id = parse_prefixed_uuid(link_edge_handle, PrefixedUuidClass::Edge)?;

    let identity = pg
        .ensure_subject_personality(&owner, &owner)
        .await?;
    let subject_self = identity.self_perspective_memory_id.into_inner();
    let subject_instance = identity.instance_id.into_inner();

    assert_eq!(
        memory_personality_instance(pg.pool(), source_id).await?,
        subject_instance,
        "remembered Fact must be stamped by the HostBearer subject personality"
    );
    assert_eq!(
        memory_personality_instance(pg.pool(), derived_id).await?,
        subject_instance,
        "derived Abstraction must be stamped by the same subject personality"
    );
    assert_eq!(
        edge_authorship_owner_memory(pg.pool(), provenance_edge_id).await?,
        Some(subject_self),
        "derive provenance edge must attribute to the subject root perspective"
    );
    assert_eq!(
        edge_authorship_owner_memory(pg.pool(), link_edge_id).await?,
        Some(subject_self),
        "agent link edge must attribute to the subject root perspective"
    );

    drop(server);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

fn host_bearer_auth(owner: &Owner) -> McpAuthContext {
    McpAuthContext {
        owner: owner.clone(),
        authz: AuthzContext::single_owner(owner, AuthPath::HostBearer),
        model_id: None,
        master_token_id: None,
    }
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

async fn memory_personality_instance(
    pool: &sqlx::PgPool,
    memory_id: Uuid,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT personality_instance_id
         FROM proxima_core.memories
         WHERE memory_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pool)
    .await
}

async fn edge_authorship_owner_memory(
    pool: &sqlx::PgPool,
    edge_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT authorship_owner_memory_id
         FROM proxima_core.edges
         WHERE edge_id = $1",
    )
    .bind(edge_id)
    .fetch_one(pool)
    .await
}

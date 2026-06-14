#![allow(dead_code)]

use std::sync::Arc;

use proxima_core::mcp::{HandleTable, McpAuthorContext, McpToolCtx, OutputMode};
use proxima_core::{AuthPath, AuthzContext, FlavorRegistry, OrgId, Owner, Principal, UserId};
pub use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

pub fn owner_fixture() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::nil())),
        org_id: OrgId::new(Uuid::nil()),
    }
}

pub fn other_owner_fixture() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::nil()),
    }
}

pub async fn fresh_pg() -> Option<(PgStorage, String)> {
    let db_name = unique_db_name("proxima_test");
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    match PgStorage::connect(&url).await {
        Ok(pg) => Some((pg, db_name)),
        Err(err) => {
            let _ = drop_db(&db_name).await;
            panic!("PG required for tests but unavailable: {err}");
        }
    }
}

pub async fn migrated() -> Option<(PgStorage, String)> {
    let (pg, db_name) = fresh_pg().await?;
    if let Err(err) = async {
        pg.run_migrations().await?;
        proxima_flavor_goal::migrator().run(pg.pool()).await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await
    {
        let _ = drop_db(&db_name).await;
        panic!("migration failed: {err}");
    }
    Some((pg, db_name))
}

pub fn ctx(pg: &PgStorage, owner: Owner) -> McpToolCtx {
    let mut registry = FlavorRegistry::new();
    proxima_flavor_goal::register(&mut registry);
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    McpToolCtx {
        pool: pg.pool().clone(),
        owner,
        authz,
        handles: Some(Arc::new(HandleTable::new())),
        mode: OutputMode::Handles,
        registry: Arc::new(registry.freeze()),
        author: McpAuthorContext {
            model_id: "test-model".into(),
            client_name: "test".into(),
            client_version: "1".into(),
            personality_instance_id: None,
            caller_self_perspective: None,
        },
        caller_self_perspective: None,
        master_token_id: None,
        engine: None,
    }
}

pub async fn insert_abstraction(
    pg: &PgStorage,
    owner: &Owner,
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let owner_kind = proxima_core::OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    let memory_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id)
         VALUES ($1, $2, $3, $4, 'test/abstraction', 1, $5,
                 'evidence', $6, 'test-model', 'v1', $7)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .bind(proxima_core::EntityKind::Abstraction)
    .bind(proxima_core::MemoryOperatorKind::FtoA)
    .bind(Uuid::nil())
    .execute(pg.pool())
    .await?;
    Ok(memory_id)
}

pub async fn insert_self_perspective(
    pg: &PgStorage,
    owner: &Owner,
) -> Result<proxima_core::MemoryId, Box<dyn std::error::Error>> {
    let owner_kind = proxima_core::OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    let memory_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id)
         VALUES ($1, $2, $3, $4, 'test/self-perspective', 1, $5,
                 'self', $6, 'test-model', 'v1', $7)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .bind(proxima_core::EntityKind::Perspective)
    .bind(proxima_core::MemoryOperatorKind::AtoP)
    .bind(Uuid::nil())
    .execute(pg.pool())
    .await?;
    Ok(proxima_core::MemoryId::new(memory_id))
}

use std::sync::Arc;

use proxima_core::mcp::{HandleTable, McpAuthorContext, McpToolCtx};
use proxima_core::{FlavorRegistry, OrgId, Owner, Principal, UserId};
use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection};
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://postgres@localhost/postgres";

pub fn owner_fixture() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::nil())),
        org_id: OrgId::new(Uuid::nil()),
    }
}

#[allow(dead_code)]
pub fn other_owner_fixture() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::nil()),
    }
}

pub async fn fresh_pg() -> Option<(PgStorage, String)> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if create_db(&db_name).await.is_err() {
        eprintln!("skipping (no admin PG)");
        return None;
    }
    let url = format!("postgres://postgres@localhost/{db_name}");
    match PgStorage::connect(&url).await {
        Ok(pg) => Some((pg, db_name)),
        Err(err) => {
            let _ = drop_db(&db_name).await;
            eprintln!("skipping (PG unavailable): {err}");
            None
        }
    }
}

pub async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(ADMIN_URL).await?;
    conn.execute(format!("CREATE DATABASE \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

pub async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(ADMIN_URL).await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
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
        eprintln!("skipping (migration failed): {err}");
        let _ = drop_db(&db_name).await;
        return None;
    }
    Some((pg, db_name))
}

pub fn ctx(pg: &PgStorage, owner: Owner) -> McpToolCtx {
    let mut registry = FlavorRegistry::new();
    proxima_flavor_goal::register(&mut registry);
    McpToolCtx {
        pool: pg.pool().clone(),
        owner,
        handles: Arc::new(HandleTable::new()),
        registry: Arc::new(registry.freeze()),
        author: McpAuthorContext {
            model_id: "test-model".into(),
            client_name: "test".into(),
            client_version: "1".into(),
        },
    }
}

pub async fn insert_abstraction(
    pg: &PgStorage,
    owner: &Owner,
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let (owner_kind, owner_principal_id) = match owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    let memory_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_id, personality_state_hash)
         VALUES ($1, $2, $3, $4, 'test/abstraction', 1, 'Abstraction',
                 'evidence', 'FtoA', 'test-model', 'v1', 'test/personality', $5)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .bind([1_u8; 32])
    .execute(pg.pool())
    .await?;
    Ok(memory_id)
}

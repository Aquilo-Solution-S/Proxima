//! `PostgreSQL` receipts for the schema-evolution completion fence.

use crate::test_fixtures::create_core_db;
use proxiima_pg_testkit::{drop_db, split_role_urls};
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

const SOURCE: &str = "example/source-v1";
const TARGET: &str = "example/target-v2";

#[tokio::test]
async fn goal_writer_finishes_before_waiting_migration_locks_its_revision_table() {
    let database = format!("proxiima_test_{}", Uuid::now_v7().simple());
    create_core_db(&database)
        .await
        .expect("PG required for test");
    let result = async {
        let (_, platform_url) = split_role_urls(&database).await?;
        let pool = PgPool::connect(&platform_url).await?;
        let owner = owner_id(&pool).await?;
        let handle = Uuid::now_v7();
        let revision = Uuid::now_v7();
        let mut writer = pool.begin().await?;
        sqlx::query("SELECT set_config('app.proxiima_scope','platform',true)").execute(&mut *writer).await?;
        sqlx::query("INSERT INTO proxiima_core.goal_head(handle,schema_id,owner_id,t) VALUES($1,$2,$3,$4)")
            .bind(handle).bind(SOURCE).bind(owner).bind(revision).execute(&mut *writer).await?;

        let mut migration = pool.begin().await?;
        sqlx::query("SELECT set_config('app.proxiima_scope','platform',true)").execute(&mut *migration).await?;
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *migration).await?;
        let migrating = tokio::spawn(async move {
            super::execute::lock_sources(&mut migration, &[SOURCE.to_owned()]).await?;
            migration.commit().await.map_err(crate::error::map_err)
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE pid=$1 AND NOT granted)")
                    .bind(pid).fetch_one(&pool).await?;
                if blocked { return Ok::<(), sqlx::Error>(()); }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("migration reaches writer fence")?;
        let premature_lock: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE pid=$1 AND relation='proxiima_core.goal'::regclass AND mode='ShareRowExclusiveLock' AND granted)")
            .bind(pid).fetch_one(&pool).await?;
        assert!(!premature_lock, "migration must wait on goal_head before locking goal");
        sqlx::query("INSERT INTO proxiima_core.goal(handle,t,owner_id,title,text,state,request_id,\"by\") VALUES($1,$2,$3,'Goal','Body','Active','concurrent-create',$3)")
            .bind(handle).bind(revision).bind(owner).execute(&mut *writer).await?;
        writer.commit().await?;
        tokio::time::timeout(Duration::from_secs(5), migrating).await
            .expect("migration resumes after Goal commit").expect("migration task")
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let mut read = pool.begin().await?;
        sqlx::query("SELECT set_config('app.proxiima_scope','platform',true)").execute(&mut *read).await?;
        let schema: String = sqlx::query_scalar("SELECT schema_id FROM proxiima_core.goal WHERE t=$1")
            .bind(revision).fetch_one(&mut *read).await?;
        assert_eq!(schema, SOURCE, "the writer committed before the migration fence");
        read.commit().await?;
        Ok::<(), sqlx::Error>(())
    }.await;
    drop_db(&database).await.expect("drop test database");
    result.expect("concurrent Goal writer and schema migration");
}

async fn owner_id(pool: &PgPool) -> Result<Uuid, sqlx::Error> {
    let owner = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('app.proxiima_scope', 'platform', true)")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO proxiima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(owner)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(owner)
}

#[tokio::test]
async fn completed_source_is_fenced_and_target_remains_writable() {
    let database = format!("proxiima_test_{}", Uuid::now_v7().simple());
    create_core_db(&database)
        .await
        .expect("PG required for test");
    let result = async {
        let (runtime_url, platform_url) = split_role_urls(&database).await?;
        let runtime = PgPool::connect(&runtime_url).await?;
        let platform = PgPool::connect(&platform_url).await?;
        let owner = owner_id(&platform).await?;

        let mut tx = platform.begin().await?;
        sqlx::query("SELECT set_config('app.proxiima_scope', 'platform', true)")
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO proxiima_core.schema_evolution (source_kind,source_schema_id,evolution_id,spiice_id,target_schema_ids,target_kinds) VALUES ('cited_object',$1,'evolution-1','example',ARRAY[$2],ARRAY['cited_object'])")
            .bind(SOURCE).bind(TARGET).execute(&mut *tx).await?;
        tx.commit().await?;

        let mut source = runtime.begin().await?;
        sqlx::query("SELECT set_config('app.proxiima_scope', 'owner', true)")
            .execute(&mut *source).await?;
    sqlx::query("SELECT set_config('app.owner', $1::uuid[]::text, true), set_config('app.write_owner', $1::uuid[]::text, true)").bind(vec![owner])
            .execute(&mut *source).await?;
        let error = sqlx::query("INSERT INTO proxiima_core.blob (owner_id,schema_id,content_hash) VALUES ($1,$2,decode(repeat('01',32),'hex'))")
            .bind(owner).bind(SOURCE).execute(&mut *source).await
            .expect_err("completed source must refuse runtime writes");
        assert!(error.to_string().contains("completed evolution"), "{error}");
        source.rollback().await?;

        let mut target = runtime.begin().await?;
        sqlx::query("SELECT set_config('app.proxiima_scope', 'owner', true)").execute(&mut *target).await?;
        sqlx::query("SELECT set_config('app.owner', $1::uuid[]::text, true), set_config('app.write_owner', $1::uuid[]::text, true)").bind(vec![owner]).execute(&mut *target).await?;
        sqlx::query("INSERT INTO proxiima_core.blob (owner_id,schema_id,content_hash) VALUES ($1,$2,decode(repeat('02',32),'hex'))")
            .bind(owner).bind(TARGET).execute(&mut *target).await?;
        target.commit().await?;

        let mut forge = runtime.begin().await?;
        sqlx::query("SELECT set_config('app.proxiima_scope', 'owner', true)").execute(&mut *forge).await?;
        let error = sqlx::query("INSERT INTO proxiima_core.schema_evolution (source_kind,source_schema_id,evolution_id,spiice_id,target_schema_ids,target_kinds) VALUES ('cited_object','forged-source','forged','forged',ARRAY['forged-target'],ARRAY['cited_object'])")
            .execute(&mut *forge).await.expect_err("runtime must not forge completion metadata");
        assert!(error.to_string().contains("row-level security"));
        forge.rollback().await?;
        let mut capture_link = runtime.begin().await?;
        sqlx::query("SELECT set_config('app.proxiima_scope','owner',true),set_config('app.owner',$1::uuid[]::text,true),set_config('app.write_owner',$1::uuid[]::text,true)")
            .bind(vec![owner]).execute(&mut *capture_link).await?;
        let error = sqlx::query("INSERT INTO proxiima_core.schema_evolution_publication(publication_t,memory_t) VALUES ($1,$2)")
            .bind(Uuid::now_v7()).bind(Uuid::now_v7()).execute(&mut *capture_link).await
            .expect_err("runtime must not forge links that erase captured publications");
        assert_eq!(error.as_database_error().and_then(sqlx::error::DatabaseError::code).as_deref(), Some("42501"));
        capture_link.rollback().await?;
        Ok::<(), sqlx::Error>(())
    }.await;
    drop_db(&database).await.expect("drop test database");
    result.expect("schema-evolution fence assertions");
}

#[tokio::test]
async fn an_old_writer_waiting_on_retirement_is_refused_after_commit() {
    let database = format!("proxiima_test_{}", Uuid::now_v7().simple());
    create_core_db(&database)
        .await
        .expect("PG required for test");
    let result = async {
        let (runtime_url, platform_url) = split_role_urls(&database).await?;
        let platform = PgPool::connect(&platform_url).await?;
        let runtime = PgPool::connect(&runtime_url).await?;
        let owner = owner_id(&platform).await?;
        let mut retirement = platform.begin().await?;
        sqlx::query("SELECT set_config('app.proxiima_scope','platform',true)").execute(&mut *retirement).await?;
        sqlx::query("SELECT proxiima_core.lock_schema_evolution_exclusive($1)").bind(SOURCE).execute(&mut *retirement).await?;
        sqlx::query("INSERT INTO proxiima_core.schema_evolution(source_kind,source_schema_id,evolution_id,spiice_id,target_schema_ids,target_kinds)
                    VALUES('cited_object',$1,'retirement','example',ARRAY[$2],ARRAY['cited_object'])")
            .bind(SOURCE).bind(TARGET).execute(&mut *retirement).await?;
        let mut old_writer = tokio::spawn(async move {
            let mut tx = runtime.begin().await?;
            sqlx::query("SELECT set_config('app.proxiima_scope','owner',true),set_config('app.owner',$1::uuid[]::text,true),set_config('app.write_owner',$1::uuid[]::text,true)")
                .bind(vec![owner]).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO proxiima_core.blob(owner_id,schema_id,content_hash) VALUES($1,$2,decode(repeat('01',32),'hex'))")
                .bind(owner).bind(SOURCE).execute(&mut *tx).await
        });
        assert!(tokio::time::timeout(Duration::from_millis(100), &mut old_writer).await.is_err(), "old write must wait for the migration transaction");
        retirement.commit().await?;
        let error = tokio::time::timeout(Duration::from_secs(2), old_writer).await
            .expect("writer resumes after commit").expect("writer task does not panic")
            .expect_err("retired source cannot commit after the fence opens");
        assert!(error.to_string().contains("completed evolution"), "{error}");
        Ok::<(), sqlx::Error>(())
    }.await;
    drop_db(&database).await.expect("drop test database");
    result.expect("schema retirement writer race");
}

#[tokio::test]
async fn authority_permit_must_be_deleted_before_commit() {
    let database = format!("proxiima_test_{}", Uuid::now_v7().simple());
    create_core_db(&database)
        .await
        .expect("PG required for test");
    let result = async {
        let (_, platform_url) = split_role_urls(&database).await?;
        let pool = PgPool::connect(&platform_url).await?;
        let owner = owner_id(&pool).await?;
        let source = Uuid::now_v7();
        let target = Uuid::now_v7();

        let mut cleared = pool.begin().await?;
        sqlx::query("SELECT set_config('app.proxiima_scope', 'platform', true)")
            .execute(&mut *cleared).await?;
        sqlx::query("INSERT INTO proxiima_core.schema_evolution_authority (record_kind,source_id,target_id,source_schema,target_schema,record_owner,primary_target) VALUES ('blob',$1,$2,'s1','s2',$3,true)")
            .bind(source).bind(target).bind(owner).execute(&mut *cleared).await?;
        sqlx::query("DELETE FROM proxiima_core.schema_evolution_authority WHERE xact=pg_current_xact_id()")
            .execute(&mut *cleared).await?;
        cleared.commit().await?;

        let mut residual = pool.begin().await?;
        sqlx::query("SELECT set_config('app.proxiima_scope', 'platform', true)")
            .execute(&mut *residual).await?;
        sqlx::query("INSERT INTO proxiima_core.schema_evolution_authority (record_kind,source_id,target_id,source_schema,target_schema,record_owner,primary_target) VALUES ('blob',$1,$2,'s1','s2',$3,true)")
            .bind(source).bind(target).bind(owner).execute(&mut *residual).await?;
        assert!(residual.commit().await.is_err());
        Ok::<(), sqlx::Error>(())
    }.await;
    drop_db(&database).await.expect("drop test database");
    result.expect("authority drain assertions");
}

#[tokio::test]
async fn same_schema_string_is_kind_scoped() {
    let database = format!("proxiima_test_{}", Uuid::now_v7().simple());
    create_core_db(&database)
        .await
        .expect("PG required for test");
    let result = async {
        let (_, platform_url) = split_role_urls(&database).await?;
        let platform = PgPool::connect(&platform_url).await?;
        let owner = owner_id(&platform).await?;
        let mut tx = platform.begin().await?;
        sqlx::query("SELECT set_config('app.proxiima_scope', 'platform', true)")
            .execute(&mut *tx).await?;
        sqlx::query("INSERT INTO proxiima_core.schema_evolution (source_kind,source_schema_id,evolution_id,spiice_id,target_schema_ids,target_kinds) VALUES ('abstraction','shared/schema-v1','e','example',ARRAY['shared/schema-v2'],ARRAY['abstraction'])")
            .execute(&mut *tx).await?;
        let fact_handle = Uuid::now_v7();
        let fact_t = Uuid::now_v7();
        sqlx::query("INSERT INTO proxiima_core.memory_head (handle,kind,schema_id,owner_id,t) VALUES ($1,'fact','shared/fact-v1',$2,$3)")
            .bind(fact_handle).bind(owner).bind(fact_t).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO proxiima_core.memory (handle,t,kind,owner_id,schema_id,occurred_at,origins,refs,goal_refs,sidecar_tables,\"by\") VALUES ($1,$2,'fact',$3,'shared/fact-v1',now(),'{}','{}','{}','{}',$3)")
            .bind(fact_handle).bind(fact_t).bind(owner).execute(&mut *tx).await?;
        let handle = Uuid::now_v7();
        let t = Uuid::now_v7();
        sqlx::query("INSERT INTO proxiima_core.memory_head (handle,kind,schema_id,owner_id,t) VALUES ($1,'perspective',$2,$3,$4)")
            .bind(handle).bind("shared/schema-v1").bind(owner).bind(t)
            .execute(&mut *tx).await?;
        let content = Uuid::now_v7();
        sqlx::query("INSERT INTO proxiima_core.content(content_id,owner_id,schema_id,content_hash) VALUES($1,$2,'shared/schema-v1',decode(repeat('01',32),'hex'))")
            .bind(content).bind(owner).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO proxiima_core.memory (handle,t,kind,owner_id,schema_id,origins,refs,goal_refs,sidecar_tables,\"by\",content_id) VALUES ($1,$2,'perspective',$3,$4,'{}',ARRAY[$5]::uuid[],'{}','{}',$3,$6)")
            .bind(handle).bind(t).bind(owner).bind("shared/schema-v1").bind(fact_t).bind(content)
            .execute(&mut *tx).await?;
        tx.rollback().await?;
        Ok::<(), sqlx::Error>(())
    }.await;
    drop_db(&database).await.expect("drop test database");
    result.expect("kind-scoped schema fence assertion");
}

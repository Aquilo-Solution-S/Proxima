use proxima_core::storage_ports::{FactIngestPort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{AccessKind, MemoryId, OwnerId, SchemaId, SchemaVersion, UserId};
use proxima_pg_testkit::{DbGuard, db_url, split_role_urls};
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::{PgMigrationStorage, PgPlatformScope, PgPoolConfig, PgStorage, PgTuning};
use uuid::Uuid;

fn draft() -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new("core/collision-test-v1".into()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        occurred_at: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    }
}

#[tokio::test]
async fn store_identity_is_required_persisted_and_immutable()
-> Result<(), Box<dyn std::error::Error>> {
    let name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_core_db(&name).await?;
    let _guard = DbGuard::adopt(name.clone());
    let (runtime_url, platform_url) = split_role_urls(&name).await?;
    let migration = PgMigrationStorage::connect_with_config(
        &platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await?;
    migration.run_migrations().await?;
    let platform_pool = sqlx::PgPool::connect(&platform_url).await?;
    let scope = PgPlatformScope::new(platform_pool, &["proxima_core"]).await?;
    assert!(
        PgStorage::connect_with_config(
            &runtime_url,
            PgPoolConfig::default(),
            PgTuning::default(),
            scope.clone(),
            3,
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("no persisted number")
    );
    migration.initialize_store_number(3).await?;
    migration.initialize_store_number(3).await?;
    assert!(
        migration
            .initialize_store_number(4)
            .await
            .unwrap_err()
            .to_string()
            .contains("differs")
    );
    assert!(
        PgStorage::connect_with_config(
            &runtime_url,
            PgPoolConfig::default(),
            PgTuning::default(),
            scope.clone(),
            4,
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("differs")
    );
    let pg = PgStorage::connect_with_config(
        &runtime_url,
        PgPoolConfig::default(),
        PgTuning::default(),
        scope.clone(),
        3,
    )
    .await?;
    assert_eq!(pg.store_number(), 3);
    let mut tx = scope.begin().await?;
    assert!(
        sqlx::query("UPDATE proxima_core.store_identity SET store_number = 4")
            .execute(tx.as_mut())
            .await
            .is_err()
    );
    tx.rollback().await?;
    let mut tx = scope.begin().await?;
    assert!(
        sqlx::query("DELETE FROM proxima_core.store_identity")
            .execute(tx.as_mut())
            .await
            .is_err()
    );
    tx.rollback().await?;
    let current = proxima_core::new_memory_uuid(3);
    let mut tx = scope.begin().await?;
    sqlx::query("SET LOCAL TIME ZONE 'Europe/Berlin'")
        .execute(tx.as_mut())
        .await?;
    for millis in [0, 1, 999, 1000, (1_u64 << 48) - 1] {
        let mut bytes = current.into_bytes();
        bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
        let id = Uuid::from_bytes(bytes);
        let sql_time: time::OffsetDateTime =
            sqlx::query_scalar("SELECT proxima_core.memory_created_at($1)")
                .bind(id)
                .fetch_one(tx.as_mut())
                .await?;
        assert_eq!(sql_time, MemoryId::new(id).created_at());
    }
    let sql_time: time::OffsetDateTime =
        sqlx::query_scalar("SELECT proxima_core.memory_created_at($1)")
            .bind(current)
            .fetch_one(tx.as_mut())
            .await?;
    assert_eq!(sql_time, MemoryId::new(current).created_at());
    tx.commit().await?;
    Ok(())
}

#[tokio::test]
async fn a_colliding_insert_retries_without_leaving_an_old_head()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, guard) = proxima_storage_pg::test_fixtures::fresh_pg("store_collision").await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let existing = pg.ingest_fact_atomic(&permit, &draft(), None).await?;
    let admin = sqlx::PgPool::connect(&db_url(guard.name())).await?;
    let existing_t = existing.memory_id.into_inner();
    assert_eq!(existing_t.get_version_num(), 8);
    assert_eq!(proxima_core::memory_store_number(existing_t), 1);
    let occurred_at: time::OffsetDateTime =
        sqlx::query_scalar("SELECT occurred_at FROM proxima_core.memory WHERE t = $1")
            .bind(existing_t)
            .fetch_one(&admin)
            .await?;
    assert_eq!(occurred_at, existing.memory_id.created_at());
    // SQL-POLICY: fixed-fragment — the only interpolation is a typed UUID.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE SEQUENCE proxima_core.collision_attempt;
         GRANT USAGE ON SEQUENCE proxima_core.collision_attempt TO PUBLIC;
         CREATE FUNCTION proxima_core.force_one_collision() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             IF nextval('proxima_core.collision_attempt') = 1 THEN NEW.t := '{}'; END IF;
             RETURN NEW;
         END; $$;
         CREATE TRIGGER force_collision BEFORE INSERT ON proxima_core.memory
             FOR EACH ROW EXECUTE FUNCTION proxima_core.force_one_collision();",
        existing.memory_id.into_inner()
    )))
    .execute(&admin)
    .await?;
    let mut colliding = draft();
    colliding.source_id = Some("collision-source".into());
    colliding.ingest_key = Some("succeeds-after-collision".into());
    let outcome = pg.ingest_fact_atomic(&permit, &colliding, None).await?;
    assert_ne!(outcome.memory_id, existing.memory_id);
    assert_eq!(
        proxima_core::memory_store_number(outcome.memory_id.into_inner()),
        1
    );
    let heads: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.memory_head WHERE owner_id = $1")
            .bind(owner.into_inner())
            .fetch_one(&admin)
            .await?;
    assert_eq!(heads, 2);
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.memory WHERE owner_id = $1")
            .bind(owner.into_inner())
            .fetch_one(&admin)
            .await?;
    assert_eq!(rows, 2);
    let keyed_t: Uuid = sqlx::query_scalar("SELECT t FROM proxima_core.ingest_keys WHERE owner_id = $1 AND source_id = 'collision-source'")
        .bind(owner.into_inner()).fetch_one(&admin).await?;
    assert_eq!(keyed_t, outcome.memory_id.into_inner());
    // A second collision refuses rather than entering the outer transient retry.
    // SQL-POLICY: fixed-fragment — the only interpolation is a typed UUID.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE OR REPLACE FUNCTION proxima_core.force_one_collision() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             PERFORM nextval('proxima_core.collision_attempt');
             NEW.t := '{}';
             RETURN NEW;
         END; $$;", existing.memory_id.into_inner()
    )))
    .execute(&admin)
    .await?;
    colliding.ingest_key = Some("refused-after-two-collisions".into());
    let refused = pg
        .ingest_fact_atomic(&permit, &colliding, None)
        .await
        .unwrap_err();
    assert!(matches!(
        refused,
        proxima_core::StorageError::ConstraintViolation(_)
    ));
    let attempts: i64 = sqlx::query_scalar("SELECT last_value FROM proxima_core.collision_attempt")
        .fetch_one(&admin)
        .await?;
    assert_eq!(attempts, 4);
    let keys: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.ingest_keys WHERE owner_id = $1")
            .bind(owner.into_inner())
            .fetch_one(&admin)
            .await?;
    assert_eq!(keys, 1, "the refused write leaves no replay key");
    let heads: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.memory_head WHERE owner_id = $1")
            .bind(owner.into_inner())
            .fetch_one(&admin)
            .await?;
    assert_eq!(heads, 2);
    Ok(())
}

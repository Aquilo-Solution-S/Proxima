//! v0.0.31 refuses undelivered pre-v0.0.28 outbox records before changing the
//! schema, while delivered legacy records remain upgradeable and immutable.

use std::borrow::Cow;

use proxima_core::storage_ports::publication::{ConsumerName, PublicationConsumerPort};
use proxima_pg_testkit::{create_db, db_url, drop_db, split_role_urls};
use proxima_storage_pg::{PgPlatformScope, PgStorage, begin_migration_transaction, core_migrator};
use sqlx::PgPool;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

async fn database_at_0027(db_name: &str) -> TestResult<PgStorage> {
    create_db(db_name).await?;
    let pg = PgStorage::connect(&db_url(db_name)).await?;
    let (_, platform_url) = split_role_urls(db_name).await?;
    let platform = PgPool::connect(&platform_url).await?;
    let mut staged = core_migrator();
    staged.migrations = Cow::Owned(
        staged
            .iter()
            .filter(|migration| migration.version < 28)
            .cloned()
            .collect(),
    );
    let mut connection = platform.acquire().await?;
    let mut migration = begin_migration_transaction(&mut connection).await?;
    staged.run(migration.as_mut()).await?;
    migration.commit().await?;
    drop(connection);
    let scope = PgPlatformScope::new(platform, &["proxima_core"]).await?;
    Ok(pg.with_platform_scope(scope))
}

async fn run_current_migrations(pg: &PgStorage) -> TestResult<()> {
    let (_, platform_url) = split_role_urls(
        &sqlx::query_scalar::<_, String>("SELECT current_database()")
            .fetch_one(pg.pool_for_tests())
            .await?,
    )
    .await?;
    let platform = PgPool::connect(&platform_url).await?;
    let mut connection = platform.acquire().await?;
    let mut migration = begin_migration_transaction(&mut connection).await?;
    core_migrator().run(migration.as_mut()).await?;
    migration.commit().await?;
    Ok(())
}

async fn insert_owner_and_legacy_record(
    pool: &PgPool,
    delivered: bool,
) -> TestResult<(Uuid, Vec<u8>)> {
    let owner = Uuid::now_v7();
    let t = Uuid::now_v7();
    let envelope = br#"{"specversion":"1.0","id":"F:legacy","source":"urn:test","type":"probe/listenable-v1","data":null}"#.to_vec();
    sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(owner)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO proxima_core.publication_outbox
             (t, owner_id, schema_id, schema_version, event_type, event_id,
              envelope, envelope_digest, state, published_at)
         VALUES ($1, $2, 'probe/listenable', 1, 'probe/listenable-v1', $3,
                 $4, decode(repeat('00', 32), 'hex'),
                 CASE WHEN $5 THEN 'published'::proxima_core.publication_state
                      ELSE 'pending'::proxima_core.publication_state END,
                 CASE WHEN $5 THEN now() ELSE NULL END)",
    )
    .bind(t)
    .bind(owner)
    .bind(format!("F:{t}"))
    .bind(&envelope)
    .bind(delivered)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.publication_delivery
             (t, consumer, state, published_at)
         VALUES ($1, 'default',
                 CASE WHEN $2 THEN 'published'::proxima_core.publication_state
                      ELSE 'pending'::proxima_core.publication_state END,
                 CASE WHEN $2 THEN now() ELSE NULL END)",
    )
    .bind(t)
    .bind(delivered)
    .execute(pool)
    .await?;
    Ok((t, envelope))
}

#[tokio::test]
async fn an_undelivered_legacy_record_refuses_before_schema_change_then_retire_allows_upgrade()
-> TestResult {
    let db_name = format!("proxima_test_outbox_guard_{}", Uuid::now_v7().simple());
    let result: TestResult = async {
        let pg = database_at_0027(&db_name).await?;
        let pool = pg.pool_for_tests();
        let (t, _) = insert_owner_and_legacy_record(pool, false).await?;

        let before_state: String = sqlx::query_scalar(
            "SELECT state::text FROM proxima_core.publication_outbox LIMIT 1",
        )
        .fetch_one(pool)
        .await?;
        let before_columns: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.columns
             WHERE table_schema = 'proxima_core' AND table_name = 'publication_outbox'
               AND column_name IN ('claim_token', 'claimed_by', 'lease_expires_at', 'attempts')",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(before_state, "pending");
        assert_eq!(before_columns, 4);
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT 'claimed'::proxima_core.publication_state::text")
                .fetch_one(pool).await?,
            "claimed",
            "the old shared state type includes claimed"
        );
        let error = run_current_migrations(&pg)
            .await
            .expect_err("an owed pre-v0.0.28 lane must refuse the upgrade");
        let message = error.to_string();
        assert!(message.contains("drain"));
        assert!(message.contains("PublicationConsumerPort::retire"));
        assert!(message.contains("default"));
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT state::text FROM proxima_core.publication_outbox LIMIT 1",
            )
            .fetch_one(pool)
            .await?,
            before_state
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM information_schema.columns
                 WHERE table_schema = 'proxima_core' AND table_name = 'publication_outbox'
                   AND column_name IN ('claim_token', 'claimed_by', 'lease_expires_at', 'attempts')",
            )
            .fetch_one(pool)
            .await?,
            before_columns
        );

        assert_eq!(
            sqlx::query_as::<_, (bool, bool, bool)>(
                "SELECT to_regclass('proxima_core.group_edges') IS NULL,
                        to_regtype('proxima_core.publication_record_state') IS NULL,
                        NOT EXISTS (SELECT 1 FROM public._sqlx_migrations WHERE version = 28)"
            ).fetch_one(pool).await?,
            (true, true, true),
            "no v0.0.31 table, enum or ledger entry after refusal"
        );
        assert_eq!(
            PublicationConsumerPort::retire(&pg, &ConsumerName::default()).await?,
            proxima_core::storage_ports::publication::RetireOutcome::Retired { undelivered: 1 }
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT state::text FROM proxima_core.publication_outbox WHERE t = $1")
                .bind(t).fetch_one(pool).await?,
            "published",
            "retiring the final owed consumer settles the record before the upgrade"
        );
        run_current_migrations(&pg).await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM information_schema.columns
                 WHERE table_schema = 'proxima_core' AND table_name = 'publication_outbox'
                   AND column_name IN ('claim_token', 'claimed_by', 'lease_expires_at', 'attempts')",
            )
            .fetch_one(pool)
            .await?,
            0
        );
        assert!(!sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM pg_constraint
             WHERE conname = 'publication_outbox_no_claim_chk')",
        )
        .fetch_one(pool)
        .await?);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn a_delivered_legacy_record_upgrades_unchanged_and_record_state_rejects_claimed()
-> TestResult {
    let db_name = format!("proxima_test_outbox_delivered_{}", Uuid::now_v7().simple());
    let result: TestResult = async {
        let pg = database_at_0027(&db_name).await?;
        let pool = pg.pool_for_tests();
        let (t, envelope) = insert_owner_and_legacy_record(pool, true).await?;
        run_current_migrations(&pg).await?;
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT state::text FROM proxima_core.publication_outbox WHERE t = $1",
            )
            .bind(t)
            .fetch_one(pool)
            .await?,
            "published"
        );
        assert_eq!(
            sqlx::query_scalar::<_, Vec<u8>>(
                "SELECT envelope FROM proxima_core.publication_outbox WHERE t = $1"
            )
            .bind(t)
            .fetch_one(pool)
            .await?,
            envelope
        );
        let error = sqlx::query(
            "UPDATE proxima_core.publication_outbox
                SET state = 'claimed'::proxima_core.publication_record_state
              WHERE t = $1",
        )
        .bind(t)
        .execute(pool)
        .await
        .expect_err("record state must have no claimed variant");
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("22P02"),
            "claimed is refused by the record enum, before any row constraint"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

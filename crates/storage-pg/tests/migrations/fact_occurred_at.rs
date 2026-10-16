//! Migration 0030 (`_v036_`) gives every Fact the time it occurred.
//!
//! The fixture is a database standing at the released 0029 schema, with a
//! Fact and an Abstraction written the way the released binary wrote them:
//! no `occurred_at` column. The upgrade is then the real runner, ledger
//! included.
use std::borrow::Cow;

use proxima_pg_testkit::{create_db, drop_db, split_role_urls};
use proxima_storage_pg::{PgPoolConfig, PgTuning, begin_migration_transaction, core_migrator};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A database with every migration up to 0029 applied and recorded.
async fn database_at_0029(db_name: &str) -> TestResult<sqlx::PgPool> {
    create_db(db_name).await?;
    let (_, platform_url) = split_role_urls(db_name).await?;
    let platform = PgPoolOptions::new()
        .max_connections(1)
        .connect(&platform_url)
        .await?;
    let mut staged = core_migrator();
    staged.migrations = Cow::Owned(
        staged
            .iter()
            .filter(|migration| migration.version < 30)
            .cloned()
            .collect(),
    );
    let mut connection = platform.acquire().await?;
    let mut migration = begin_migration_transaction(&mut connection).await?;
    staged.run(migration.as_mut()).await?;
    migration.commit().await?;
    drop(connection);
    assert!(
        !sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = 'proxima_core' AND table_name = 'memory' AND column_name = 'occurred_at')")
            .fetch_one(&platform).await?,
        "the fixture stands at 0029, before the column exists"
    );
    Ok(platform)
}

/// A `UUIDv7` minted at an exact millisecond, so the write time the backfill
/// reads out of it is known.
fn t_at(millis: u64) -> Uuid {
    Uuid::new_v7(uuid::Timestamp::from_unix(
        uuid::NoContext,
        millis / 1000,
        u32::try_from(millis % 1000).expect("below 1000") * 1_000_000,
    ))
}

async fn seed_memory(
    pool: &sqlx::PgPool,
    owner: Uuid,
    t: Uuid,
    kind: &str,
    origins: &[Uuid],
    content_id: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    let handle = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, $2::proxima_core.memory_kind, 'test/upgrade-v1', $3, $4)",
    )
    .bind(handle)
    .bind(kind)
    .bind(owner)
    .bind(t)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id, origins, content_id)
         VALUES ($1, $2, $3::proxima_core.memory_kind, $4, 'test/upgrade-v1', $5, $6)",
    )
    .bind(handle)
    .bind(t)
    .bind(kind)
    .bind(owner)
    .bind(origins)
    .bind(content_id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn stored_occurrence(
    pool: &sqlx::PgPool,
    t: Uuid,
) -> Result<Option<time::OffsetDateTime>, sqlx::Error> {
    sqlx::query_scalar("SELECT occurred_at FROM proxima_core.memory WHERE t = $1")
        .bind(t)
        .fetch_one(pool)
        .await
}

/// A Fact written before the column existed occurred at its write time; an
/// Abstraction gets no time; and from here on the time is part of the
/// append-only row and present on every Fact.
#[tokio::test]
#[allow(clippy::too_many_lines)] // the backfill, the repeat and the two guards share one fixture
async fn a_fact_written_before_the_column_occurred_at_its_write_time() -> TestResult {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    let result: TestResult = async {
        let pool = database_at_0029(&db_name).await?;
        sqlx::query("SELECT set_config('app.proxima_scope', 'platform', false)")
            .execute(&pool)
            .await?;
        let owner = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner)
            .execute(&pool)
            .await?;

        let written_ms = 1_700_000_000_123;
        let fact = t_at(written_ms);
        seed_memory(&pool, owner, fact, "fact", &[], None).await?;
        let content_id: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
             VALUES ($1, 'test/upgrade-v1', $2)
             RETURNING content_id",
        )
        .bind(owner)
        .bind(vec![7_u8; 32])
        .fetch_one(&pool)
        .await?;
        let abstraction = t_at(written_ms + 1);
        seed_memory(
            &pool,
            owner,
            abstraction,
            "abstraction",
            &[fact],
            Some(content_id),
        )
        .await?;

        let migration = proxima_storage_pg::PgMigrationStorage::connect_with_config(
            &split_role_urls(&db_name).await?.1,
            PgPoolConfig::default(),
            PgTuning::default(),
        )
        .await?;
        migration.run_migrations().await?;

        let write_time =
            time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(written_ms) * 1_000_000)?;
        assert_eq!(stored_occurrence(&pool, fact).await?, Some(write_time));
        assert_eq!(stored_occurrence(&pool, abstraction).await?, None);
        super::assert_current_markers(&pool).await?;
        super::assert_current_schema(&pool).await?;

        // Running the file again changes nothing.
        super::apply_core_migrations(&pool, 30..=30).await?;
        assert_eq!(stored_occurrence(&pool, fact).await?, Some(write_time));
        assert_eq!(stored_occurrence(&pool, abstraction).await?, None);

        // The backfill turned the append-only guard off for one statement.
        let refused = sqlx::query(
            "UPDATE proxima_core.memory SET occurred_at = occurred_at - interval '1 day'
              WHERE t = $1",
        )
        .bind(fact)
        .execute(&pool)
        .await
        .expect_err("a Fact's occurrence time must not be rewritable");
        assert!(refused.to_string().contains("append-only"), "{refused}");

        // Every Fact carries the time, and nothing else does.
        let refused = seed_memory(&pool, owner, Uuid::now_v7(), "fact", &[], None)
            .await
            .expect_err("a Fact without an occurrence time must be refused");
        assert!(
            refused.to_string().contains("memory_fact_occurred_at_chk"),
            "{refused}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

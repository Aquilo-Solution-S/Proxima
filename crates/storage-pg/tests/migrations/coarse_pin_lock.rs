use proxima_pg_testkit::{create_db, db_url, drop_db, split_role_urls};
use proxima_storage_pg::{PgStorage, begin_migration_transaction, core_migrator};
use uuid::Uuid;

/// A live v0.0.27 database keeps `memory_pin_checks`' identity, definer
/// settings and closed ACL; the marker refuses the body without the mode.
#[tokio::test]
async fn coarse_pin_lock_upgrade_preserves_pin_check_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&db_url(&db_name)).await?;
        let (_, platform_url) = split_role_urls(&db_name).await?;
        let platform = sqlx::PgPool::connect(&platform_url).await?;
        let mut staged = core_migrator();
        staged.migrations = std::borrow::Cow::Owned(
            staged
                .iter()
                .filter(|migration| migration.version < 22)
                .cloned()
                .collect(),
        );
        let mut connection = platform.acquire().await?;
        let mut migration = begin_migration_transaction(&mut connection).await?;
        staged.run(migration.as_mut()).await?;
        migration.commit().await?;
        drop(connection);
        let identity_before = function_identity(pg.pool_for_tests()).await?;

        super::apply_current_migrations(&pg).await?;
        assert_eq!(
            function_identity(pg.pool_for_tests()).await?,
            identity_before
        );
        super::assert_current_markers(pg.pool_for_tests()).await?;

        // The pre-0022 shape: per-target locks only.
        sqlx::query(
            "CREATE OR REPLACE FUNCTION proxima_core.memory_pin_checks()
             RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER
             SET search_path = pg_catalog, proxima_core, pg_temp
             AS $$
             BEGIN
                 PERFORM proxima_core.lock_pin_targets(ARRAY[NEW.t] || NEW.origins);
                 RETURN NEW;
             END;
             $$",
        )
        .execute(&platform)
        .await?;
        let error = super::assert_current_markers(pg.pool_for_tests())
            .await
            .expect_err("the current ledger cannot hide a pin check without the mode");
        assert!(error.to_string().contains("coarse table locks"), "{error}");

        platform.close().await;
        Ok(())
    }
    .await;
    drop_db(&db_name).await?;
    result
}

async fn function_identity(
    pool: &sqlx::PgPool,
) -> Result<(i64, i64, bool, Option<String>, Vec<String>), sqlx::Error> {
    sqlx::query_as(
        "SELECT oid::bigint, proowner::bigint, prosecdef, proacl::text, proconfig
           FROM pg_proc
          WHERE oid = 'proxima_core.memory_pin_checks()'::regprocedure",
    )
    .fetch_one(pool)
    .await
}

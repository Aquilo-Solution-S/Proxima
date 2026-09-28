use proxima_pg_testkit::{create_db, db_url, drop_db, split_role_urls};
use proxima_storage_pg::{PgStorage, begin_migration_transaction, core_migrator};
use uuid::Uuid;

/// A live v0.0.26 database keeps its function identity, authority and data.
#[tokio::test]
async fn query_stopwords_upgrade_preserves_identity_and_runtime_authority()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&db_url(&db_name)).await?;
        let (runtime_url, platform_url) = split_role_urls(&db_name).await?;
        let platform = sqlx::PgPool::connect(&platform_url).await?;
        let mut staged = core_migrator();
        staged.migrations = std::borrow::Cow::Owned(
            staged
                .iter()
                .filter(|migration| migration.version < 21)
                .cloned()
                .collect(),
        );
        let mut connection = platform.acquire().await?;
        let mut migration = begin_migration_transaction(&mut connection).await?;
        staged.run(migration.as_mut()).await?;
        migration.commit().await?;
        drop(connection);
        sqlx::query("INSERT INTO proxima_core.lexical_languages VALUES ('german')")
            .execute(pg.pool_for_tests())
            .await?;
        let identity_before = function_identity(pg.pool_for_tests()).await?;
        let catalog_before = lexical_catalog(pg.pool_for_tests()).await?;
        let query = "Wie funktioniert der checkpoint recovery und das commit protocol?";
        let before: String = sqlx::query_scalar(
            "SELECT proxima_core.lexical_query_text('english', $1)",
        )
        .bind(query)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(before, query, "the released function is the identity");

        super::apply_current_migrations(&pg).await?;
        assert_eq!(function_identity(pg.pool_for_tests()).await?, identity_before);
        assert_eq!(lexical_catalog(pg.pool_for_tests()).await?, catalog_before);
        super::assert_current_markers(pg.pool_for_tests()).await?;

        let runtime = sqlx::PgPool::connect(&runtime_url).await?;
        let mut connection = runtime.acquire().await?;
        sqlx::query(
            "SELECT set_config('app.proxima_scope', 'owner', false),
                    set_config('app.owner', $1, false)",
        )
        .bind(format!("{{{}}}", Uuid::now_v7()))
        .execute(&mut *connection)
        .await?;
        let after: String = sqlx::query_scalar(
            "SELECT proxima_core.lexical_query_text('english', $1)",
        )
        .bind(query)
        .fetch_one(&mut *connection)
        .await?;
        assert_eq!(after, " funktioniert  checkpoint recovery   commit protocol?");
        drop(connection);
        runtime.close().await;

        sqlx::query(
            "CREATE OR REPLACE FUNCTION proxima_core.lexical_query_text(config regconfig, query_text text)
             RETURNS text LANGUAGE sql STABLE PARALLEL SAFE
             SET search_path = pg_catalog, proxima_core, pg_temp
             AS 'SELECT query_text'",
        )
        .execute(&platform)
        .await?;
        let error = super::assert_current_markers(pg.pool_for_tests())
            .await
            .expect_err("the current ledger cannot hide a reverted identity function");
        assert!(error.to_string().contains("dominant-language stopwords"), "{error}");
        platform.close().await;
        Ok(())
    }
    .await;
    drop_db(&db_name).await?;
    result
}

async fn function_identity(pool: &sqlx::PgPool) -> Result<(i64, i64, Option<String>), sqlx::Error> {
    sqlx::query_as(
        "SELECT oid::bigint, proowner::bigint, proacl::text
           FROM pg_proc
          WHERE oid = 'proxima_core.lexical_query_text(regconfig,text)'::regprocedure",
    )
    .fetch_one(pool)
    .await
}

async fn lexical_catalog(pool: &sqlx::PgPool) -> Result<(Vec<String>, String, i64), sqlx::Error> {
    sqlx::query_as(
        "SELECT (SELECT array_agg(config::text ORDER BY config::text)
                   FROM proxima_core.lexical_languages),
                (SELECT config::text FROM proxima_core.lexical_default),
                pg_relation_filenode('proxima_core.projection')::bigint",
    )
    .fetch_one(pool)
    .await
}

use proxima_pg_testkit::{create_db, db_url, drop_db, split_role_urls};
use proxima_storage_pg::{PgStorage, begin_migration_transaction, core_migrator};
use uuid::Uuid;

/// 0023 names what decides the kind of every `proxima_core` table's rows and
/// refuses a table it does not name: a new owner-keyed table cannot land on
/// the Fact lists by default.
#[tokio::test]
async fn an_unclassified_table_refuses_the_kind_rule_migration()
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
                .filter(|migration| migration.version < 23)
                .cloned()
                .collect(),
        );
        let mut connection = platform.acquire().await?;
        let mut migration = begin_migration_transaction(&mut connection).await?;
        staged.run(migration.as_mut()).await?;
        migration.commit().await?;
        drop(connection);

        sqlx::query("CREATE TABLE proxima_core.unclassified_probe (owner_id uuid NOT NULL)")
            .execute(&platform)
            .await?;
        let error = super::apply_current_migrations(&pg)
            .await
            .expect_err("0023 refuses a table it does not classify");
        assert!(
            error
                .to_string()
                .contains("owner RLS classification missing for proxima_core.unclassified_probe"),
            "{error}"
        );

        sqlx::query("DROP TABLE proxima_core.unclassified_probe")
            .execute(&platform)
            .await?;
        super::apply_current_migrations(&pg).await?;
        super::assert_current_markers(pg.pool_for_tests()).await?;
        platform.close().await;
        Ok(())
    }
    .await;
    drop_db(&db_name).await?;
    result
}

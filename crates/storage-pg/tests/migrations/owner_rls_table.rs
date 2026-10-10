use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

/// The schema-marker probe for 0024 accepts a fresh schema and names, in the
/// order it checks them, each piece of the per-table installer a damaged
/// schema lacks: the shared class code, the census, the per-table function,
/// the class type, and a schema installer that no longer builds its policies
/// through the shared class code.
#[tokio::test]
async fn schema_markers_name_a_missing_per_table_installer_piece()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&db_url(&db_name)).await?;
        super::apply_current_migrations(&pg).await?;
        super::assert_current_markers(pg.pool_for_tests()).await?;

        sqlx::query(
            "CREATE OR REPLACE FUNCTION proxima_core.install_owner_rls(
                 target_schema text,
                 owner_id_tables text[],
                 fk_parent_tables text[],
                 ownerless_tables text[],
                 memory_owner_tables text[] DEFAULT '{}'
             ) RETURNS void LANGUAGE plpgsql SECURITY INVOKER
             SET search_path = pg_catalog
             AS $stub$ BEGIN NULL; END $stub$",
        )
        .execute(pg.pool_for_tests())
        .await?;
        let error = super::assert_current_markers(pg.pool_for_tests())
            .await
            .expect_err("a schema installer outside the shared class code is refused");
        assert!(
            error.to_string().contains(
                "install_owner_rls must build its policies through owner_rls_apply_class"
            ),
            "{error}"
        );

        // Each drop below removes a piece that an earlier check of the probe
        // does not reach, so the probe names exactly that piece.
        sqlx::query(
            "DROP FUNCTION proxima_core.owner_rls_apply_class(
                 text, text, text, text, proxima_core.owner_rls_class, text)",
        )
        .execute(pg.pool_for_tests())
        .await?;
        let error = super::assert_current_markers(pg.pool_for_tests())
            .await
            .expect_err("the shared class code is required");
        assert!(
            error
                .to_string()
                .contains("missing function proxima_core.owner_rls_apply_class"),
            "{error}"
        );

        sqlx::query("DROP FUNCTION proxima_core.assert_owner_rls_census(text)")
            .execute(pg.pool_for_tests())
            .await?;
        let error = super::assert_current_markers(pg.pool_for_tests())
            .await
            .expect_err("the census function is required");
        assert!(
            error
                .to_string()
                .contains("missing function proxima_core.assert_owner_rls_census"),
            "{error}"
        );

        sqlx::query(
            "DROP FUNCTION proxima_core.install_owner_rls_table(
                 text, text, proxima_core.owner_rls_class, text)",
        )
        .execute(pg.pool_for_tests())
        .await?;
        let error = super::assert_current_markers(pg.pool_for_tests())
            .await
            .expect_err("the per-table installer is required");
        assert!(
            error
                .to_string()
                .contains("missing function proxima_core.install_owner_rls_table"),
            "{error}"
        );

        sqlx::query("DROP TYPE proxima_core.owner_rls_class")
            .execute(pg.pool_for_tests())
            .await?;
        let error = super::assert_current_markers(pg.pool_for_tests())
            .await
            .expect_err("the class type is required");
        assert!(
            error
                .to_string()
                .contains("missing type proxima_core.owner_rls_class"),
            "{error}"
        );
        Ok(())
    }
    .await;
    drop_db(&db_name).await?;
    result
}

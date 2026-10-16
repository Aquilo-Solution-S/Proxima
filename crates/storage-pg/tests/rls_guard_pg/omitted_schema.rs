use proxima_storage_pg::{PgPlatformScope, PgPoolConfig, PgStorage, PgTuning};

use super::{cleanup, execute, quoted_identifier, setup_full_schema};

fn role_url(database: &str, role: &str, password: &str) -> String {
    let url = proxima_pg_testkit::db_url(database);
    let (scheme, rest) = url.split_once("://").expect("scheme");
    let (_credentials, host) = rest.rsplit_once('@').expect("credentials");
    format!("{scheme}://{role}:{password}@{host}")
}

#[tokio::test]
async fn storage_construction_refuses_unsafe_registered_schema_outside_scope() {
    let (database, admin, runtime, platform, platform_role, runtime_role, password) =
        setup_full_schema().await;
    let scope = PgPlatformScope::new(platform.clone(), &["proxima_core"])
        .await
        .expect("the core platform scope is valid before the corruption");

    execute(
        &admin,
        &format!(
            "ALTER TABLE proxima_code.code_chunk_v1 OWNER TO {}",
            quoted_identifier(&runtime_role)
        ),
    )
    .await;

    let error = PgStorage::connect_with_config(
        &role_url(&database, &runtime_role, &password),
        PgPoolConfig::default(),
        PgTuning::default(),
        scope,
    )
    .await
    .expect_err("storage construction must reject the omitted unsafe schema");
    assert!(error.to_string().contains("proxima_code.code_chunk_v1"));

    runtime.close().await;
    platform.close().await;
    cleanup(&database, admin, &platform_role, &runtime_role).await;
}

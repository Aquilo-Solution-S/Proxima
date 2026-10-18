// Each integration-test binary independently includes this module via
// `mod common;`. Items unused by a particular binary would otherwise trip
// `dead_code` even though another binary uses them.
#![allow(dead_code)]

pub async fn split_roles(admin_url: &str) -> Result<(String, String), sqlx::Error> {
    let options = admin_url.parse::<sqlx::postgres::PgConnectOptions>()?;
    proxima_pg_testkit::split_role_urls(options.get_database().expect("fixture database")).await
}

pub fn schemas() -> &'static [&'static str] {
    &["proxima_core"]
}

pub async fn runtime_storage(
    runtime_url: &str,
    platform_url: &str,
) -> Result<proxima_storage_pg::PgStorage, Box<dyn std::error::Error>> {
    let migration = proxima_storage_pg::PgMigrationStorage::connect_with_config(
        platform_url,
        proxima_storage_pg::PgPoolConfig::from_env()?,
        proxima_storage_pg::PgTuning::from_env()?,
    )
    .await?;
    migration.run_migrations().await?;
    migration.initialize_store_number(0).await?;
    let platform = sqlx::PgPool::connect(platform_url).await?;
    sqlx::query("CREATE SCHEMA IF NOT EXISTS proxima_code")
        .execute(&platform)
        .await?;
    let scope = proxima_storage_pg::PgPlatformScope::new(
        sqlx::PgPool::connect(platform_url).await?,
        schemas(),
    )
    .await?;
    Ok(proxima_storage_pg::PgStorage::connect_with_config(
        runtime_url,
        proxima_storage_pg::PgPoolConfig::from_env()?,
        proxima_storage_pg::PgTuning::from_env()?,
        scope,
        0,
    )
    .await?)
}

pub async fn owner_access(
    runtime_url: &str,
    platform_url: &str,
) -> Result<proxima_storage_pg::PgOwnerAccessResolver, Box<dyn std::error::Error>> {
    let migration = proxima_storage_pg::PgMigrationStorage::connect_with_config(
        platform_url,
        proxima_storage_pg::PgPoolConfig::from_env()?,
        proxima_storage_pg::PgTuning::from_env()?,
    )
    .await?;
    migration.run_migrations().await?;
    let platform = sqlx::PgPool::connect(platform_url).await?;
    sqlx::query("CREATE SCHEMA IF NOT EXISTS proxima_code")
        .execute(&platform)
        .await?;
    let scope = proxima_storage_pg::PgPlatformScope::new(
        sqlx::PgPool::connect(platform_url).await?,
        schemas(),
    )
    .await?;
    Ok(proxima_storage_pg::PgOwnerAccessResolver::new(
        sqlx::PgPool::connect(runtime_url).await?,
        scope,
    ))
}

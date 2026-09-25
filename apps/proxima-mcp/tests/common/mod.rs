#![allow(dead_code)]

pub async fn split_roles(admin_url: &str) -> Result<(String, String), sqlx::Error> {
    let options = admin_url.parse::<sqlx::postgres::PgConnectOptions>()?;
    proxima_pg_testkit::split_role_urls(options.get_database().expect("fixture database")).await
}

pub fn schemas() -> &'static [&'static str] {
    #[cfg(feature = "code")]
    {
        &["proxima_core", "proxima_code"]
    }
    #[cfg(not(feature = "code"))]
    {
        &["proxima_core"]
    }
}

pub async fn runtime_storage(
    runtime_url: &str,
    platform_url: &str,
) -> Result<proxima_storage_pg::PgStorage, Box<dyn std::error::Error>> {
    let scope = proxima_storage_pg::PgPlatformScope::new(
        sqlx::PgPool::connect(platform_url).await?,
        schemas(),
    )
    .await?;
    Ok(proxima_storage_pg::PgStorage::connect(runtime_url)
        .await?
        .with_platform_scope(scope))
}

// Each integration-test binary independently includes this module via
// `mod common;`. Items unused by a particular binary would otherwise trip
// `dead_code` even though another binary uses them.

/// Read an env var gating a local-only or PG-backed e2e test.
///
/// Locally (or on any runner without `CI=true`), a missing/empty value
/// skips the test cleanly. Under `CI=true`, a missing value is a hard test
/// failure instead of a silent skip.
///
/// GitHub Actions exports `DATABASE_URL` / `PROXIMA_TEST_DATABASE_URL` and
/// runs the PG/OIDC e2e lane against live pgvector/pg18
/// (`.github/workflows/ci.yml`); this keeps non-GHA CI runners and local
/// `CI=true` runs from letting that lane go dark unnoticed.
pub fn require_env_or_skip(name: &str) -> Option<String> {
    require_env_or_skip_with(name, proxima_core::process_env)
}

/// Env-lookup-parameterized core, so unit tests can exercise every branch
/// without mutating real process env —
/// other `#[tokio::test]` functions in this same binary read `DATABASE_URL`
/// / `CI` concurrently, and cargo's default parallel test threads would
/// otherwise race a mutation against them.
fn require_env_or_skip_with(name: &str, lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    // Both reads go through `env_value` (whitespace is unset; `CI=" true "`
    // still means CI).
    match proxima_core::env_value(&lookup, name) {
        Some(value) => Some(value),
        None if proxima_core::env_value(&lookup, "CI").as_deref() == Some("true") => {
            panic!("{name} required under CI=true")
        }
        None => None,
    }
}

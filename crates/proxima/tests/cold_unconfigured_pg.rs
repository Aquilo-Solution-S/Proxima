use std::error::Error;
use std::time::Duration;

use proxima::flavor::{FlavorBundle, FlavorRegistry, FlavorRegistryError, NamedMigrator};
use proxima::{
    AppInfo, AuthPath, AuthzContext, BuiltProxima, FactWrite, FlavorApp, GetMemoriesReadRequest,
    GroupId, MemoryId, OwnerEraseOutcome, OwnerId, Proxima, Role, ServedSurface, UserId,
};
use proxima_core::{AgentNoteV1, ProtocolError};
use proxima_pg_testkit::{create_db, db_url, drop_db, split_role_urls, unique_db_name};
use uuid::Uuid;

type TestResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug)]
struct ForgetObservation {
    forget: Result<(), ProtocolError>,
    rows_after_forget: (i64, i64, i64),
    forgotten_after_forget: i64,
    readable_after_forget: Option<AgentNoteV1>,
    rows_after_restart: (i64, i64, i64),
    readable_after_restart: Option<AgentNoteV1>,
    lifecycle_before_forget: LifecycleState,
    lifecycle_after_forget: LifecycleState,
    lifecycle_after_restart: LifecycleState,
}

#[derive(Debug, PartialEq, Eq)]
struct LifecycleState {
    head: Option<HeadState>,
}

#[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
struct HeadState {
    handle: Uuid,
    kind: String,
    schema_id: String,
    owner_id: Uuid,
    t: Uuid,
}

/// #488: forget needs no object store. Without S3 it succeeds, keeps the row
/// and its typed payload in Postgres marked forgotten, and by-id reads return
/// nothing, before and after a restart. The head row stays on the forgotten
/// version because it is the series' only one.
#[tokio::test]
async fn forget_without_s3_marks_the_row_and_hides_it_across_restart() -> TestResult<()> {
    let database = unique_db_name("proxima_no_s3_cold");
    create_db(&database).await?;
    let result = tokio::time::timeout(Duration::from_mins(1), observe_forget(&database)).await;
    drop_db(&database).await?;
    let observed = result??;
    eprintln!("no-S3 forget/restart observation: {observed:?}");

    assert!(observed.forget.is_ok(), "forget needs no S3: {observed:?}");
    assert_eq!(
        observed.rows_after_forget,
        (1, 0, 1),
        "the admission and its typed payload stay, with no cooled stub: {observed:?}"
    );
    assert_eq!(observed.forgotten_after_forget, 1, "the row is marked");
    assert_eq!(
        observed.readable_after_forget, None,
        "a by-id read returns nothing once forgotten"
    );
    assert_eq!(observed.rows_after_restart, (1, 0, 1));
    assert_eq!(
        observed.readable_after_restart, None,
        "a new runtime does not read the forgotten memory either"
    );
    assert_eq!(
        observed.lifecycle_after_forget, observed.lifecycle_before_forget,
        "a series with one version keeps its head on it"
    );
    assert_eq!(
        observed.lifecycle_after_restart, observed.lifecycle_before_forget,
        "the head survives restart"
    );
    Ok(())
}

async fn observe_forget(database: &str) -> TestResult<ForgetObservation> {
    let admin_pool = sqlx::PgPool::connect(&db_url(database)).await?;
    let owner = OwnerId::from(GroupId::new(Uuid::now_v7()));
    let authz = host_context(owner, AuthPath::HostBearer);
    let initial = boot(database, owner).await?;
    let expected = note("cold durability");
    let written = initial
        .host()
        .engine()
        .ingest_fact(&authz, FactWrite::new(owner, "test/no-s3-cold", &expected))
        .await?;
    if read_note(&initial, &authz, written.memory_id)
        .await?
        .as_ref()
        != Some(&expected)
    {
        return Err("fixture must first read the exact typed Fact through the engine".into());
    }
    let lifecycle_before_forget = lifecycle(&admin_pool, written.handle).await?;
    if lifecycle_before_forget.head.as_ref().map(|head| head.t)
        != Some(written.memory_id.into_inner())
    {
        return Err("fixture must have a persisted admission head".into());
    }
    let forget = initial
        .host()
        .engine()
        .forget_memory(&authz, owner, written.memory_id)
        .await;
    let rows_after_forget = rows(&admin_pool, written.memory_id).await?;
    let forgotten_after_forget: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.memory WHERE t = $1 AND forgotten_at IS NOT NULL",
    )
    .bind(written.memory_id.into_inner())
    .fetch_one(&admin_pool)
    .await?;
    let readable_after_forget = read_note(&initial, &authz, written.memory_id).await?;
    let lifecycle_after_forget = lifecycle(&admin_pool, written.handle).await?;
    // Consume the entire first runtime and close its pool before composition
    // from scratch. No cold adapter or engine clone survives into the reboot.
    stop(initial).await;

    let restarted = boot(database, owner).await?;
    let rows_after_restart = rows(&admin_pool, written.memory_id).await?;
    let lifecycle_after_restart = lifecycle(&admin_pool, written.handle).await?;
    let readable_after_restart = read_note(&restarted, &authz, written.memory_id).await?;
    stop(restarted).await;
    Ok(ForgetObservation {
        forget,
        rows_after_forget,
        forgotten_after_forget,
        readable_after_forget,
        rows_after_restart,
        readable_after_restart,
        lifecycle_before_forget,
        lifecycle_after_forget,
        lifecycle_after_restart,
    })
}

#[tokio::test]
async fn database_only_content_remains_readable_and_erasable_without_s3() -> TestResult<()> {
    let database = unique_db_name("proxima_no_s3_read");
    create_db(&database).await?;
    let result =
        tokio::time::timeout(Duration::from_mins(1), database_only_control(&database)).await;
    drop_db(&database).await?;
    result??;
    Ok(())
}

async fn database_only_control(database: &str) -> TestResult<()> {
    let group = GroupId::new(Uuid::now_v7());
    let owner = OwnerId::from(group);
    let authz = host_context(owner, AuthPath::HostBearer);
    let initial = boot(database, owner).await?;
    register_group(&sqlx::PgPool::connect(&db_url(database)).await?, group).await?;
    let expected = note("database-only control");
    let written = initial
        .host()
        .engine()
        .ingest_fact(&authz, FactWrite::new(owner, "test/no-s3-read", &expected))
        .await?;
    let initial_read = read_note(&initial, &authz, written.memory_id).await?;
    stop(initial).await;
    let restarted = boot(database, owner).await?;
    let restarted_read = read_note(&restarted, &authz, written.memory_id).await?;
    let receipt = restarted
        .host()
        .engine()
        .erase_group_owner(&host_context(owner, AuthPath::System), group)
        .await?;
    let after_erase = rows(restarted.host().pool_for_tests(), written.memory_id).await?;
    let debt: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.cold_purge_pending")
        .fetch_one(restarted.host().pool_for_tests())
        .await?;
    stop(restarted).await;
    if initial_read.as_ref() != Some(&expected) || restarted_read.as_ref() != Some(&expected) {
        return Err("ordinary database-only reads must survive a fresh runtime".into());
    }
    if !matches!(
        receipt,
        OwnerEraseOutcome::Completed {
            cold_object_purge_pending: false,
            ..
        }
    ) || after_erase != (0, 0, 0)
        || debt != 0
    {
        return Err(format!(
            "database-only erase failed: {receipt:?}, {after_erase:?}, debt={debt}"
        )
        .into());
    }
    Ok(())
}

/// Core only: no flavor, no S3.
struct CoreApp;

impl FlavorBundle for CoreApp {
    fn register(_registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for CoreApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "cold-unconfigured-test",
            title: "Cold Unconfigured Test",
            version: "1",
        }
    }
}

async fn boot(database: &str, owner: OwnerId) -> TestResult<BuiltProxima> {
    let (runtime_url, platform_url) = split_role_urls(database).await?;
    Ok(Proxima::<CoreApp>::app()
        .database_url(runtime_url)
        .store_number(0)
        .platform_database_url(platform_url)
        .owner(owner)
        .served_surface(ServedSurface::All)
        .build()
        .await?)
}

async fn stop(runtime: BuiltProxima) {
    let pool = runtime.host().pool_for_tests().clone();
    runtime.shutdown().await;
    pool.close().await;
}

fn host_context(owner: OwnerId, path: AuthPath) -> AuthzContext {
    // This is the trusted host's resolved subject role, not a caller-chosen
    // group or the intentionally unauthorized single_owner(group) shortcut.
    proxima_core::test_fixtures::authenticated_context(AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(owner, Role::admin())],
        path,
    ))
    .narrowed_to_owner(owner)
    .expect("trusted host resolved this exact owner")
}

fn note(title: &str) -> AgentNoteV1 {
    AgentNoteV1 {
        note_id: Uuid::now_v7(),
        title: title.into(),
        body: "Durable typed content must outlive the process that admitted it.".into(),
        tags: vec!["restart".into(), "no-s3".into()],
        idempotency_key: None,
    }
}

async fn read_note(
    runtime: &BuiltProxima,
    authz: &AuthzContext,
    memory_id: MemoryId,
) -> TestResult<Option<AgentNoteV1>> {
    let response = runtime
        .host()
        .engine()
        .get_memories(
            authz,
            &GetMemoriesReadRequest {
                memory_ids: vec![memory_id],
            },
        )
        .await?;
    Ok(response
        .memories
        .first()
        .and_then(|memory| memory.payload.as_ref())
        .and_then(|payload| payload.downcast_ref::<AgentNoteV1>())
        .cloned())
}

async fn rows(pool: &sqlx::PgPool, memory_id: MemoryId) -> TestResult<(i64, i64, i64)> {
    Ok(sqlx::query_as(
        "SELECT (SELECT count(*) FROM proxima_core.memory WHERE t = $1),
                (SELECT count(*) FROM proxima_core.cooled WHERE t = $1),
                (SELECT count(*) FROM proxima_core.agent_note_v1 WHERE t = $1)",
    )
    .bind(memory_id.into_inner())
    .fetch_one(pool)
    .await?)
}

async fn lifecycle(pool: &sqlx::PgPool, handle: Uuid) -> TestResult<LifecycleState> {
    let head = sqlx::query_as(
        "SELECT handle, kind::text, schema_id, owner_id, t
           FROM proxima_core.memory_head WHERE handle = $1",
    )
    .bind(handle)
    .fetch_optional(pool)
    .await?;
    Ok(LifecycleState { head })
}

/// Register `group` as a Group, the way a host creates one before writing
/// under it: the group erase path admits only a registered Group.
async fn register_group(pool: &sqlx::PgPool, group: GroupId) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'group')
         ON CONFLICT (owner_id) DO NOTHING",
    )
    .bind(group.into_inner())
    .execute(pool)
    .await
    .map(drop)
}

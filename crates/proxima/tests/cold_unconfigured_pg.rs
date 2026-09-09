use std::error::Error;
use std::time::Duration;

use proxima::{
    AuthPath, AuthzContext, EmbedConfig, EmbeddedProxima, FactWrite, GetMemoriesReadRequest,
    GroupId, MemoryHydrationStatus, MemoryId, OwnerEraseOutcome, OwnerRef, ProximaBuilder, Role,
    UserId,
};
use proxima_core::{AgentNoteV1, ErrorCode, ProtocolError};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use uuid::Uuid;

type TestResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug)]
struct ForgetObservation {
    expected: AgentNoteV1,
    forget: Result<(), ProtocolError>,
    rows_after_forget: (i64, i64, i64),
    readable_after_forget: Option<AgentNoteV1>,
    rows_after_restart: (i64, i64, i64),
    hydration: Result<MemoryHydrationStatus, ProtocolError>,
    readable_after_restart: Option<AgentNoteV1>,
    lifecycle_before_forget: LifecycleState,
    lifecycle_after_forget: LifecycleState,
    lifecycle_after_restart: LifecycleState,
}

#[derive(Debug, PartialEq, Eq)]
struct LifecycleState {
    head: Option<HeadState>,
    announcements: Vec<AnnounceState>,
}

#[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
struct HeadState {
    handle: Uuid,
    kind: String,
    schema_id: String,
    owner_id: Uuid,
    t: Uuid,
}

#[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
struct AnnounceState {
    seq: Uuid,
    owner_id: Uuid,
    op: String,
    entity: String,
    handle: Uuid,
    t: Uuid,
}

#[tokio::test]
async fn forget_without_s3_preserves_content_across_restart() -> TestResult<()> {
    let database = unique_db_name("proxima_no_s3_cold");
    create_db(&database).await?;
    let result = tokio::time::timeout(Duration::from_mins(1), observe_forget(&database)).await;
    // Keep the failing safety assertion after cleanup, including on timeout.
    drop_db(&database).await?;
    let observed = result??;
    eprintln!("no-S3 forget/restart observation: {observed:?}");

    if let Err(error) = &observed.forget {
        assert_eq!(error.code, ErrorCode::Internal);
        assert_eq!(
            observed.rows_after_forget,
            (1, 0, 1),
            "refusal must retain the hot admission and typed payload: {observed:?}"
        );
        assert_eq!(
            observed.readable_after_forget.as_ref(),
            Some(&observed.expected),
            "refusal must leave the original content readable"
        );
        assert_eq!(observed.rows_after_restart, (1, 0, 1));
        assert_eq!(
            observed.lifecycle_after_forget, observed.lifecycle_before_forget,
            "refusal must not mutate the head or announce a lifecycle transition"
        );
        assert_eq!(
            observed.lifecycle_after_restart, observed.lifecycle_before_forget,
            "the unchanged head and announcements must survive restart"
        );
    }
    assert_eq!(
        observed.hydration.as_ref().ok(),
        Some(&if observed.forget.is_ok() {
            MemoryHydrationStatus::Hydrated
        } else {
            MemoryHydrationStatus::AlreadyHot
        }),
        "forget must either refuse safely or leave a durable cold object: {observed:?}"
    );
    assert_eq!(
        observed.readable_after_restart.as_ref(),
        Some(&observed.expected),
        "a new runtime must recover the complete original typed payload: {observed:?}"
    );
    Ok(())
}

async fn observe_forget(database: &str) -> TestResult<ForgetObservation> {
    let owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    let authz = host_context(owner, AuthPath::HostBearer);
    let initial = boot(database, owner).await?;
    let expected = note("cold durability");
    let written = initial
        .engine
        .ingest_fact(&authz, FactWrite::new(owner, "test/no-s3-cold", &expected))
        .await?;
    if read_note(&initial, &authz, written.memory_id)
        .await?
        .as_ref()
        != Some(&expected)
    {
        return Err("fixture must first read the exact typed Fact through the engine".into());
    }
    let lifecycle_before_forget = lifecycle(&initial, written.handle).await?;
    if lifecycle_before_forget.head.as_ref().map(|head| head.t)
        != Some(written.memory_id.into_inner())
        || lifecycle_before_forget.announcements.is_empty()
    {
        return Err("fixture must have a persisted admission head and announcement".into());
    }
    let forget = initial
        .engine
        .forget_memory(&authz, owner, written.memory_id)
        .await;
    let rows_after_forget = rows(&initial, written.memory_id).await?;
    let readable_after_forget = read_note(&initial, &authz, written.memory_id).await?;
    let lifecycle_after_forget = lifecycle(&initial, written.handle).await?;
    // Consume the entire first runtime and close its pool before composition
    // from scratch. No cold adapter or engine clone survives into the reboot.
    stop(initial).await;

    let restarted = boot(database, owner).await?;
    let rows_after_restart = rows(&restarted, written.memory_id).await?;
    let lifecycle_after_restart = lifecycle(&restarted, written.handle).await?;
    let hydration = restarted
        .engine
        .hydrate_memory(&authz, owner, written.memory_id)
        .await
        .map(|outcome| outcome.status);
    let readable_after_restart = read_note(&restarted, &authz, written.memory_id).await?;
    stop(restarted).await;
    Ok(ForgetObservation {
        expected,
        forget,
        rows_after_forget,
        readable_after_forget,
        rows_after_restart,
        hydration,
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
    let owner = OwnerRef::Group(group);
    let authz = host_context(owner, AuthPath::HostBearer);
    let initial = boot(database, owner).await?;
    let expected = note("database-only control");
    let written = initial
        .engine
        .ingest_fact(&authz, FactWrite::new(owner, "test/no-s3-read", &expected))
        .await?;
    let initial_read = read_note(&initial, &authz, written.memory_id).await?;
    stop(initial).await;
    let restarted = boot(database, owner).await?;
    let restarted_read = read_note(&restarted, &authz, written.memory_id).await?;
    let receipt = restarted
        .engine
        .erase_group_owner(&host_context(owner, AuthPath::System), group)
        .await?;
    let after_erase = rows(&restarted, written.memory_id).await?;
    let debt: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.cold_purge_pending")
        .fetch_one(restarted.pool_for_tests())
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

async fn boot(database: &str, owner: OwnerRef) -> TestResult<EmbeddedProxima> {
    Ok(ProximaBuilder::new(
        EmbedConfig {
            database_url: db_url(database),
            s3: None,
        },
        owner,
    )
    .boot()
    .await?)
}

async fn stop(runtime: EmbeddedProxima) {
    let pool = runtime.pool_for_tests().clone();
    runtime.engine.stop(runtime.handle);
    pool.close().await;
}

fn host_context(owner: OwnerRef, path: AuthPath) -> AuthzContext {
    // This is the trusted host's resolved subject role, not a caller-chosen
    // group or the intentionally unauthorized single_owner(group) shortcut.
    AuthzContext::for_subject_with_role(UserId::new(Uuid::now_v7()), [(owner, Role::admin())], path)
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
    runtime: &EmbeddedProxima,
    authz: &AuthzContext,
    memory_id: MemoryId,
) -> TestResult<Option<AgentNoteV1>> {
    let response = runtime
        .engine
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

async fn rows(runtime: &EmbeddedProxima, memory_id: MemoryId) -> TestResult<(i64, i64, i64)> {
    Ok(sqlx::query_as(
        "SELECT (SELECT count(*) FROM proxima_core.memory WHERE t = $1),
                (SELECT count(*) FROM proxima_core.cooled WHERE t = $1),
                (SELECT count(*) FROM proxima_core.agent_note_v1 WHERE t = $1)",
    )
    .bind(memory_id.into_inner())
    .fetch_one(runtime.pool_for_tests())
    .await?)
}

async fn lifecycle(runtime: &EmbeddedProxima, handle: Uuid) -> TestResult<LifecycleState> {
    let head = sqlx::query_as(
        "SELECT handle, kind::text, schema_id, owner_id, t
           FROM proxima_core.memory_head WHERE handle = $1",
    )
    .bind(handle)
    .fetch_optional(runtime.pool_for_tests())
    .await?;
    let announcements = sqlx::query_as(
        "SELECT seq, owner_id, op::text, entity::text, handle, t
           FROM proxima_core.announce WHERE handle = $1 ORDER BY seq",
    )
    .bind(handle)
    .fetch_all(runtime.pool_for_tests())
    .await?;
    Ok(LifecycleState {
        head,
        announcements,
    })
}

//! `ingest_fact_detached`: the Fact recording an upstream side effect lands
//! even when the request that made it is dropped, while a plain ingest
//! dropped the same way does not.

use std::sync::Arc;
use std::time::Duration;

use proxima::{EmbedConfig, EmbeddedProxima, FactWrite, ProximaBuilder, QueryRequest};
use proxima_core::{AgentNoteV1, AuthPath, AuthzContext, Owner, ToolCtx, ToolServices, UserId};
use proxima_pg_testkit::SplitRoleDb;
use proxima_storage_pg::PgPoolConfig;

async fn boot(db: &SplitRoleDb, owner: Owner) -> EmbeddedProxima {
    ProximaBuilder::new(
        EmbedConfig {
            database_url: db.runtime_url().to_owned(),
            platform_database_url: Some(db.platform_url().to_owned()),
            s3: None,
        },
        owner,
    )
    .pg_pool_config(PgPoolConfig::default())
    .bundle::<proxima_code::CodeFlavor>()
    .boot()
    .await
    .expect("split-role boot")
}

fn note(title: &str) -> AgentNoteV1 {
    AgentNoteV1 {
        note_id: uuid::Uuid::now_v7(),
        title: title.to_owned(),
        body: "the upstream side effect this records already happened".to_owned(),
        tags: Vec::new(),
        idempotency_key: None,
    }
}

async fn memory_count(booted: &EmbeddedProxima, authz: &AuthzContext) -> usize {
    booted
        .engine
        .query(authz, &QueryRequest::readable())
        .await
        .expect("owner query")
        .memories
        .len()
}

/// Whether `note` was committed: re-ingesting the same write replays it.
async fn committed(
    booted: &EmbeddedProxima,
    authz: &AuthzContext,
    owner: Owner,
    note: &AgentNoteV1,
) -> bool {
    booted
        .engine
        .ingest_fact(authz, FactWrite::new(owner, "test/detached", note))
        .await
        .expect("re-ingest")
        .idempotent_replay
}

#[tokio::test]
async fn a_dropped_request_still_records_its_fact() {
    let db = SplitRoleDb::create("proxima_detached_ingest", &[])
        .await
        .expect("PG required");
    let owner = Owner::Personal(UserId::new(uuid::Uuid::now_v7()));
    let booted = boot(&db, owner).await;
    let authz = proxima_core::test_fixtures::authenticated_context(AuthzContext::single_owner(
        &owner,
        AuthPath::HostBearer,
    ));
    let before = memory_count(&booted, &authz).await;

    // The control: a plain ingest polled once and dropped, as a handler is
    // when its client disconnects, rolls back.
    let plain = note("plain");
    {
        let mut request = Box::pin(
            booted
                .engine
                .ingest_fact(&authz, FactWrite::new(owner, "test/detached", &plain)),
        );
        assert!(futures::poll!(request.as_mut()).is_pending());
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(memory_count(&booted, &authz).await, before);
    assert!(
        !committed(&booted, &authz, owner, &plain).await,
        "a dropped plain ingest must not have landed"
    );

    // The same drop after `ingest_fact_detached` has been polled once.
    let detached = note("detached");
    {
        let mut request = Box::pin(booted.engine.ingest_fact_detached(
            &authz,
            FactWrite::new(owner, "test/detached", &detached),
            Duration::from_secs(30),
        ));
        assert!(futures::poll!(request.as_mut()).is_pending());
    }
    let landed = async {
        while memory_count(&booted, &authz).await < before + 2 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(20), landed)
        .await
        .expect("the detached write lands after its request is dropped");
    assert!(
        committed(&booted, &authz, owner, &detached).await,
        "the detached write is the one that landed"
    );

    // A missed deadline abandons the write and says so.
    let late = note("late");
    let error = booted
        .engine
        .ingest_fact_detached(
            &authz,
            FactWrite::new(owner, "test/detached", &late),
            Duration::ZERO,
        )
        .await
        .expect_err("a zero deadline cannot be met");
    assert!(error.message.contains("deadline"), "{}", error.message);
    assert!(!committed(&booted, &authz, owner, &late).await);

    // The tool-facing form resolves the engine and authorization from the
    // tool context.
    let registry = Arc::new(booted.engine.registry().clone());
    let ctx = ToolCtx::new(
        owner,
        authz.clone(),
        Arc::clone(&registry),
        ToolServices::default(),
    )
    .with_engine(Some(Arc::clone(&booted.engine)));
    let from_tool = note("from tool");
    let outcome = proxima::flavor::ingest_fact_detached(
        &ctx,
        FactWrite::new(owner, "test/detached", &from_tool),
        Duration::from_secs(30),
    )
    .await
    .expect("tool-context ingest");
    assert!(!outcome.idempotent_replay);
    let engineless = ToolCtx::new(owner, authz.clone(), registry, ToolServices::default());
    let error = proxima::flavor::ingest_fact_detached(
        &engineless,
        FactWrite::new(owner, "test/detached", &from_tool),
        Duration::from_secs(30),
    )
    .await
    .expect_err("no engine, no ingest");
    assert!(error.to_string().contains("no engine"), "{error}");
}

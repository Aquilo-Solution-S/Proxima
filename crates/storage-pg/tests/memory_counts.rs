//! `Engine::memory_counts` over Postgres: the current memories a caller can
//! read, by kind. One per series head for Fact, Abstraction and Perspective,
//! one per Goal; never one per version.

use std::sync::Arc;

use proxima_core::error::ErrorCode;
use proxima_core::read_models::MemoryCounts;
use proxima_core::storage_ports::{FactIngestPort, MemoryAuthoringPort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{
    AccessKind, AuthPath, AuthzContext, EdgeEndpoint, Engine, EntityKind, FlavorRegistry, GroupId,
    MemoryId, OwnerId, Role, SchemaId, SchemaVersion, UserId,
};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::verbs::forget::MemoryColdStore;
use proxima_storage_pg::verbs::goal_timeseries::{GoalWriteCommand, write_goal};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn memory(kind: &str, handle: Option<Uuid>) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new("core/test-fact-v1".to_string()),
        schema_version: SchemaVersion::new(1),
        handle,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: kind.into(),
    }
}

/// What [`seed`] wrote that the tests need to name again.
struct Seeded {
    /// A Fact nothing else points at, so it can be forgotten alone.
    forgettable: MemoryId,
}

/// Current memories `seed` leaves for one owner, with `extra_facts` more.
fn seeded_counts(extra_facts: u64) -> MemoryCounts {
    MemoryCounts {
        fact: 2 + extra_facts,
        abstraction: 1,
        perspective: 1,
        goal: 2,
    }
}

/// For `owner`: one forgettable Fact, `extra_facts` more, and a stateful Fact
/// series written twice (`2 + extra_facts` Fact heads over `3 + extra_facts`
/// rows); an Abstraction over that series and a Perspective over the
/// Abstraction; a Goal written Active then Achieved on one handle and a second
/// Paused Goal (two Goal heads over three rows).
async fn seed(pg: &PgStorage, owner: OwnerId, extra_facts: u64) -> TestResult<Seeded> {
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let forgettable = pg
        .ingest_fact_atomic(&permit, &memory("fact", None), None)
        .await?;
    for _ in 0..extra_facts {
        pg.ingest_fact_atomic(&permit, &memory("fact", None), None)
            .await?;
    }
    let series = pg
        .ingest_fact_atomic(&permit, &memory("fact", None), None)
        .await?;
    pg.ingest_fact_atomic(&permit, &memory("fact", Some(series.handle)), None)
        .await?;

    let mut abstraction = memory("abstraction", None);
    abstraction.additional_references =
        vec![EdgeEndpoint::memory(EntityKind::Fact, series.memory_id)];
    let abstraction = pg.ingest_fact_atomic(&permit, &abstraction, None).await?;
    let mut perspective = memory("perspective", None);
    perspective.refs = vec![abstraction.memory_id.into_inner()];
    pg.ingest_fact_atomic(&permit, &perspective, None).await?;

    let goal = |handle, state, request_id: &str| GoalWriteCommand {
        handle,
        schema_id: "core/task-v1".into(),
        title: request_id.into(),
        text: None,
        state,
        request_id: request_id.into(),
        close_fact_t: matches!(state, GoalState::Achieved).then(|| series.memory_id.into_inner()),
        assignment_t: None,
        dependency_t: Vec::new(),
        evidence_t: Vec::new(),
        wake_id: None,
        mint_write_act: false,
        write_act_t: None,
    };
    let mut tx = pg.pool_for_tests().begin().await?;
    let first = write_goal(&mut tx, &owner, &goal(None, GoalState::Active, "first")).await?;
    write_goal(
        &mut tx,
        &owner,
        &goal(Some(first.handle), GoalState::Achieved, "first-achieved"),
    )
    .await?;
    write_goal(&mut tx, &owner, &goal(None, GoalState::Paused, "second")).await?;
    tx.commit().await?;

    Ok(Seeded {
        forgettable: forgettable.memory_id,
    })
}

/// Rows of `proxima_core.memory` for one owner and kind: what counting the
/// versions instead of the heads would report.
async fn memory_rows(pg: &PgStorage, owner: OwnerId, kind: &str) -> TestResult<u64> {
    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.memory
          WHERE owner_id = $1 AND kind = $2::proxima_core.memory_kind",
    )
    .bind(owner.into_inner())
    .bind(kind)
    .fetch_one(pg.pool_for_tests())
    .await?;
    Ok(u64::try_from(rows)?)
}

fn engine(pg: &PgStorage) -> Engine {
    Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports())
}

fn personal() -> (UserId, OwnerId) {
    let user = UserId::new(Uuid::now_v7());
    (user, OwnerId::from(user))
}

fn group() -> OwnerId {
    OwnerId::from(GroupId::new(Uuid::now_v7()))
}

fn sum(a: MemoryCounts, b: MemoryCounts) -> MemoryCounts {
    MemoryCounts {
        fact: a.fact + b.fact,
        abstraction: a.abstraction + b.abstraction,
        perspective: a.perspective + b.perspective,
        goal: a.goal + b.goal,
    }
}

/// A migrated database and a storage onto it; the caller drops the database.
async fn fixture() -> TestResult<(String, PgStorage)> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(error) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {error}");
    }
    let pg = PgStorage::connect(&db_url(&db_name))
        .await?
        .with_cold(Arc::new(MemoryColdStore::default()));
    pg.run_before_owner_rls_migrations().await?;
    Ok((db_name, pg))
}

#[tokio::test]
async fn counts_are_heads_by_kind_and_follow_forget() {
    let (db_name, pg) = fixture().await.expect("fixture");
    let result: TestResult = async {
        let (_, owner) = personal();
        let seeded = seed(&pg, owner, 0).await?;
        let engine = engine(&pg);
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

        // Counting `proxima_core.memory` instead would report the versions.
        assert_eq!(memory_rows(&pg, owner, "fact").await?, 3);
        assert_eq!(engine.memory_counts(&authz).await?, seeded_counts(0));

        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        MemoryAuthoringPort::forget_memory(&pg, &permit, seeded.forgettable).await?;
        assert_eq!(
            engine.memory_counts(&authz).await?,
            MemoryCounts {
                fact: seeded_counts(0).fact - 1,
                ..seeded_counts(0)
            },
            "a forgotten Fact left its head and is not counted"
        );

        // A caller who owns nothing sees every kind, all zero.
        let (_, empty) = personal();
        let nobody = AuthzContext::single_owner(&empty, AuthPath::HostBearer);
        assert_eq!(
            engine.memory_counts(&nobody).await?,
            MemoryCounts::default()
        );
        let wire = serde_json::to_value(engine.memory_counts(&nobody).await?)?;
        assert_eq!(
            wire,
            serde_json::json!({"Fact": 0, "Abstraction": 0, "Perspective": 0, "Goal": 0})
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("memory counts test failed");
}

#[tokio::test]
async fn counts_leave_out_owners_the_caller_cannot_read() {
    let (db_name, pg) = fixture().await.expect("fixture");
    let result: TestResult = async {
        let (user, own) = personal();
        let (_, stranger) = personal();
        let (readable_group, closed_group) = (group(), group());
        seed(&pg, own, 0).await?;
        seed(&pg, readable_group, 1).await?;
        seed(&pg, closed_group, 2).await?;
        seed(&pg, stranger, 3).await?;
        let engine = engine(&pg);

        let alone = AuthzContext::for_subject(user, AuthPath::HostBearer);
        assert_eq!(engine.memory_counts(&alone).await?, seeded_counts(0));

        let member = AuthzContext::for_subject_with_role(
            user,
            [(readable_group, Role::viewer())],
            AuthPath::HostBearer,
        );
        assert_eq!(
            engine.memory_counts(&member).await?,
            sum(seeded_counts(0), seeded_counts(1)),
            "the personal owner and the group the caller is in; not the group \
             it is not in, and not another user's personal owner"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("owner scoping test failed");
}

#[tokio::test]
async fn a_context_that_reads_nothing_is_forbidden() {
    let (db_name, pg) = fixture().await.expect("fixture");
    let result: TestResult = async {
        let (_, owner) = personal();
        seed(&pg, owner, 0).await?;
        let denied = AuthzContext::denied_for_owner(&owner);
        let error = engine(&pg)
            .memory_counts(&denied)
            .await
            .expect_err("a denied context reads nothing");
        assert_eq!(error.code, ErrorCode::Forbidden, "{error}");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("denied context test failed");
}

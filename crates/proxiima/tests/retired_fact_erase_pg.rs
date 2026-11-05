//! A composed host drains a retired Fact through the bounded series inverse.
#![cfg(feature = "testkit")]
#![allow(clippy::too_many_lines)]

#[allow(
    dead_code,
    reason = "this fixture uses only the shared split-role database and pool helpers"
)]
#[path = "fixtures/example_engine.rs"]
mod example_engine;

use std::sync::Arc;

use proxiima::spiice::{PgSidecarRegistry, SpiiceBundle, register_core_pg_sidecars};
use proxiima_core::spiice::{
    CounterRule, EraseRule, ExportRule, ForgetRule, KeyShape, ProjectionDecl, RetiredFactContract,
    SchemaRef, SpiiceContract, Surface, TransferRule,
};
use proxiima_core::verbs::own_erase::{
    EraseMode, MAX_ERASE_SERIES_PER_CALL, SeriesEraseError, SeriesEraseRefusalKind, SeriesSelection,
};
use proxiima_core::{
    AgentNoteV1, AuthPath, AuthzContext, Engine, FactPayload, FactWrite, MemoryId, OwnerId,
    SchemaId, SpiiceRegistry, UserId,
};
use proxiima_example::ExampleSpiice;
use proxiima_storage_pg::{PgPlatformScope, PgStorage, PgTuning};
use sqlx::PgPool;
use uuid::Uuid;

const SPIICE: &str = "test-retired";
const SCHEMA: &str = "test-retired/removed-v1";
const TABLE: &str = "test_retired.removed_v1";
const DETAIL: &str = "test_retired.removed_detail";

const fn surface(table: &'static str, column: &'static str) -> Surface {
    Surface {
        table,
        key: KeyShape::MemoryT { column },
        owner_column: Some("owner_id"),
        transfer: TransferRule::Follow,
        erase: EraseRule::ByKey,
        export: ExportRule::Rows,
        forget: ForgetRule::DumpThenDelete,
        lexical_language_column: None,
        counter: CounterRule::Counted("sidecar_rows"),
        completeness: None,
    }
}
static CONTRACT: SpiiceContract = SpiiceContract {
    spiice_id: SPIICE,
    ordinal: 94,
    schemas: &[],
    retired_facts: &[RetiredFactContract {
        id: SchemaRef::new(SPIICE, "removed", 1),
        sidecar: surface(TABLE, "memory_t"),
        inverse_surfaces: &[surface(DETAIL, "fact_t")],
    }],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
    projection: ProjectionDecl::None {
        why: "a retired Fact has no search surface",
    },
};

struct Harness {
    engine: Engine,
    pool: PgPool,
    db: proxiima::testkit::SplitRoleDb,
}

async fn harness() -> Harness {
    let db = example_engine::migrated_db("proxiima_retired_erase").await;
    example_engine::initialize_store_number(&db).await.unwrap();
    let pool = example_engine::fixture_pool(&db.admin_url(), 2)
        .await
        .unwrap();
    // These rows represent a schema admitted before its payload was removed.
    // The non-default key proves retirement uses the frozen inverse metadata.
    sqlx::raw_sql(
        "CREATE SCHEMA test_retired AUTHORIZATION proxiima_test_platform;
         CREATE TABLE test_retired.removed_v1 (
             memory_t uuid PRIMARY KEY REFERENCES proxiima_core.memory(t),
             owner_id uuid NOT NULL REFERENCES proxiima_core.owners(owner_id),
             target_t uuid REFERENCES proxiima_core.memory(t),
             body text NOT NULL
         );
         CREATE TABLE test_retired.removed_detail (
             fact_t uuid PRIMARY KEY REFERENCES proxiima_core.memory(t),
             owner_id uuid NOT NULL REFERENCES proxiima_core.owners(owner_id),
             body text NOT NULL
         );
         ALTER TABLE test_retired.removed_detail OWNER TO proxiima_test_platform;
         ALTER TABLE test_retired.removed_v1 OWNER TO proxiima_test_platform;
         GRANT USAGE ON SCHEMA test_retired TO proxiima_test_runtime;
         GRANT SELECT, INSERT, UPDATE, DELETE ON test_retired.removed_v1, test_retired.removed_detail TO proxiima_test_runtime;
         INSERT INTO proxiima_core.spiice_surface(table_name, spiice_id)
             VALUES ('test_retired.removed_v1', 'test-retired'), ('test_retired.removed_detail', 'test-retired');",
    )
    .execute(&pool)
    .await
    .unwrap();
    let platform_pool = example_engine::fixture_pool(db.platform_url(), 4)
        .await
        .unwrap();
    sqlx::query("SELECT proxiima_core.install_owner_rls('test_retired', ARRAY['removed_v1', 'removed_detail'], '{}'::text[], '{}'::text[])")
        .execute(&platform_pool).await.unwrap();
    let scope = PgPlatformScope::new(
        platform_pool,
        &["proxiima_core", "proxiima_example", "test_retired"],
    )
    .await
    .unwrap();
    let mut registry = SpiiceRegistry::new();
    ExampleSpiice::register(&mut registry).unwrap();
    registry.try_add_contract(&CONTRACT).unwrap();
    let registry = registry.try_freeze().unwrap();
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    ExampleSpiice::register_pg_sidecars(&mut sidecars);
    let sidecars = sidecars.freeze_against(&registry).unwrap();
    let pg = PgStorage::connect_with_config(
        db.runtime_url(),
        example_engine::engine_pool_config(),
        PgTuning::default(),
        scope,
        0,
    )
    .await
    .unwrap()
    .with_sidecars(sidecars)
    .with_spiices(&registry);
    let engine = Engine::new(registry).with_storage_ports(Arc::new(pg).storage_ports());
    Harness { engine, pool, db }
}

fn owner() -> OwnerId {
    OwnerId::from(UserId::new(Uuid::now_v7()))
}

fn schema() -> SchemaId {
    SchemaId::new(SCHEMA.to_owned())
}

/// Seed whole historical series, including forgotten rows, without a payload type.
async fn seed(
    pool: &PgPool,
    owner: OwnerId,
    series: usize,
    versions: usize,
    forgotten: bool,
    target: Option<MemoryId>,
) -> Vec<MemoryId> {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE proxiima_core.memory, proxiima_core.goal IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO proxiima_core.owners(owner_id, kind) VALUES ($1, 'personal') ON CONFLICT DO NOTHING")
        .bind(owner.into_inner()).execute(&mut *tx).await.unwrap();
    let mut ids = Vec::new();
    for _ in 0..series {
        let handle = Uuid::now_v7();
        let ts: Vec<Uuid> = (0..versions).map(|_| Uuid::now_v7()).collect();
        sqlx::query("INSERT INTO proxiima_core.memory_head(handle, kind, schema_id, owner_id, t) VALUES ($1, 'fact', $2, $3, $4)")
            .bind(handle).bind(SCHEMA).bind(owner.into_inner()).bind(ts.last().unwrap())
            .execute(&mut *tx).await.unwrap();
        for t in ts {
            sqlx::query(
                "INSERT INTO proxiima_core.memory(handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via, forgotten_at)
                 VALUES ($1, $2, 'fact', $3, $4, ARRAY['test_retired.removed_v1'], uuid_extract_timestamp($2), $3, NULL,
                         CASE WHEN $5 THEN now() ELSE NULL END)",
            ).bind(handle).bind(t).bind(owner.into_inner()).bind(SCHEMA).bind(forgotten)
                .execute(&mut *tx).await.unwrap();
            sqlx::query("INSERT INTO test_retired.removed_v1(memory_t, owner_id, target_t, body) VALUES ($1, $2, $3, 'retired payload')")
                .bind(t).bind(owner.into_inner()).bind(target.map(MemoryId::into_inner))
                .execute(&mut *tx).await.unwrap();
            sqlx::query("INSERT INTO test_retired.removed_detail(fact_t, owner_id, body) VALUES ($1, $2, 'retired detail')")
                .bind(t).bind(owner.into_inner()).execute(&mut *tx).await.unwrap();
            ids.push(MemoryId::new(t));
        }
    }
    tx.commit().await.unwrap();
    ids.sort_unstable_by_key(|id| id.into_inner());
    ids
}

async fn footprint(pool: &PgPool, ids: &[MemoryId]) -> (i64, i64, i64) {
    let ts: Vec<_> = ids.iter().map(|id| id.into_inner()).collect();
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM proxiima_core.memory WHERE t = ANY($1)),
                (SELECT count(*) FROM test_retired.removed_v1 WHERE memory_t = ANY($1)),
                (SELECT count(*) FROM test_retired.removed_detail WHERE fact_t = ANY($1))",
    )
    .bind(ts)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn inventory_and_pages_reach_unretained_forgotten_and_older_versions() {
    let Harness { engine, pool, db } = harness().await;
    let first = owner();
    let second = owner();
    let first_ids = seed(&pool, first, MAX_ERASE_SERIES_PER_CALL + 1, 1, false, None).await;
    let second_ids = seed(&pool, second, 1, 2, true, None).await;
    let unrelated_owner = owner();
    let authz = proxiima_core::test_fixtures::authenticated_context(AuthzContext::for_subject(
        unrelated_owner.as_user(),
        AuthPath::HostBearer,
    ));
    let unrelated = engine
        .ingest_fact(
            &authz,
            FactWrite::new(
                unrelated_owner,
                "retired-fixture",
                &AgentNoteV1 {
                    note_id: Uuid::now_v7(),
                    title: "unrelated".into(),
                    body: "keep".into(),
                    tags: vec![],
                    idempotency_key: None,
                },
            ),
        )
        .await
        .unwrap()
        .memory_id;
    let retention: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxiima_core.retention WHERE owner_id = ANY($1)")
            .bind(vec![first.into_inner(), second.into_inner()])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(retention, 0, "retention is neither inventory nor consent");
    let (engine, system) = engine.into_system_authority();
    for selection in [
        SeriesSelection::Ids(vec![first_ids[0]]),
        SeriesSelection::AdmittedBefore {
            schema: schema(),
            cutoff: time::OffsetDateTime::UNIX_EPOCH,
        },
    ] {
        let mut ordinary = engine.system_unit_of_work(&system).unwrap();
        match ordinary
            .erase_own_series(SPIICE, first, selection, EraseMode::Erase)
            .await
        {
            Err(SeriesEraseError::Refused(refusal)) => {
                assert_eq!(refusal.kind, SeriesEraseRefusalKind::ForeignSchema);
            }
            other => panic!("ordinary series erase must not accept retired schemas: {other:?}"),
        }
    }
    let retirement: proxiima::RetiredFactErase<'_> = engine
        .retired_fact_erase(&system, SPIICE, &schema())
        .unwrap();
    let mut expected = vec![first, second];
    expected.sort_unstable_by_key(|id| id.into_inner());
    assert_eq!(retirement.owners(None).await.unwrap(), expected);
    assert_eq!(
        retirement.owners(Some(expected[0])).await.unwrap(),
        vec![expected[1]]
    );
    let page = retirement.erase_page(first).await.unwrap();
    assert_eq!(page.versions.len(), MAX_ERASE_SERIES_PER_CALL);
    assert!(page.more_remaining);
    assert_eq!(footprint(&pool, &first_ids).await, (1, 1, 1));
    let last = retirement.erase_page(first).await.unwrap();
    assert_eq!(last.versions.len(), 1);
    assert!(!last.more_remaining);
    assert_eq!(retirement.owners(None).await.unwrap(), vec![second]);
    let forgotten = retirement.erase_page(second).await.unwrap();
    assert_eq!(forgotten.versions, second_ids);
    assert!(!forgotten.more_remaining);
    assert!(retirement.owners(None).await.unwrap().is_empty());
    assert_eq!(footprint(&pool, &first_ids).await, (0, 0, 0));
    assert_eq!(footprint(&pool, &second_ids).await, (0, 0, 0));
    let remains: i64 = sqlx::query_scalar("SELECT count(*) FROM proxiima_core.memory WHERE t = $1")
        .bind(unrelated.into_inner())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remains, 1);
    pool.close().await;
    drop(db);
}

#[tokio::test]
async fn retirement_rejects_active_undeclared_foreign_targets_and_another_engines_authority() {
    let Harness { engine, pool, db } = harness().await;
    let (engine, system) = engine.into_system_authority();
    for (spiice, target) in [
        (SPIICE, SchemaId::new("test-retired/undeclared-v1".into())),
        (SPIICE, SchemaId::new(AgentNoteV1::SCHEMA_ID.into())),
        (proxiima_example::SPIICE_ID, schema()),
        ("unregistered", schema()),
    ] {
        assert!(engine.retired_fact_erase(&system, spiice, &target).is_err());
    }
    let (_, foreign_system) =
        Engine::new(SpiiceRegistry::new().try_freeze().unwrap()).into_system_authority();
    assert!(
        engine
            .retired_fact_erase(&foreign_system, SPIICE, &schema())
            .is_err()
    );
    pool.close().await;
    drop(db);
}

#[tokio::test]
async fn retired_reference_closure_refuses_another_owner_before_any_delete() {
    let Harness { engine, pool, db } = harness().await;
    let selected_owner = owner();
    let other_owner = owner();
    let selected = seed(&pool, selected_owner, 1, 1, false, None).await;
    let foreign = seed(&pool, other_owner, 1, 1, false, Some(selected[0])).await;
    let (engine, system) = engine.into_system_authority();
    let retirement = engine
        .retired_fact_erase(&system, SPIICE, &schema())
        .unwrap();
    match retirement.erase_page(selected_owner).await {
        Err(SeriesEraseError::Refused(refusal)) => {
            assert_eq!(refusal.kind, SeriesEraseRefusalKind::CrossOwner);
        }
        other => panic!("cross-owner closure must refuse: {other:?}"),
    }
    assert_eq!(footprint(&pool, &selected).await, (1, 1, 1));
    assert_eq!(footprint(&pool, &foreign).await, (1, 1, 1));
    pool.close().await;
    drop(db);
}

#[tokio::test]
async fn owner_inventory_is_bounded_and_cursor_reaches_the_next_page() {
    let Harness { engine, pool, db } = harness().await;
    let mut owners: Vec<_> = (0..=MAX_ERASE_SERIES_PER_CALL).map(|_| owner()).collect();
    owners.sort_unstable_by_key(|id| id.into_inner());
    let raw_owners: Vec<_> = owners.iter().map(|id| id.into_inner()).collect();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE proxiima_core.memory, proxiima_core.goal IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO proxiima_core.owners(owner_id, kind) SELECT owner, 'personal' FROM unnest($1::uuid[]) AS owner")
        .bind(&raw_owners).execute(&mut *tx).await.unwrap();
    let ts: Vec<Uuid> = sqlx::query_scalar(
        "INSERT INTO proxiima_core.memory_head(handle, kind, schema_id, owner_id, t)
         SELECT uuidv7(), 'fact', $2, owner, uuidv7() FROM unnest($1::uuid[]) AS owner
         RETURNING t",
    )
    .bind(&raw_owners)
    .bind(SCHEMA)
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO proxiima_core.memory(handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via, forgotten_at)
         SELECT h.handle, h.t, 'fact', h.owner_id, $2, ARRAY['test_retired.removed_v1'], uuid_extract_timestamp(h.t), h.owner_id, NULL, now()
         FROM proxiima_core.memory_head h WHERE h.t = ANY($1)",
    ).bind(&ts).bind(SCHEMA).execute(&mut *tx).await.unwrap();
    sqlx::query(
        "INSERT INTO test_retired.removed_v1(memory_t, owner_id, body)
         SELECT t, owner_id, 'retired payload' FROM proxiima_core.memory_head WHERE t = ANY($1)",
    )
    .bind(&ts)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO test_retired.removed_detail(fact_t, owner_id, body)
         SELECT t, owner_id, 'retired detail' FROM proxiima_core.memory_head WHERE t = ANY($1)",
    )
    .bind(&ts)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (engine, system) = engine.into_system_authority();
    let retirement = engine
        .retired_fact_erase(&system, SPIICE, &schema())
        .unwrap();
    let first = retirement.owners(None).await.unwrap();
    assert_eq!(first, owners[..MAX_ERASE_SERIES_PER_CALL]);
    let after = *first.last().unwrap();
    assert_eq!(
        retirement.owners(Some(after)).await.unwrap(),
        owners[MAX_ERASE_SERIES_PER_CALL..]
    );
    assert!(
        retirement
            .owners(Some(*owners.last().unwrap()))
            .await
            .unwrap()
            .is_empty()
    );
    pool.close().await;
    drop(db);
}

#[tokio::test]
async fn retired_reference_closure_refuses_a_foreign_spiices_schema() {
    let Harness { engine, pool, db } = harness().await;
    let selected_owner = owner();
    let selected = seed(&pool, selected_owner, 1, 1, false, None).await;
    let authz = proxiima_core::test_fixtures::authenticated_context(AuthzContext::for_subject(
        selected_owner.as_user(),
        AuthPath::HostBearer,
    ));
    let foreign = engine
        .ingest_fact(
            &authz,
            FactWrite::new(
                selected_owner,
                "retired-fixture",
                &proxiima_example::StampV1 {
                    mark: "foreign referrer".into(),
                },
            ),
        )
        .await
        .unwrap()
        .memory_id;
    sqlx::query("ALTER TABLE proxiima_example.stamp_v1 ADD COLUMN parent_t uuid REFERENCES proxiima_core.memory(t)")
        .execute(&pool).await.unwrap();
    sqlx::query("UPDATE proxiima_example.stamp_v1 SET parent_t = $1 WHERE t = $2")
        .bind(selected[0].into_inner())
        .bind(foreign.into_inner())
        .execute(&pool)
        .await
        .unwrap();
    let (engine, system) = engine.into_system_authority();
    let retirement = engine
        .retired_fact_erase(&system, SPIICE, &schema())
        .unwrap();
    match retirement.erase_page(selected_owner).await {
        Err(SeriesEraseError::Refused(refusal)) => {
            assert_eq!(refusal.kind, SeriesEraseRefusalKind::ForeignSchema);
        }
        other => panic!("foreign-schema closure must refuse: {other:?}"),
    }
    assert_eq!(footprint(&pool, &selected).await, (1, 1, 1));
    let remains: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM proxiima_core.memory WHERE t = $1),
                (SELECT count(*) FROM proxiima_example.stamp_v1 WHERE t = $1)",
    )
    .bind(foreign.into_inner())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(remains, (1, 1));
    pool.close().await;
    drop(db);
}

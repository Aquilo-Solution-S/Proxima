//! Retirement is a common post-composition, pre-serving boot step.
#![cfg(feature = "testkit")]
#![allow(clippy::too_many_lines)]

#[allow(dead_code, reason = "shared split-role fixture helpers")]
#[path = "fixtures/example_engine.rs"]
mod example_engine;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use proxiima::spiice::{
    CounterRule, EraseRule, ExportRule, ForgetRule, KeyShape, NamedMigrator, ProjectionDecl,
    SpiiceBundle, SpiiceContract, SpiiceRegistry, SpiiceRegistryError, SpiiceServiceError,
    SpiiceServices, SpiiceWorker, SpiiceWorkerContext, Surface, TransferRule,
};
use proxiima::{AppContext, AppInfo, Proxiima, ServedSurface, SpiiceApp};
use proxiima_core::spiice::{HostStateEraseDisposition, RetiredFactContract, SchemaRef};
use proxiima_core::storage_ports::{
    HostStateEraseReceipt, HostStateEraseRequest, HostStateEraseScope, HostStateEraseTableCount,
    HostStateExportReceipt, HostStateExportRequest, HostStateParticipantId, HostStateReply,
    HostStateRequest, HostStateWritePermit, StateSurfaceName,
};
use proxiima_core::verbs::own_erase::{MAX_ERASE_SERIES_PER_CALL, SeriesEraseError};
use proxiima_core::{
    AgentNoteV1, AuthError, AuthPath, Authenticator, AuthzContext, Credentials, FactWrite,
    MemoryId, OwnerId, StorageError, UserId,
};
use proxiima_example::ExampleSpiice;
use proxiima_storage_pg::{PgHostStateLifecyclePort, PgHostStateParticipant, PgHostStateSession};
use sqlx::{PgPool, Postgres, Transaction};
use tokio::sync::Notify;
use tokio::task::JoinSet;
use uuid::Uuid;

const SPIICE: &str = "test-boot-retired";
const SCHEMA: &str = "test-boot-retired/removed-v1";
const TABLE: &str = "test_boot_retired.retired_boot_v1";
const HOST_TABLE: &str = "test_boot_retired.retired_boot_host";
const PARTICIPANT: HostStateParticipantId = HostStateParticipantId::new("retired_boot");
const TABLES: &[StateSurfaceName] = &[StateSurfaceName::new(HOST_TABLE)];
const PATIENCE: Duration = Duration::from_secs(20);

const SIDECAR: Surface = Surface {
    table: TABLE,
    key: KeyShape::MemoryT { column: "memory_t" },
    owner_column: Some("owner_id"),
    transfer: TransferRule::Follow,
    erase: EraseRule::ByKey,
    export: ExportRule::Rows,
    forget: ForgetRule::DumpThenDelete,
    lexical_language_column: None,
    counter: CounterRule::Counted("sidecar_rows"),
    completeness: None,
};
const HOST_SURFACE: Surface = Surface {
    table: HOST_TABLE,
    key: KeyShape::OwnerId,
    transfer: TransferRule::RetainAtSource {
        why: "host-owned state",
    },
    erase: EraseRule::HostState {
        whole_owner: HostStateEraseDisposition::Erase,
        source: HostStateEraseDisposition::Erase,
        exact_fact: HostStateEraseDisposition::Erase,
    },
    export: ExportRule::Allowlist(&["fact_id", "owner_id"]),
    forget: ForgetRule::Keep {
        why: "the lifecycle callback owns this state",
    },
    ..SIDECAR
};
static CONTRACT: SpiiceContract = SpiiceContract {
    spiice_id: SPIICE,
    ordinal: 95,
    schemas: &[],
    retired_facts: &[RetiredFactContract {
        id: SchemaRef::new(SPIICE, "removed", 1),
        sidecar: SIDECAR,
        inverse_surfaces: &[],
    }],
    state_surfaces: &[HOST_SURFACE],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
    projection: ProjectionDecl::None {
        why: "inverse-only fixture",
    },
    evolutions: &[],
};

proxiima::spiice::proxiima_spiice! {
    name = "test-boot-retired",
    contract = &CONTRACT,
}

struct BootApp;
impl SpiiceBundle for BootApp {
    fn register(registry: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        register(registry)
    }
    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
    fn spawn_workers(ctx: &SpiiceWorkerContext) -> Vec<SpiiceWorker> {
        let probe = ctx.service::<Probe>().unwrap();
        assert!(probe.0.composed.load(Ordering::SeqCst));
        assert_eq!(
            probe.0.remaining.load(Ordering::SeqCst),
            0,
            "no producer can start while the drain has work"
        );
        probe.0.workers.fetch_add(1, Ordering::SeqCst);
        Vec::new()
    }
}
impl SpiiceApp for BootApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: SPIICE,
            title: "Boot retirement",
            version: "1",
        }
    }
    fn services(ctx: &AppContext) -> Result<SpiiceServices, SpiiceServiceError> {
        ctx.host()
            .services()
            .get::<Probe>()
            .unwrap()
            .0
            .composed
            .store(true, Ordering::SeqCst);
        Ok(SpiiceServices::new())
    }
}

type App = (BootApp, ExampleSpiice);

#[derive(Default)]
struct ProbeState {
    composed: AtomicBool,
    remaining: AtomicUsize,
    workers: AtomicUsize,
    calls: AtomicUsize,
    fail_call: AtomicUsize,
    gate: Mutex<Option<i64>>,
    entered: Notify,
    selections: Mutex<Vec<Vec<MemoryId>>>,
}
#[derive(Clone, Default)]
struct Probe(Arc<ProbeState>);

#[async_trait::async_trait]
impl PgHostStateParticipant for Probe {
    fn participant_id(&self) -> HostStateParticipantId {
        PARTICIPANT
    }
    fn declared_tables(&self) -> &'static [StateSurfaceName] {
        TABLES
    }
    fn lifecycle_port(&self) -> Option<Arc<dyn PgHostStateLifecyclePort>> {
        Some(Arc::new(self.clone()))
    }
    async fn apply(
        &self,
        _: &mut PgHostStateSession<'_, '_>,
        _: &HostStateWritePermit,
        _: HostStateRequest,
    ) -> Result<HostStateReply, StorageError> {
        unreachable!("boot only invokes the lifecycle inverse")
    }
}

fn storage_error(error: &sqlx::Error) -> StorageError {
    StorageError::Internal(error.to_string())
}

#[async_trait::async_trait]
impl PgHostStateLifecyclePort for Probe {
    fn participant_id(&self) -> HostStateParticipantId {
        PARTICIPANT
    }
    fn declared_tables(&self) -> &'static [StateSurfaceName] {
        TABLES
    }
    async fn erase(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        request: HostStateEraseRequest,
    ) -> Result<HostStateEraseReceipt, StorageError> {
        assert!(
            self.0.composed.load(Ordering::SeqCst),
            "host services compose first"
        );
        assert_eq!(request.scope(), &HostStateEraseScope::ExactFacts);
        assert!(
            request.selected_fact_ids().len()
                <= proxiima_core::verbs::own_erase::MAX_ERASE_VERSIONS_PER_CALL
        );
        let ids: Vec<_> = request
            .selected_fact_ids()
            .iter()
            .map(|id| id.into_inner())
            .collect();
        let role: (String, bool, bool) = sqlx::query_as(
            "SELECT current_user::text, rolsuper, rolbypassrls FROM pg_roles WHERE rolname = current_user")
            .fetch_one(&mut **tx).await.map_err(|error| storage_error(&error))?;
        // Series erase already uses the runtime's validated maintenance pool
        // to reach original-owner publication copies; this is not a migrator.
        assert_eq!(role, ("proxiima_test_platform".to_owned(), false, false));
        let scope: String = sqlx::query_scalar("SELECT current_setting('app.proxiima_scope')")
            .fetch_one(&mut **tx)
            .await
            .map_err(|error| storage_error(&error))?;
        assert_eq!(scope, "platform");
        let fenced: bool = sqlx::query_scalar(
            "SELECT bool_and(EXISTS (SELECT 1 FROM pg_locks l
              WHERE l.locktype = 'advisory' AND l.pid = pg_backend_pid() AND l.granted
                AND l.mode = 'ExclusiveLock' AND l.objsubid = 1
                AND l.classid::bigint = ((k.key >> 32) & 4294967295)
                AND l.objid::bigint = (k.key & 4294967295)))
             FROM (SELECT hashtextextended('proxiima-forget:' || id::text, 0) AS key
                     FROM unnest($1::uuid[]) id) k",
        )
        .bind(&ids)
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| storage_error(&error))?;
        assert!(fenced, "the exact-Fact lifecycle locks precede the inverse");
        let gate = self.0.gate.lock().unwrap().take();
        if let Some(key) = gate {
            self.0.entered.notify_one();
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(key)
                .execute(&mut **tx)
                .await
                .map_err(|error| storage_error(&error))?;
        }
        let deleted = sqlx::query("DELETE FROM test_boot_retired.retired_boot_host WHERE fact_id = ANY($1) AND owner_id = $2")
            .bind(&ids).bind(request.owner().into_inner()).execute(&mut **tx)
            .await.map_err(|error| storage_error(&error))?.rows_affected();
        assert_eq!(
            usize::try_from(deleted).unwrap(),
            ids.len(),
            "each inverse runs once per Fact"
        );
        let call = self.0.calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.0
            .selections
            .lock()
            .unwrap()
            .push(request.selected_fact_ids().to_vec());
        if self.0.fail_call.load(Ordering::SeqCst) == call {
            return Err(StorageError::Internal(
                "injected retirement inverse refusal".into(),
            ));
        }
        self.0.remaining.fetch_sub(ids.len(), Ordering::SeqCst);
        Ok(HostStateEraseReceipt {
            participant: request.participant(),
            owner: request.owner(),
            scope: request.scope().clone(),
            selection: request.selection().clone(),
            counts: vec![HostStateEraseTableCount {
                table: TABLES[0],
                deleted,
                scrubbed: 0,
            }],
        })
    }
    async fn export(
        &self,
        _: &mut Transaction<'_, Postgres>,
        _: HostStateExportRequest,
    ) -> Result<HostStateExportReceipt, StorageError> {
        unreachable!("boot does not export")
    }
}

struct Auth(OwnerId);
#[async_trait::async_trait]
impl Authenticator for Auth {
    async fn authenticate(&self, _: &Credentials) -> Result<AuthzContext, AuthError> {
        Ok(AuthzContext::for_subject(
            self.0.as_user(),
            AuthPath::HostBearer,
        ))
    }
}

struct Fixture {
    pool: PgPool,
    owner: OwnerId,
    unrelated: MemoryId,
    db: proxiima::testkit::SplitRoleDb,
}
impl Fixture {
    async fn new() -> Self {
        let db = example_engine::migrated_db("proxiima_retired_boot").await;
        example_engine::initialize_store_number(&db).await.unwrap();
        let pool = example_engine::fixture_pool(&db.admin_url(), 3)
            .await
            .unwrap();
        sqlx::raw_sql(
            "CREATE SCHEMA test_boot_retired AUTHORIZATION proxiima_test_platform;
             GRANT USAGE ON SCHEMA test_boot_retired TO proxiima_test_runtime;
             CREATE TABLE test_boot_retired.retired_boot_v1 (
                memory_t uuid PRIMARY KEY REFERENCES proxiima_core.memory(t),
                owner_id uuid NOT NULL REFERENCES proxiima_core.owners(owner_id), body text NOT NULL);
             CREATE TABLE test_boot_retired.retired_boot_host (fact_id uuid PRIMARY KEY, owner_id uuid NOT NULL);
             ALTER TABLE test_boot_retired.retired_boot_v1 OWNER TO proxiima_test_platform;
             ALTER TABLE test_boot_retired.retired_boot_host OWNER TO proxiima_test_platform;
             GRANT SELECT, INSERT, UPDATE, DELETE ON test_boot_retired.retired_boot_v1, test_boot_retired.retired_boot_host TO proxiima_test_runtime;
             INSERT INTO proxiima_core.spiice_surface(table_name, spiice_id)
                 VALUES ('test_boot_retired.retired_boot_v1', 'test-boot-retired'),
                        ('test_boot_retired.retired_boot_host', 'test-boot-retired');")
            .execute(&pool).await.unwrap();
        let platform = example_engine::fixture_pool(db.platform_url(), 2)
            .await
            .unwrap();
        for table in ["retired_boot_v1", "retired_boot_host"] {
            sqlx::query(
                "SELECT proxiima_core.install_owner_rls_table('test_boot_retired', $1, 'owner_id')",
            )
            .bind(table)
            .execute(&platform)
            .await
            .unwrap();
        }
        platform.close().await;
        for artifact in [
            proxiima_storage_pg::integrity::declaration_trigger(TABLE, "memory_t").unwrap(),
            proxiima_storage_pg::integrity::presence_trigger(TABLE, "memory_t").unwrap(),
            proxiima_storage_pg::integrity::delete_guard_trigger(TABLE, "memory_t").unwrap(),
            proxiima_storage_pg::integrity::key_repoint_trigger(TABLE, "memory_t").unwrap(),
        ] {
            // SQL-POLICY: generated integrity DDL over fixed fixture identifiers.
            sqlx::raw_sql(sqlx::AssertSqlSafe(artifact.forward))
                .execute(&pool)
                .await
                .unwrap();
        }
        let owner = new_owner();
        let engine = example_engine::compose(db.runtime_url(), db.platform_url(), |registry| {
            BootApp::register(registry).unwrap();
        })
        .await;
        let authz = proxiima_core::test_fixtures::authenticated_context(AuthzContext::for_subject(
            owner.as_user(),
            AuthPath::HostBearer,
        ));
        let unrelated = engine
            .ingest_fact(
                &authz,
                FactWrite::new(
                    owner,
                    "boot-fixture",
                    &AgentNoteV1 {
                        note_id: Uuid::now_v7(),
                        title: "keep".into(),
                        body: "keep".into(),
                        tags: vec![],
                        idempotency_key: None,
                    },
                ),
            )
            .await
            .unwrap()
            .memory_id;
        sqlx::query(
            "INSERT INTO test_boot_retired.retired_boot_host(fact_id, owner_id) VALUES ($1, $2)",
        )
        .bind(unrelated.into_inner())
        .bind(owner.into_inner())
        .execute(&pool)
        .await
        .unwrap();
        Self {
            pool,
            owner,
            unrelated,
            db,
        }
    }
    fn app(&self, probe: &Probe, skip: bool) -> Proxiima<App> {
        let app = Proxiima::<App>::app()
            .database_url(self.db.runtime_url())
            .platform_database_url(self.db.platform_url())
            .store_number(0)
            .owner(self.owner)
            .authenticator(Arc::new(Auth(self.owner)))
            .served_surface(ServedSurface::All)
            .pg_pool_config(example_engine::engine_pool_config())
            .services(SpiiceServices::with(probe.clone()))
            .host_state_participant(Arc::new(probe.clone()));
        let _ = skip;
        app
    }
    async fn seed(
        &self,
        owner: OwnerId,
        series: usize,
        versions: usize,
        forgotten: bool,
    ) -> Vec<MemoryId> {
        let mut tx = self.pool.begin().await.unwrap();
        sqlx::query(
            "LOCK TABLE proxiima_core.memory, proxiima_core.goal IN SHARE ROW EXCLUSIVE MODE",
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query("INSERT INTO proxiima_core.owners(owner_id, kind) VALUES ($1, 'personal') ON CONFLICT DO NOTHING")
            .bind(owner.into_inner()).execute(&mut *tx).await.unwrap();
        let mut ids = Vec::new();
        for _ in 0..series {
            let handle = Uuid::now_v7();
            let ts: Vec<_> = (0..versions).map(|_| Uuid::now_v7()).collect();
            sqlx::query("INSERT INTO proxiima_core.memory_head(handle, kind, schema_id, owner_id, t) VALUES ($1, 'fact', $2, $3, $4)")
                .bind(handle).bind(SCHEMA).bind(owner.into_inner()).bind(ts.last().unwrap()).execute(&mut *tx).await.unwrap();
            for t in ts {
                sqlx::query("INSERT INTO proxiima_core.memory(handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via, forgotten_at)
                    VALUES ($1, $2, 'fact', $3, $4, ARRAY['test_boot_retired.retired_boot_v1'], uuid_extract_timestamp($2), $3, NULL, CASE WHEN $5 THEN now() ELSE NULL END)")
                    .bind(handle).bind(t).bind(owner.into_inner()).bind(SCHEMA).bind(forgotten).execute(&mut *tx).await.unwrap();
                sqlx::query("INSERT INTO test_boot_retired.retired_boot_v1(memory_t, owner_id, body) VALUES ($1, $2, 'retired payload')")
                    .bind(t).bind(owner.into_inner()).execute(&mut *tx).await.unwrap();
                sqlx::query("INSERT INTO test_boot_retired.retired_boot_host(fact_id, owner_id) VALUES ($1, $2)")
                    .bind(t).bind(owner.into_inner()).execute(&mut *tx).await.unwrap();
                sqlx::query("INSERT INTO proxiima_core.embedding_heads(entity_id, model_id, dim, embedding_version, owner_id) VALUES ($1, 'boot-test', 384, 1, $2)")
                    .bind(t).bind(owner.into_inner()).execute(&mut *tx).await.unwrap();
                sqlx::query("INSERT INTO proxiima_core.embeddings(entity_id, model_id, dim, embedding_version, owner_id, vec) VALUES ($1, 'boot-test', 384, 1, $2, $3::text::vector)")
                    .bind(t).bind(owner.into_inner()).bind(format!("[{}]", vec!["0"; 384].join(",")))
                    .execute(&mut *tx).await.unwrap();
                ids.push(MemoryId::new(t));
            }
        }
        tx.commit().await.unwrap();
        ids
    }
    async fn footprint(&self, ids: &[MemoryId]) -> (i64, i64, i64, i64) {
        let ts: Vec<_> = ids.iter().map(|id| id.into_inner()).collect();
        sqlx::query_as(
            "SELECT
            (SELECT count(*) FROM proxiima_core.memory WHERE t = ANY($1)),
            (SELECT count(*) FROM test_boot_retired.retired_boot_v1 WHERE memory_t = ANY($1)),
            (SELECT count(*) FROM test_boot_retired.retired_boot_host WHERE fact_id = ANY($1)),
            (SELECT count(*) FROM proxiima_core.embedding_heads WHERE entity_id = ANY($1)) + (SELECT count(*) FROM proxiima_core.embeddings WHERE entity_id = ANY($1))",
        )
        .bind(ts)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }
    async fn assert_drained(&self, ids: &[MemoryId]) {
        assert_eq!(self.footprint(ids).await, (0, 0, 0, 0));
        assert_eq!(self.footprint(&[self.unrelated]).await, (1, 0, 1, 0));
    }
}
fn new_owner() -> OwnerId {
    OwnerId::from(UserId::new(Uuid::now_v7()))
}

// More than one bounded page, physical embeddings and forgotten versions
// deliberately exercise real PG commits; this integration test can exceed 5 s.
#[tokio::test]
async fn build_drains_bounded_pages_and_forgotten_versions_before_returning_or_starting_workers() {
    let fixture = Fixture::new().await;
    let mut ids = fixture
        .seed(fixture.owner, MAX_ERASE_SERIES_PER_CALL + 1, 1, false)
        .await;
    ids.extend(fixture.seed(new_owner(), 1, 2, true).await);
    let count = i64::try_from(ids.len()).unwrap();
    assert_eq!(
        fixture.footprint(&ids).await,
        (count, count, count, 2 * count)
    );
    let probe = Probe::default();
    probe.0.remaining.store(ids.len(), Ordering::SeqCst);
    let built = fixture.app(&probe, false).build().await.unwrap();
    fixture.assert_drained(&ids).await;
    assert_eq!(probe.0.calls.load(Ordering::SeqCst), ids.len());
    assert_eq!(probe.0.workers.load(Ordering::SeqCst), 1);
    built.shutdown().await;
    // A fresh empty store is an ordinary no-op; no completion ledger is needed.
    let again = Probe::default();
    let built = fixture.app(&again, false).build().await.unwrap();
    assert_eq!(again.0.calls.load(Ordering::SeqCst), 0);
    built.shutdown().await;
    fixture.pool.close().await;
}

#[tokio::test]
async fn run_with_out_of_band_migrations_uses_validated_split_role_pools_before_listening() {
    let fixture = Fixture::new().await;
    let ids = fixture.seed(fixture.owner, 1, 2, true).await;
    let probe = Probe::default();
    probe.0.remaining.store(ids.len(), Ordering::SeqCst);
    // Occupy the future listener address until the inverse has entered. A
    // listener-before-drain implementation would fail bind and never reach
    // this positive marker; no timing-based negative probe is needed.
    let runtime = example_engine::fixture_pool(fixture.db.runtime_url(), 1)
        .await
        .unwrap();
    let role: (String, bool, bool) = sqlx::query_as(
        "SELECT current_user::text, rolsuper, rolbypassrls FROM pg_roles WHERE rolname = current_user")
        .fetch_one(&runtime).await.unwrap();
    assert_eq!(role, ("proxiima_test_runtime".to_owned(), false, false));
    runtime.close().await;
    let reserved = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = reserved.local_addr().unwrap();
    let key = 8_521_601_i64;
    *probe.0.gate.lock().unwrap() = Some(key);
    let mut gate = fixture.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(key)
        .execute(&mut *gate)
        .await
        .unwrap();
    let mut tasks = JoinSet::new();
    tasks.spawn(fixture.app(&probe, true).mcp_bind(address).run());
    tokio::time::timeout(PATIENCE, async {
        tokio::select! {
            () = probe.0.entered.notified() => {},
            early = tasks.join_next() => panic!("boot ended before the inverse: {early:?}"),
        }
    })
    .await
    .unwrap();
    assert_eq!(probe.0.workers.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.footprint(&ids).await, (2, 2, 2, 4));
    drop(reserved);
    gate.commit().await.unwrap();
    let running = tokio::time::timeout(PATIENCE, tasks.join_next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .unwrap();
    fixture.assert_drained(&ids).await;
    assert!(running.mcp_addr().is_some());
    running.shutdown().await;
    fixture.pool.close().await;
}

// This integration test erases a whole 256-Fact page before injecting a
// second-page failure and booting again, so real PG work can exceed 5 s.
#[tokio::test]
async fn callback_failure_rolls_back_its_page_and_restart_derives_only_the_remaining_work() {
    let fixture = Fixture::new().await;
    let ids = fixture
        .seed(fixture.owner, MAX_ERASE_SERIES_PER_CALL + 1, 1, false)
        .await;
    let failed = Probe::default();
    failed.0.remaining.store(ids.len(), Ordering::SeqCst);
    // The inverse runs once per physical Fact inside a page transaction.
    // Refuse the first Fact of page two, after the full first page committed.
    failed
        .0
        .fail_call
        .store(MAX_ERASE_SERIES_PER_CALL + 1, Ordering::SeqCst);
    let error = fixture.app(&failed, true).build().await.unwrap_err();
    let proxiima::ProxiimaError::Engine(source) = &error else {
        panic!("expected a typed engine failure, got {error:?}");
    };
    assert!(matches!(
        source.downcast_ref::<SeriesEraseError>(),
        Some(SeriesEraseError::Protocol(_))
    ));
    assert!(
        error
            .to_string()
            .contains("injected retirement inverse refusal")
    );
    assert_eq!(failed.0.workers.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.footprint(&ids).await, (1, 1, 1, 2));
    let expected = failed.0.selections.lock().unwrap().last().unwrap().clone();
    let restarted = Probe::default();
    restarted
        .0
        .remaining
        .store(expected.len(), Ordering::SeqCst);
    let built = fixture.app(&restarted, true).build().await.unwrap();
    assert_eq!(*restarted.0.selections.lock().unwrap(), vec![expected]);
    fixture.assert_drained(&ids).await;
    built.shutdown().await;
    fixture.pool.close().await;
}

#[tokio::test]
async fn concurrent_boots_serialize_at_the_existing_lifecycle_fence() {
    let fixture = Fixture::new().await;
    let ids = fixture.seed(fixture.owner, 1, 1, false).await;
    let first = Probe::default();
    first.0.remaining.store(1, Ordering::SeqCst);
    let second = Probe::default();
    // The winner can be observed from either runtime; the SQL footprint is authoritative.
    let key = 8_521_600;
    *first.0.gate.lock().unwrap() = Some(key);
    let mut gate = fixture.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(key)
        .execute(&mut *gate)
        .await
        .unwrap();
    let mut tasks = JoinSet::new();
    let first_task = tasks.spawn(fixture.app(&first, true).build());
    tokio::time::timeout(PATIENCE, async {
        tokio::select! {
            () = first.0.entered.notified() => {},
            early = tasks.join_next() => panic!("boot ended before the inverse: {early:?}"),
        }
    })
    .await
    .unwrap();
    let second_task = tasks.spawn(fixture.app(&second, true).build());
    tokio::time::timeout(PATIENCE, async {
        loop {
            let waiting: bool = sqlx::query_scalar("WITH k AS (SELECT hashtextextended($1, 0) AS key)
                SELECT EXISTS (SELECT 1 FROM pg_locks l CROSS JOIN k WHERE l.locktype = 'advisory' AND NOT l.granted
                  AND l.classid::bigint = ((k.key >> 32) & 4294967295) AND l.objid::bigint = (k.key & 4294967295))")
                .bind(format!("proxiima-owner-fence:{}", fixture.owner.into_inner()))
                .fetch_one(&fixture.pool).await.unwrap();
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert!(!first_task.is_finished());
    assert!(!second_task.is_finished());
    gate.commit().await.unwrap();
    let (first_built, second_built) = tokio::time::timeout(PATIENCE, async {
        (
            tasks.join_next().await.unwrap().unwrap().unwrap(),
            tasks.join_next().await.unwrap().unwrap().unwrap(),
        )
    })
    .await
    .unwrap();
    fixture.assert_drained(&ids).await;
    assert_eq!(
        first.0.calls.load(Ordering::SeqCst) + second.0.calls.load(Ordering::SeqCst),
        1
    );
    first_built.shutdown().await;
    second_built.shutdown().await;
    fixture.pool.close().await;
}

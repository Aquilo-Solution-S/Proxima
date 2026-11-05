//! A spiice's scope erase runs on the production Engine's erase verb, which
//! carries the full frozen host lifecycle registry.
//!
//! Ungated: the spiice is the example test fixture, composed with a host
//! lifecycle participant by this test's own `SpiiceApp`.
#![allow(clippy::too_many_lines)]

mod common;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use proxiima::spiice::{
    CounterRule, EraseRule, ExportRule, ForgetRule, KeyShape, NamedMigrator, PgSidecarRegistry,
    ProjectionDecl, SpiiceBundle, SpiiceContract, SpiiceRegistry, SpiiceRegistryError, Surface,
    TransferRule,
};
use proxiima::{AppInfo, Proxiima, SpiiceApp};
use proxiima_core::owner_inverse::{EraseAuthorization, OwnerEraseOutcome, OwnerEraseTarget};
use proxiima_core::storage_ports::{
    HostStateEraseReceipt, HostStateEraseRequest, HostStateEraseScope, HostStateEraseTableCount,
    HostStateExportReceipt, HostStateExportRequest, HostStateParticipantId, HostStateReply,
    HostStateRequest, HostStateWritePermit, OwnerInversePort, StateSurfaceName,
};
use proxiima_core::{
    AuthError, AuthPath, Authenticator, AuthzContext, Credentials, FactPayload, MemoryId, OwnerId,
    OwnerRoles, SourceId, SpiiceServiceError, SpiiceServices, StorageError, UserId,
};
use proxiima_example::testkit::{erase_notebook_with_scope, register_notebook};
use proxiima_example::{ExampleSpiice, NoteV1};
use proxiima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxiima_storage_pg::{PgHostStateLifecyclePort, PgHostStateParticipant};
use sqlx::{AssertSqlSafe, Postgres, Transaction};
use uuid::Uuid;

const PARTICIPANT: HostStateParticipantId =
    HostStateParticipantId::new("scope_erase_full_registry");
const COPY_TABLE: &str = "proxiima_core.test_scope_host_copy";
const TABLES: &[StateSurfaceName] = &[StateSurfaceName::new(COPY_TABLE)];

const COPY_SURFACE: Surface = Surface {
    table: COPY_TABLE,
    key: KeyShape::MemoryT { column: "fact_id" },
    owner_column: Some("owner_id"),
    transfer: TransferRule::RetainAtSource {
        why: "the fixture copy remains tied to its original owner",
    },
    erase: EraseRule::HostState {
        whole_owner: proxiima_core::spiice::HostStateEraseDisposition::Erase,
        source: proxiima_core::spiice::HostStateEraseDisposition::Erase,
        exact_fact: proxiima_core::spiice::HostStateEraseDisposition::Erase,
    },
    // Erased by the host lifecycle, so the export must carry it or name
    // what it derives from (#478).
    export: ExportRule::DerivedFrom {
        source: "proxiima_core.memory",
        why: "the fixture only observes the erase callback",
    },
    forget: ForgetRule::Keep {
        why: "the fixture copy follows explicit lifecycle erasure only",
    },
    lexical_language_column: None,
    counter: CounterRule::Counted("test_scope_host_copy_rows"),
    completeness: None,
};

const TEST_CONTRACT: SpiiceContract = SpiiceContract {
    retired_facts: &[],
    spiice_id: "test-scope-host-lifecycle",
    ordinal: 31,
    schemas: &[],
    state_surfaces: &[COPY_SURFACE],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    projection: ProjectionDecl::None {
        why: "the lifecycle fixture has no search surface",
    },
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
};

proxiima::spiice::proxiima_spiice! {
    name = "test-scope-host-lifecycle",
    display_name = "Scope host lifecycle test",
    contract = &TEST_CONTRACT,
}

static CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
static FAIL_AFTER_DELETE_ONCE: AtomicBool = AtomicBool::new(false);
static LAST_PHYSICAL_SELECTION: Mutex<Vec<MemoryId>> = Mutex::new(Vec::new());

struct FullRegistryApp;

struct TrustedOwnerAuth(OwnerRoles);

#[async_trait]
impl Authenticator for TrustedOwnerAuth {
    async fn authenticate(&self, _: &Credentials) -> Result<AuthzContext, AuthError> {
        Ok(AuthzContext::server_resolved(
            self.0.clone(),
            AuthPath::HostBearer,
        ))
    }
}

impl SpiiceBundle for FullRegistryApp {
    fn register(registry: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        <ExampleSpiice as SpiiceBundle>::register(registry)?;
        self::register(registry)
    }

    fn register_pg_sidecars(registry: &mut proxiima::spiice::PgSidecarRegistry) {
        <ExampleSpiice as SpiiceBundle>::register_pg_sidecars(registry);
    }

    fn migrators() -> Vec<NamedMigrator> {
        <ExampleSpiice as SpiiceBundle>::migrators()
    }
}

impl SpiiceApp for FullRegistryApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "proxiima-mcp-full-registry-scope-erase-test",
            title: "Proxiima MCP full registry scope erase test",
            version: "0",
        }
    }

    fn services(ctx: &proxiima::AppContext) -> Result<SpiiceServices, SpiiceServiceError> {
        <ExampleSpiice as SpiiceApp>::services(ctx)
    }
}

struct Lifecycle;

#[async_trait]
impl PgHostStateLifecyclePort for Lifecycle {
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
        CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);
        *LAST_PHYSICAL_SELECTION.lock().expect("selection slot") =
            request.selected_fact_ids().to_vec();

        if !matches!(
            request.scope(),
            HostStateEraseScope::ExactFacts | HostStateEraseScope::Source(_)
        ) {
            return Err(StorageError::Internal(
                "scope lifecycle erase used an unsupported scope".into(),
            ));
        }
        let ids = request
            .selected_fact_ids()
            .iter()
            .map(|id| id.into_inner())
            .collect::<Vec<_>>();
        let deleted =
            sqlx::query("DELETE FROM proxiima_core.test_scope_host_copy WHERE fact_id = ANY($1)")
                .bind(&ids)
                .execute(&mut **tx)
                .await
                .map_err(|error| StorageError::Internal(error.to_string()))?
                .rows_affected();

        if FAIL_AFTER_DELETE_ONCE.swap(false, Ordering::SeqCst) {
            return Err(StorageError::Internal(
                "test failure after host callback delete".into(),
            ));
        }

        Ok(HostStateEraseReceipt {
            participant: PARTICIPANT,
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
        _tx: &mut Transaction<'_, Postgres>,
        request: HostStateExportRequest,
    ) -> Result<HostStateExportReceipt, StorageError> {
        if !request.included_tables().is_empty() {
            return Err(StorageError::Internal(
                "excluded test table was requested for export".into(),
            ));
        }
        Ok(HostStateExportReceipt {
            participant: PARTICIPANT,
            owner: request.owner(),
            tables: Vec::new(),
        })
    }
}

struct Participant(Arc<Lifecycle>);

#[async_trait]
impl PgHostStateParticipant for Participant {
    fn participant_id(&self) -> HostStateParticipantId {
        PARTICIPANT
    }

    fn declared_tables(&self) -> &'static [StateSurfaceName] {
        TABLES
    }

    fn lifecycle_port(&self) -> Option<Arc<dyn PgHostStateLifecyclePort>> {
        Some(self.0.clone())
    }

    async fn apply(
        &self,
        _session: &mut proxiima_storage_pg::PgHostStateSession<'_, '_>,
        _permit: &HostStateWritePermit,
        _request: HostStateRequest,
    ) -> Result<HostStateReply, StorageError> {
        Err(StorageError::Internal(
            "write callback is unused in the erase integration test".into(),
        ))
    }
}

#[tokio::test]
async fn production_scope_erase_uses_full_boot_registry_for_atomic_host_erase()
-> Result<(), Box<dyn std::error::Error>> {
    let database = unique_db_name("proxiima_full_registry_scope_erase");
    create_db(&database).await?;
    let result = exercise_full_registry_erase(&db_url(&database)).await;
    let drop_result = drop_db(&database).await;
    drop_result?;
    result
}

async fn exercise_full_registry_erase(
    database_url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    CALLBACK_COUNT.store(0, Ordering::SeqCst);
    FAIL_AFTER_DELETE_ONCE.store(false, Ordering::SeqCst);
    LAST_PHYSICAL_SELECTION
        .lock()
        .expect("selection slot")
        .clear();

    let (runtime_url, platform_url) = common::split_roles(database_url).await?;
    let migration_storage = proxiima_storage_pg::PgMigrationStorage::connect_with_config(
        &platform_url,
        proxiima_storage_pg::PgPoolConfig::from_env()?,
        proxiima_storage_pg::PgTuning::from_env()?,
    )
    .await?;
    let mut registry = proxiima::SpiiceRegistry::new();
    FullRegistryApp::register(&mut registry)?;
    let registry = registry.try_freeze()?;
    let mut sidecars = proxiima::PgSidecarRegistry::new();
    proxiima::spiice::register_core_pg_sidecars(&mut sidecars);
    FullRegistryApp::register_pg_sidecars(&mut sidecars);
    let sidecars = sidecars.freeze_against(&registry)?;
    proxiima::run_core_and_spiice_migrations(
        &migration_storage,
        <FullRegistryApp as SpiiceBundle>::migrators(),
        &sidecars,
    )
    .await?;
    let platform_pool = sqlx::PgPool::connect(&platform_url).await?;
    sqlx::query(
        "CREATE TABLE proxiima_core.test_scope_host_copy (
             fact_id uuid PRIMARY KEY,
             owner_id uuid NOT NULL,
             payload bytea NOT NULL
         )",
    )
    .execute(&platform_pool)
    .await?;
    sqlx::raw_sql(AssertSqlSafe("ALTER TABLE proxiima_core.test_scope_host_copy ENABLE ROW LEVEL SECURITY; ALTER TABLE proxiima_core.test_scope_host_copy FORCE ROW LEVEL SECURITY; CREATE POLICY proxiima_owner_read ON proxiima_core.test_scope_host_copy FOR SELECT TO PUBLIC USING (owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])); CREATE POLICY proxiima_owner_write ON proxiima_core.test_scope_host_copy FOR ALL TO PUBLIC USING (owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])) WITH CHECK (owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])); CREATE POLICY proxiima_platform ON proxiima_core.test_scope_host_copy FOR ALL TO CURRENT_USER USING (current_setting('app.proxiima_scope', true) = 'platform') WITH CHECK (current_setting('app.proxiima_scope', true) = 'platform')".to_owned()))
        .execute(&platform_pool)
        .await?;
    platform_pool.close().await;

    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let subject = owner.as_user();
    let authz = proxiima_core::authenticate(
        &TrustedOwnerAuth(OwnerRoles::for_subject(subject, []).unwrap()),
        &Credentials::Bearer("full-registry-test".into()),
    )
    .await?;
    let owner_scope = authz.owner_scope().expect("verified owner scope").clone();
    let built = Proxiima::<FullRegistryApp>::app()
        .database_url(&runtime_url)
        .store_number(0)
        .platform_database_url(&platform_url)
        .owner(owner)
        .allow_insecure_single_owner()
        .served_surface(proxiima::ServedSurface::All)
        .skip_migrations()
        .host_state_participant(Arc::new(Participant(Arc::new(Lifecycle))))
        .build()
        .await?;
    let pool = sqlx::PgPool::connect(database_url).await?;
    let runtime_pool = sqlx::PgPool::connect(&runtime_url).await?;

    let notebook_id = register_notebook(
        &runtime_pool,
        &owner_scope,
        owner,
        "full registry lifecycle callback",
    )
    .await
    .map_err(|err| err.to_string())?;
    let fact_id = Uuid::now_v7();
    insert_note_fact(&pool, &owner, fact_id, notebook_id).await?;
    sqlx::query(
        "INSERT INTO proxiima_core.test_scope_host_copy (fact_id, owner_id, payload)
         VALUES ($1, $2, decode('cafe', 'hex'))",
    )
    .bind(fact_id)
    .bind(owner.into_inner())
    .execute(&pool)
    .await?;
    let source = SourceId::new("full-registry-scope-source");
    sqlx::query(
        "INSERT INTO proxiima_core.publication_origin
             (t, original_owner_id, source_id)
         VALUES ($1, $2, $3)",
    )
    .bind(fact_id)
    .bind(owner.into_inner())
    .bind(source.as_str())
    .execute(&pool)
    .await?;

    // The host callback deletes its row and then fails. The production scope
    // erase must roll back that callback SQL together with its Fact inverse.
    FAIL_AFTER_DELETE_ONCE.store(true, Ordering::SeqCst);
    let engine = Arc::clone(built.host().engine());
    let failed =
        erase_notebook_with_scope(&engine, &authz, &runtime_pool, owner, notebook_id).await;
    assert!(
        failed.is_err(),
        "the injected callback failure must abort erase"
    );
    assert_eq!(count_memory(&pool, fact_id).await?, 1);
    assert_eq!(count_copy(&pool, fact_id).await?, 1);
    assert_eq!(CALLBACK_COUNT.load(Ordering::SeqCst), 1);

    let erased = erase_notebook_with_scope(&engine, &authz, &runtime_pool, owner, notebook_id)
        .await
        .map_err(|err| err.to_string())?;
    assert_eq!(erased.memories_deleted, 1);
    assert!(erased.notebook_record_deleted);
    assert_eq!(CALLBACK_COUNT.load(Ordering::SeqCst), 2);
    assert_eq!(
        *LAST_PHYSICAL_SELECTION.lock().expect("selection slot"),
        vec![MemoryId::new(fact_id)],
        "the full-registry callback receives the exact selected physical Fact"
    );
    assert_eq!(count_memory(&pool, fact_id).await?, 0);
    assert_eq!(count_copy(&pool, fact_id).await?, 0);
    assert_eq!(count_note(&pool, fact_id).await?, 0);
    assert_eq!(count_publication_origin(&pool, fact_id).await?, 0);

    // Re-enter source erasure with the same boot-frozen full spiice registry.
    // The exact scope erase already removed the Fact's physical copy and origin;
    // the later source inverse must finish cleanly without leaving an origin
    // or host copy orphaned.
    let mut sidecars = PgSidecarRegistry::new();
    proxiima::spiice::register_core_pg_sidecars(&mut sidecars);
    <FullRegistryApp as SpiiceBundle>::register_pg_sidecars(&mut sidecars);
    let sidecars = sidecars.freeze_against(built.host().registry())?;
    let source_storage = common::runtime_storage(&runtime_url, &platform_url)
        .await?
        .with_host_state_participant(Arc::new(Participant(Arc::new(Lifecycle))))
        .with_sidecars(sidecars)
        .await?
        .try_with_spiices(built.host().registry())?;
    let user_id = owner.as_user();
    let source_auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
        user_id,
        source_id: source.clone(),
        drop_event_id: "full-registry-source-after-scope".into(),
    });
    let source_outcome = OwnerInversePort::erase_personal_source_scope(
        &source_storage,
        &source_auth,
        user_id,
        &source,
        source_storage.surfaces(),
    )
    .await?;
    assert!(matches!(
        source_outcome,
        OwnerEraseOutcome::Completed { .. }
    ));
    assert_eq!(CALLBACK_COUNT.load(Ordering::SeqCst), 3);
    assert_eq!(count_memory(&pool, fact_id).await?, 0);
    assert_eq!(count_copy(&pool, fact_id).await?, 0);
    assert_eq!(count_publication_origin(&pool, fact_id).await?, 0);

    drop(source_storage);
    pool.close().await;
    runtime_pool.close().await;
    drop(engine);
    built.shutdown().await;
    Ok(())
}

async fn insert_note_fact(
    pool: &sqlx::PgPool,
    owner: &OwnerId,
    fact_id: Uuid,
    notebook_id: Uuid,
) -> Result<(), sqlx::Error> {
    let owner_id = owner.into_inner();
    let handle = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxiima_core.owners (owner_id, kind)
         VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxiima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'fact', $2, $3, $4)",
    )
    .bind(handle)
    .bind(NoteV1::SCHEMA_ID)
    .bind(owner_id)
    .bind(fact_id)
    .execute(pool)
    .await?;
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO proxiima_core.memory (handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via)
         VALUES ($1, $2, 'fact', $3, $4, $5, uuid_extract_timestamp($2), $3, NULL)",
    )
    .bind(handle)
    .bind(fact_id)
    .bind(owner_id)
    .bind(NoteV1::SCHEMA_ID)
    .bind(vec!["proxiima_example.note_v1"])
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO proxiima_example.note_v1 (t, notebook_id, key, body, written_at)
         VALUES ($1, $2, 'fixture', 'fixture', now())",
    )
    .bind(fact_id)
    .bind(notebook_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn count_memory(pool: &sqlx::PgPool, id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxiima_core.memory WHERE t = $1")
        .bind(id)
        .fetch_one(pool)
        .await
}

async fn count_copy(pool: &sqlx::PgPool, id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxiima_core.test_scope_host_copy WHERE fact_id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
}

async fn count_note(pool: &sqlx::PgPool, id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxiima_example.note_v1 WHERE t = $1")
        .bind(id)
        .fetch_one(pool)
        .await
}

async fn count_publication_origin(pool: &sqlx::PgPool, id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxiima_core.publication_origin WHERE t = $1")
        .bind(id)
        .fetch_one(pool)
        .await
}

//! Production Proxima MCP service assembly carries the full frozen host
//! lifecycle registry into Code repository erasure.
#![cfg(feature = "code")]
#![allow(clippy::too_many_lines)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use proxima::flavor::{
    CounterRule, EraseRule, ExportRule, FlavorBundle, FlavorContract, FlavorRegistry,
    FlavorRegistryError, ForgetRule, KeyShape, NamedMigrator, PgSidecarRegistry, ProjectionDecl,
    Surface, TransferRule,
};
use proxima::{AppInfo, FlavorApp, Proxima};
use proxima_code::testkit::{erase_repo, register_repo};
use proxima_code::{CodeFlavorStore, CommitV1, RepoScope};
use proxima_core::owner_inverse::{EraseAuthorization, OwnerEraseOutcome, OwnerEraseTarget};
use proxima_core::storage_ports::{
    HostStateEraseReceipt, HostStateEraseRequest, HostStateEraseScope, HostStateEraseTableCount,
    HostStateExportReceipt, HostStateExportRequest, HostStateParticipantId, HostStateReply,
    HostStateRequest, HostStateWritePermit, OwnerInversePort, StateSurfaceName,
};
use proxima_core::{
    FactPayload, FlavorServiceError, FlavorServices, MemoryId, Owner, OwnerRef, SourceId,
    StorageError, UserId,
};
use proxima_mcp::ProximaMcpApp;
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::{PgHostStateLifecyclePort, PgHostStateParticipant, PgStorage};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

const PARTICIPANT: HostStateParticipantId = HostStateParticipantId::new("code_erase_full_registry");
const COPY_TABLE: &str = "proxima_core.test_code_host_copy";
const TABLES: &[StateSurfaceName] = &[StateSurfaceName::new(COPY_TABLE)];

const COPY_SURFACE: Surface = Surface {
    table: COPY_TABLE,
    key: KeyShape::MemoryT { column: "fact_id" },
    owner_column: Some("owner_id"),
    transfer: TransferRule::RetainAtSource {
        why: "the fixture copy remains tied to its original owner",
    },
    erase: EraseRule::HostState {
        whole_owner: proxima_core::flavor::HostStateEraseDisposition::Erase,
        source: proxima_core::flavor::HostStateEraseDisposition::Erase,
        exact_fact: proxima_core::flavor::HostStateEraseDisposition::Erase,
    },
    export: ExportRule::Excluded {
        why: "the fixture only observes the erase callback",
    },
    forget: ForgetRule::Keep {
        why: "the fixture copy follows explicit lifecycle erasure only",
    },
    lexical_language_column: None,
    counter: CounterRule::Counted("test_code_host_copy_rows"),
    completeness: None,
};

const TEST_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: "test-code-host-lifecycle",
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

proxima::flavor::proxima_flavor! {
    name = "test-code-host-lifecycle",
    display_name = "Code host lifecycle test",
    contract = &TEST_CONTRACT,
}

static CODE_STORE: Mutex<Option<Arc<CodeFlavorStore>>> = Mutex::new(None);
static CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
static FAIL_AFTER_DELETE_ONCE: AtomicBool = AtomicBool::new(false);
static LAST_PHYSICAL_SELECTION: Mutex<Vec<MemoryId>> = Mutex::new(Vec::new());

struct FullRegistryApp;

impl FlavorBundle for FullRegistryApp {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        <ProximaMcpApp as FlavorBundle>::register(registry)?;
        self::register(registry)
    }

    fn register_pg_sidecars(registry: &mut proxima::flavor::PgSidecarRegistry) {
        <ProximaMcpApp as FlavorBundle>::register_pg_sidecars(registry);
    }

    fn migrators() -> Vec<NamedMigrator> {
        <ProximaMcpApp as FlavorBundle>::migrators()
    }
}

impl FlavorApp for FullRegistryApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "proxima-mcp-full-registry-erase-test",
            title: "Proxima MCP full registry erase test",
            version: "0",
        }
    }

    fn services(ctx: &proxima::AppContext) -> Result<FlavorServices, FlavorServiceError> {
        let services = <ProximaMcpApp as FlavorApp>::services(ctx)?;
        let store = services
            .get::<CodeFlavorStore>()
            .expect("the production ProximaMcpApp registers its Code store");
        *CODE_STORE.lock().expect("store slot") = Some(store);
        Ok(services)
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
                "Code lifecycle erase used an unsupported scope".into(),
            ));
        }
        let ids = request
            .selected_fact_ids()
            .iter()
            .map(|id| id.into_inner())
            .collect::<Vec<_>>();
        let deleted =
            sqlx::query("DELETE FROM proxima_core.test_code_host_copy WHERE fact_id = ANY($1)")
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
        _tx: &mut Transaction<'_, Postgres>,
        _permit: &HostStateWritePermit,
        _request: HostStateRequest,
    ) -> Result<HostStateReply, StorageError> {
        Err(StorageError::Internal(
            "write callback is unused in the erase integration test".into(),
        ))
    }
}

#[tokio::test]
async fn production_code_store_uses_full_boot_registry_for_atomic_host_erase()
-> Result<(), Box<dyn std::error::Error>> {
    let database = unique_db_name("proxima_full_registry_code_erase");
    create_db(&database).await?;
    let result = exercise_full_registry_erase(&db_url(&database)).await;
    *CODE_STORE.lock().expect("store slot") = None;
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

    let storage = PgStorage::connect(database_url).await?;
    proxima::run_core_and_flavor_migrations(
        &storage,
        <FullRegistryApp as FlavorBundle>::migrators(),
    )
    .await?;
    sqlx::query(
        "CREATE TABLE proxima_core.test_code_host_copy (
             fact_id uuid PRIMARY KEY,
             owner_id uuid NOT NULL,
             payload bytea NOT NULL
         )",
    )
    .execute(storage.pool_for_tests())
    .await?;
    drop(storage);

    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let built = Proxima::<FullRegistryApp>::app()
        .database_url(database_url)
        .owner(owner)
        .allow_insecure_single_owner()
        .tool_scope(proxima::ToolScope::All)
        .skip_migrations()
        .host_state_participant(Arc::new(Participant(Arc::new(Lifecycle))))
        .build()
        .await?;
    let store = CODE_STORE
        .lock()
        .expect("store slot")
        .clone()
        .expect("production service assembly captured CodeFlavorStore");
    let pool = sqlx::PgPool::connect(database_url).await?;

    let repo_id = Uuid::now_v7();
    register_repo(
        &pool,
        &owner,
        repo_id,
        &format!("/tmp/proxima-full-registry-{repo_id}"),
        "full registry lifecycle callback",
        &RepoScope::default(),
    )
    .await?;
    let fact_id = Uuid::now_v7();
    insert_code_fact(&pool, &owner, fact_id, repo_id).await?;
    sqlx::query(
        "INSERT INTO proxima_core.test_code_host_copy (fact_id, owner_id, payload)
         VALUES ($1, $2, decode('cafe', 'hex'))",
    )
    .bind(fact_id)
    .bind(owner.stored_owner_id())
    .execute(&pool)
    .await?;
    let source = SourceId::new("full-registry-code-source");
    sqlx::query(
        "INSERT INTO proxima_core.publication_origin
             (t, original_owner_id, original_owner_kind, source_id)
         VALUES ($1, $2, 'personal', $3)",
    )
    .bind(fact_id)
    .bind(owner.stored_owner_id())
    .bind(source.as_str())
    .execute(&pool)
    .await?;

    // The host callback deletes its row and then fails. The production Code
    // erase must roll back that callback SQL together with its Fact inverse.
    FAIL_AFTER_DELETE_ONCE.store(true, Ordering::SeqCst);
    let failed = erase_repo(&store, &owner, repo_id).await;
    assert!(
        failed.is_err(),
        "the injected callback failure must abort erase"
    );
    assert_eq!(count_memory(&pool, fact_id).await?, 1);
    assert_eq!(count_copy(&pool, fact_id).await?, 1);
    assert_eq!(CALLBACK_COUNT.load(Ordering::SeqCst), 1);

    let erased = erase_repo(&store, &owner, repo_id).await?;
    assert_eq!(erased.memories_deleted, 1);
    assert_eq!(CALLBACK_COUNT.load(Ordering::SeqCst), 2);
    assert_eq!(
        *LAST_PHYSICAL_SELECTION.lock().expect("selection slot"),
        vec![MemoryId::new(fact_id)],
        "the full-registry callback receives the exact selected physical Fact"
    );
    assert_eq!(count_memory(&pool, fact_id).await?, 0);
    assert_eq!(count_copy(&pool, fact_id).await?, 0);
    assert_eq!(count_code_commit(&pool, fact_id).await?, 0);
    assert_eq!(count_publication_origin(&pool, fact_id).await?, 0);

    // Re-enter source erasure with the same boot-frozen full flavor registry.
    // Exact Code erase already removed the Fact's physical copy and origin;
    // the later source inverse must finish cleanly without leaving an origin
    // or host copy orphaned.
    let mut sidecars = PgSidecarRegistry::new();
    proxima::flavor::register_core_pg_sidecars(&mut sidecars);
    <FullRegistryApp as FlavorBundle>::register_pg_sidecars(&mut sidecars);
    let sidecars = sidecars.freeze_against(built.registry())?;
    let source_storage = PgStorage::connect(database_url)
        .await?
        .with_host_state_participant(Arc::new(Participant(Arc::new(Lifecycle))))
        .with_sidecars(sidecars)
        .try_with_flavors(built.registry())?;
    let Owner::Personal(user_id) = owner else {
        unreachable!("fixture owner is personal")
    };
    let source_auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
        user_id,
        source_id: source.clone(),
        drop_event_id: "full-registry-source-after-code".into(),
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
    drop(store);
    built.shutdown();
    Ok(())
}

async fn insert_code_fact(
    pool: &sqlx::PgPool,
    owner: &Owner,
    fact_id: Uuid,
    repo_id: Uuid,
) -> Result<(), sqlx::Error> {
    let owner_id = owner.stored_owner_id();
    let handle = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'fact', $2, $3, $4)",
    )
    .bind(handle)
    .bind(CommitV1::SCHEMA_ID)
    .bind(owner_id)
    .bind(fact_id)
    .execute(pool)
    .await?;
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id, sidecar_tables)
         VALUES ($1, $2, 'fact', $3, $4, $5)",
    )
    .bind(handle)
    .bind(fact_id)
    .bind(owner_id)
    .bind(CommitV1::SCHEMA_ID)
    .bind(vec!["proxima_code.commit_v1"])
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_code.commit_v1
            (t, repo_id, sha, parents, author_name, author_email,
             author_time, committer_name, committer_email, committer_time, message)
         VALUES ($1, $2, 'abc1234', ARRAY[]::text[], 'A', 'a@example.com',
                 now(), 'A', 'a@example.com', now(), 'fixture')",
    )
    .bind(fact_id)
    .bind(repo_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn count_memory(pool: &sqlx::PgPool, id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
        .bind(id)
        .fetch_one(pool)
        .await
}

async fn count_copy(pool: &sqlx::PgPool, id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.test_code_host_copy WHERE fact_id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
}

async fn count_code_commit(pool: &sqlx::PgPool, id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_code.commit_v1 WHERE t = $1")
        .bind(id)
        .fetch_one(pool)
        .await
}

async fn count_publication_origin(pool: &sqlx::PgPool, id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.publication_origin WHERE t = $1")
        .bind(id)
        .fetch_one(pool)
        .await
}

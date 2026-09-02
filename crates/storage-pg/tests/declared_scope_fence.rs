//! The declared lifecycle scope, at the substrate, under real
//! interleavings.
//!
//! The fixture owns its schema, its scope registry, its sidecars and its
//! declaration, so nothing here depends on a product flavor: what is being
//! pinned is that DECLARING a scope is enough, and that the Engine — not the
//! declaring flavor — is what takes the fence and asks the liveness
//! question.
//!
//! Nothing sleeps as a synchronization step. Each race waits for Postgres to
//! say a backend is blocked, because the thing being pinned is an ordering
//! and a clock cannot observe one.
#![allow(clippy::doc_markdown)]

use proxima_core::engine::TypedFactIngest;
use proxima_core::flavor::{
    CounterRule, EmbeddingRecipe, EraseRule, ExportRule, FlavorContract, ForgetRule, KeyShape,
    ProjectionDecl, Provenance, SchemaContract, SchemaRef, SearchProjectionDecl, Surface,
    TransferRule,
};
use proxima_core::{
    AuthPath, AuthzContext, FactPayload, FlavorRegistry, Owner, OwnerRef, PayloadKeyBuilder,
    ScopeDecl, ScopeKind, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::access::owner_columns::lock_scope_fence_exclusive_tx;
use proxima_storage_pg::{PgSidecarRegistry, PgStorage, register_core_pg_sidecars};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const TEST_FLAVOR: &str = "test-scope";
const SCOPED_TABLE: &str = "test_scope.scoped_fact_v1";
const UNSCOPED_TABLE: &str = "test_scope.unscoped_fact_v1";

/// The fixture's scope kind. Declared once below and named by the scoped
/// payload; freeze refuses the pair if either half goes missing.
const THING: ScopeKind = ScopeKind::new("test-thing");

const THING_DECL: ScopeDecl = ScopeDecl {
    kind: THING,
    registry_table: "test_scope.things",
    id_column: "thing_id",
    owner_kind_column: "owner_kind",
    owner_id_column: "owner_id",
};

/// A payload that belongs to a `test-thing`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScopedFactV1 {
    logical_id: String,
    thing_id: Uuid,
}

impl FactPayload for ScopedFactV1 {
    const SCHEMA_ID: &'static str = "test-scope/scoped-fact-v1";
    const SCHEMA_VERSION: u32 = 1;
    const SCOPE_KIND: Option<ScopeKind> = Some(THING);

    fn scope_id(&self) -> Option<Uuid> {
        Some(self.thing_id)
    }

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("logical_id", &self.logical_id);
        key.finish()
    }

    fn render(&self) -> String {
        self.logical_id.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some(SCOPED_TABLE)
    }
}

/// The same shape, belonging to nothing. Its admission must take no scope
/// fence at all — a substrate that fenced every write would serialize
/// unrelated writers behind one erase.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UnscopedFactV1 {
    logical_id: String,
}

impl FactPayload for UnscopedFactV1 {
    const SCHEMA_ID: &'static str = "test-scope/unscoped-fact-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("logical_id", &self.logical_id);
        key.finish()
    }

    fn render(&self) -> String {
        self.logical_id.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some(UNSCOPED_TABLE)
    }
}

const fn surface(table: &'static str, constraint: &'static str) -> Surface {
    Surface {
        table,
        key: KeyShape::MemoryT { column: "t" },
        owner_column: None,
        transfer: TransferRule::StaysOnKey,
        erase: EraseRule::ByKey,
        export: ExportRule::Rows,
        forget: ForgetRule::DumpThenDelete,
        lexical_language_column: None,
        counter: CounterRule::Counted("sidecar_rows"),
        completeness: Some(proxima_core::flavor::DbConstraint {
            relation: table,
            name: constraint,
        }),
    }
}

const SCOPED_SURFACE: Surface = surface(SCOPED_TABLE, "scoped_fact_v1_t_fkey");
const UNSCOPED_SURFACE: Surface = surface(UNSCOPED_TABLE, "unscoped_fact_v1_t_fkey");

const fn schema(
    name: &'static str,
    table: &'static str,
    surface: &'static [Surface],
) -> SchemaContract {
    SchemaContract {
        id: SchemaRef::new(TEST_FLAVOR, name, 1),
        kind: proxima_core::verbs::schema::PayloadKind::Fact,
        sidecar_table: Some(table),
        search: SearchProjectionDecl::None {
            why: "the fence fixture is read by key, not searched",
        },
        embedding: EmbeddingRecipe::Never {
            why: "the fence fixture carries identifiers, not searchable text",
        },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: surface,
        natural_key_columns: &[],
    }
}

static TEST_SCOPE_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: TEST_FLAVOR,
    ordinal: 78,
    schemas: &[
        schema("scoped-fact", SCOPED_TABLE, &[SCOPED_SURFACE]),
        schema("unscoped-fact", UNSCOPED_TABLE, &[UNSCOPED_SURFACE]),
    ],
    state_surfaces: &[],
    // The declaration under test. Storage spells no name of its own: the
    // fence key and the liveness probe are both generated from this.
    scopes: &[THING_DECL],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    projection: ProjectionDecl::None {
        why: "the fence fixture is not a search surface",
    },
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
};

proxima_core::proxima_flavor! {
    name = "test-scope",
    fact_schemas = [ScopedFactV1, UnscopedFactV1],
    contract = &TEST_SCOPE_CONTRACT,
}

proxima_storage_pg::pg_sidecar! {
    payload: ScopedFactV1,
    row: ScopedFactRow,
    kinds: [Fact],
    table: "test_scope.scoped_fact_v1",
    key: t,
    fields: {
        logical_id => logical_id: (text),
        thing_id => thing_id: (uuid),
    },
}

proxima_storage_pg::pg_sidecar! {
    payload: UnscopedFactV1,
    row: UnscopedFactRow,
    kinds: [Fact],
    table: "test_scope.unscoped_fact_v1",
    key: t,
    fields: {
        logical_id => logical_id: (text),
    },
}

/// Backends blocked on an advisory lock in this database.
///
/// The fence IS an advisory lock and each test owns its database, so an
/// ungranted advisory lock here is this test's writer waiting on this test's
/// fence and nothing else.
const ADVISORY_WAITERS_SQL: &str = "\
SELECT count(*)::bigint
  FROM pg_locks
 WHERE locktype = 'advisory'
   AND NOT granted
   AND database = (SELECT oid FROM pg_database WHERE datname = current_database())";

fn owner() -> Owner {
    OwnerRef::Personal(UserId::new(Uuid::now_v7()))
}

fn registry() -> proxima_core::FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    register(&mut registry).expect("fixture schema registration");
    registry.freeze_or_panic_for_tests()
}

fn sidecars(
    registry: &proxima_core::FlavorRegistryFrozen,
) -> proxima_storage_pg::PgSidecarRegistryFrozen {
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    sidecars.add_fact::<ScopedFactV1>();
    sidecars.add_fact::<UnscopedFactV1>();
    sidecars
        .freeze_against(registry)
        .expect("fixture sidecar registration")
}

async fn bootstrap() -> (String, PgStorage, proxima_core::FlavorRegistryFrozen) {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(error) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {error}");
    }
    let registry = registry();
    let result = async {
        let pg = PgStorage::connect(&db_url(&db_name)).await?;
        pg.run_migrations().await?;
        sqlx::raw_sql(
            "CREATE SCHEMA test_scope;
             CREATE TABLE test_scope.things (
                 owner_kind proxima_core.owner_kind NOT NULL,
                 owner_id uuid NOT NULL,
                 thing_id uuid NOT NULL,
                 PRIMARY KEY (owner_kind, owner_id, thing_id)
             );
             CREATE TABLE test_scope.scoped_fact_v1 (
                 t uuid PRIMARY KEY REFERENCES proxima_core.memory(t) ON DELETE CASCADE,
                 logical_id text NOT NULL,
                 thing_id uuid NOT NULL
             );
             CREATE TABLE test_scope.unscoped_fact_v1 (
                 t uuid PRIMARY KEY REFERENCES proxima_core.memory(t) ON DELETE CASCADE,
                 logical_id text NOT NULL
             );
             INSERT INTO proxima_core.flavor_surface (table_name, flavor_id)
             VALUES ('test_scope.scoped_fact_v1', 'test-scope'),
                    ('test_scope.unscoped_fact_v1', 'test-scope');
             CREATE TRIGGER scoped_fact_v1_declared_by_memory
             BEFORE INSERT ON test_scope.scoped_fact_v1
             FOR EACH ROW
             EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
             CREATE TRIGGER unscoped_fact_v1_declared_by_memory
             BEFORE INSERT ON test_scope.unscoped_fact_v1
             FOR EACH ROW
             EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');",
        )
        .execute(pg.pool_for_tests())
        .await?;
        let pg = pg
            .with_sidecars(sidecars(&registry))
            .with_flavors(&registry);
        Ok::<_, Box<dyn std::error::Error>>((pg, registry))
    }
    .await;
    match result {
        Ok((pg, registry)) => (db_name, pg, registry),
        Err(error) => {
            let _ = drop_db(&db_name).await;
            panic!("fixture bootstrap failed: {error}");
        }
    }
}

fn engine(pg: &PgStorage, registry: &proxima_core::FlavorRegistryFrozen) -> proxima_core::Engine {
    proxima_core::Engine::new(registry.clone())
        .with_storage_ports(std::sync::Arc::new(pg.clone()).storage_ports())
}

async fn register_thing(
    pool: &sqlx::PgPool,
    owner: Owner,
    thing_id: Uuid,
) -> Result<(), sqlx::Error> {
    let (kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO test_scope.things (owner_kind, owner_id, thing_id) VALUES ($1, $2, $3)",
    )
    .bind(kind)
    .bind(owner_id)
    .bind(thing_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Poll until Postgres reports at least `at_least` backends blocked on an
/// advisory lock, or give up.
///
/// The bound is a bound on the TEST, not a timing assumption: the condition
/// is "the database says a backend is blocked", true or false at each poll
/// and never becoming true by waiting longer than the interleaving needs.
async fn wait_for_advisory_waiters(pool: &sqlx::PgPool, at_least: i64) -> bool {
    for _ in 0..400 {
        let seen: i64 = sqlx::query_scalar(ADVISORY_WAITERS_SQL)
            .fetch_one(pool)
            .await
            .expect("advisory-wait probe");
        if seen >= at_least {
            return true;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    false
}

/// Ingest one Fact of the given payload on its own connection, spawned so
/// it can block.
fn spawn_scoped_ingest(
    db_name: String,
    registry: proxima_core::FlavorRegistryFrozen,
    owner: Owner,
    payload: ScopedFactV1,
) -> tokio::task::JoinHandle<Result<Uuid, String>> {
    tokio::spawn(async move {
        let pg = PgStorage::connect(&db_url(&db_name))
            .await
            .map_err(|err| err.to_string())?
            .with_sidecars(sidecars(&registry))
            .with_flavors(&registry);
        let engine = engine(&pg, &registry);
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        engine
            .ingest_typed_fact_with(&authz, TypedFactIngest::new("test/scope-fence", &payload))
            .await
            .map(|outcome| outcome.memory_id.into_inner())
            .map_err(|err| err.to_string())
    })
}

/// The admission side of the declaration: a scoped payload's write takes
/// the generated fence, and an erase holding it exclusively makes that
/// write wait.
///
/// The barrier takes the fence through the exported helper, which is the
/// same key the Engine generates from `THING_DECL` — that identity is the
/// whole claim. If the admission took no fence, or took a different key,
/// this write would sail past and the assertion would say so.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admission_of_a_scoped_payload_waits_on_the_declared_fence() {
    let (db_name, pg, registry) = bootstrap().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = owner();
        let thing_id = Uuid::now_v7();
        register_thing(pool, owner, thing_id).await?;

        // The barrier: the scope's fence, exclusively, on its own
        // connection. This is what an erase holds while it computes its
        // footprint.
        let barrier_pool = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let mut barrier = barrier_pool.begin().await?;
        lock_scope_fence_exclusive_tx(&mut barrier, THING, &owner, thing_id).await?;

        let write = spawn_scoped_ingest(
            db_name.clone(),
            registry.clone(),
            owner,
            ScopedFactV1 {
                logical_id: "fenced".to_owned(),
                thing_id,
            },
        );
        assert!(
            wait_for_advisory_waiters(pool, 1).await,
            "the admission of a scoped payload did not wait on the declared fence"
        );
        assert!(!write.is_finished(), "the admission ran past the fence");

        barrier.rollback().await?;
        barrier_pool.close().await;

        let memory_id = write.await?.expect("the admission completes once unfenced");
        let landed: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM test_scope.scoped_fact_v1 WHERE t = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(landed, 1, "and it is the write, not a no-op, that waited");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("an_admission_of_a_scoped_payload_waits_on_the_declared_fence failed");
}

/// A payload that names no scope takes no scope fence.
///
/// With the SAME fence held exclusively, an unscoped admission must go
/// through. A substrate that fenced every write would serialize every
/// writer of every flavor behind one scope's erase, which is a worse
/// failure than the one the fence exists to fix.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unscoped_payload_takes_no_scope_fence() {
    let (db_name, pg, registry) = bootstrap().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner();
        let thing_id = Uuid::now_v7();
        register_thing(pg.pool_for_tests(), owner, thing_id).await?;

        let barrier_pool = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let mut barrier = barrier_pool.begin().await?;
        lock_scope_fence_exclusive_tx(&mut barrier, THING, &owner, thing_id).await?;

        let unscoped_db = db_name.clone();
        let unscoped_registry = registry.clone();
        let write = tokio::spawn(async move {
            let pg = PgStorage::connect(&db_url(&unscoped_db))
                .await
                .map_err(|err| err.to_string())?
                .with_sidecars(sidecars(&unscoped_registry))
                .with_flavors(&unscoped_registry);
            let engine = engine(&pg, &unscoped_registry);
            let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
            let payload = UnscopedFactV1 {
                logical_id: "unfenced".to_owned(),
            };
            engine
                .ingest_typed_fact_with(&authz, TypedFactIngest::new("test/scope-fence", &payload))
                .await
                .map(|outcome| outcome.memory_id.into_inner())
                .map_err(|err| err.to_string())
        });
        // A bound on the test, not a synchronization step: this write
        // either does not wait, in which case it finishes at once, or it
        // waits for a lock nothing will release before the barrier ends.
        let write = tokio::time::timeout(std::time::Duration::from_secs(45), write)
            .await
            .map_err(|_| "an unscoped admission blocked on a scope fence it must not take")??;
        write.expect("an unscoped admission is not fenced");

        barrier.rollback().await?;
        barrier_pool.close().await;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("an_unscoped_payload_takes_no_scope_fence failed");
}

/// The liveness probe: a scope with no registry row is a typed refusal, not
/// a write.
///
/// The probe is generated from `THING_DECL` and runs under the fence, so a
/// missing row is a settled fact rather than a race. The refusal names the
/// kind and the id, which is what a host needs to tell "you erased this"
/// from "you never registered it".
#[tokio::test]
async fn an_admission_naming_an_unregistered_scope_is_refused() {
    let (db_name, pg, registry) = bootstrap().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner();
        let thing_id = Uuid::now_v7();
        // Deliberately NOT registered.
        let engine = engine(&pg, &registry);
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let payload = ScopedFactV1 {
            logical_id: "orphan".to_owned(),
            thing_id,
        };
        let error = engine
            .ingest_typed_fact_with(&authz, TypedFactIngest::new("test/scope-fence", &payload))
            .await
            .expect_err("a write into an unregistered scope must be refused");
        assert_eq!(error.code, proxima_core::error::ErrorCode::NotFound);
        assert!(
            error.message.contains("scope not registered")
                && error.message.contains(THING.as_str())
                && error.message.contains(&thing_id.to_string()),
            "the refusal must name the kind and the id; got {}",
            error.message
        );
        let landed: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM test_scope.scoped_fact_v1")
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(landed, 0, "and nothing was written");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("an_admission_naming_an_unregistered_scope_is_refused failed");
}

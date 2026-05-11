// Each integration-test binary in `proxima-core` independently includes
// this module via `mod common;`. Items unused by a particular binary
// would otherwise trip `dead_code` even though another binary uses them.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use proxima_core::auth::NoAuth;
use proxima_core::engine::Engine;
use proxima_core::personality::{
    InstantiatePersonalityRequest, PersonalityInstanceId, SetWakeEntriesRequest, WakeEntryDraft,
};
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::schema::FlavorRegistryFrozen;
use proxima_core::wake::target_adapter::{
    TargetAdapter, TargetAdapterError, TargetInvocation, TargetOutcome, TargetOutcomeKind,
};
use proxima_core::{
    BindInferenceTierRequest, InferenceTargetConfig, LocalCliConfig, ModelTier, OrgId, Owner,
    Principal, RegisterInferenceTargetRequest, SchemaId, SchemaVersion, SourceBatchId, SourceId,
    StorageHandle, UserId, WakeEntryAuthoredBy, WakeEntryTriggerKind,
};
use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection, Row};
use uuid::Uuid;

/// Override via `PROXIMA_TEST_PG_URL` (e.g. the `docker-compose.dev.yml`
/// PG: `postgres://proxima:proxima@localhost/proxima`). The default
/// targets a peer-auth local PG with a `postgres` superuser, matching
/// the `proxima-storage-pg` test harness.
const DEFAULT_ADMIN_URL: &str = "postgres://postgres@localhost/postgres";

pub fn admin_url() -> String {
    std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| DEFAULT_ADMIN_URL.into())
}

pub fn db_url(name: &str) -> String {
    let admin = admin_url();
    match admin.rfind('/') {
        Some(idx) => format!("{}/{}", &admin[..idx], name),
        None => format!("{admin}/{name}"),
    }
}

pub fn owner_fixture() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::nil())),
        org_id: OrgId::new(Uuid::nil()),
    }
}

pub async fn fresh_pg() -> Option<(PgStorage, String)> {
    let db_name = format!("proxima_core_test_{}", Uuid::now_v7().simple());
    if create_db(&db_name).await.is_err() {
        eprintln!("skipping (no admin PG)");
        return None;
    }
    let url = db_url(&db_name);
    match PgStorage::connect(&url).await {
        Ok(pg) => Some((pg, db_name)),
        Err(err) => {
            let _ = drop_db(&db_name).await;
            eprintln!("skipping (PG unavailable): {err}");
            None
        }
    }
}

pub async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(&admin_url()).await?;
    conn.execute(format!("CREATE DATABASE \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

pub async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(&admin_url()).await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

/// Apply the test sidecar table for the wake-context fixture's
/// triggering Fact. Idempotent.
pub async fn apply_wake_context_sidecars(pool: &sqlx::PgPool) -> sqlx::Result<()> {
    pool.execute(
        "CREATE SCHEMA IF NOT EXISTS proxima_test; \
         CREATE TABLE IF NOT EXISTS proxima_test.wake_context_fact_v1 ( \
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id), \
             label text NOT NULL \
         );",
    )
    .await
    .map(|_| ())
}

/// Drop the test database when the fixture goes out of scope.
pub struct PgFixture {
    pub pg: PgStorage,
    pub db: String,
}

impl PgFixture {
    pub async fn cleanup(self) {
        drop(self.pg);
        let _ = drop_db(&self.db).await;
    }
}

/// Phase 1d Task 6 fixture: provision an owner, instantiate one
/// personality with a Root Perspective sidecar, ingest one Fact authored
/// by an external source, return the bits the wake-context assembler
/// needs.
///
/// Returns a tuple of `(storage, owner, instance_id, change_event_seq)`
/// matching the signature `assemble_wake_context` consumes.
///
/// Returns `None` if no test PG is available — callers should treat the
/// `None` arm as a skip (matches the `proxima-storage-pg` test pattern).
pub async fn seed_wake_context_fixture()
-> Option<(StorageHandle, Owner, PersonalityInstanceId, Uuid, PgFixture)> {
    let (pg, db) = fresh_pg().await?;
    pg.run_migrations().await.ok()?;
    apply_wake_context_sidecars(pg.pool()).await.ok()?;

    let owner = owner_fixture();

    // After Phase 2 Step 1, `instantiate_personality` writes the canonical
    // `proxima_core.root_personality_perspective_v1` sidecar directly.
    let response = pg
        .instantiate_personality(&InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Engineer Test Personality".into(),
            purpose: "exercise wake-context assembly with a non-empty system prompt".into(),
        })
        .await
        .ok()?;

    // Ingest one external Fact authored by `external/event-source` so
    // the change event has a known seq + a triggering memory id.
    let now = time::OffsetDateTime::now_utc();
    let payload = serde_json::to_vec(&serde_json::json!({
        "label": "wake-context-test-trigger",
    }))
    .expect("payload serializes");
    let draft = EventDraft {
        source_id: SourceId::new("proxima-test/wake-context-source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: owner.clone(),
        schema_id: SchemaId::new("proxima-test/wake-context-fact-v1".into()),
        schema_version: SchemaVersion::new(1),
        payload,
        observed_at: now,
        occurred_at: now,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new("proxima-test/wake-context-cited-v1".into()),
            schema_version: SchemaVersion::new(1),
            content_hash: rand_content_hash(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new("proxima-test/wake-context-citation-v1".into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    let outcome = pg.ingest_event_atomic(&draft).await.ok()?;

    // The fact's typed payload lives in `wake_context_fact_v1`; the
    // event_ingest verb only writes to `proxima_core.memories` + the
    // change_event row, so we backfill the sidecar manually here.
    pg.pool()
        .execute(
            sqlx::query(
                "INSERT INTO proxima_test.wake_context_fact_v1 (memory_id, label)
                 VALUES ($1, $2)
                 ON CONFLICT (memory_id) DO UPDATE SET label = EXCLUDED.label",
            )
            .bind(outcome.memory_id.into_inner())
            .bind("wake-context-test-trigger"),
        )
        .await
        .ok()?;

    let storage: StorageHandle = Arc::new(pg.clone());
    Some((
        storage,
        owner,
        response.instance_id,
        outcome.change_event_seq,
        PgFixture { pg, db },
    ))
}

/// `TargetAdapter` mock used by dispatch fixtures: counts invocations,
/// returns `Succeeded` without spawning anything. Identical in shape to
/// the inline mock in `wake_dispatch_idempotent.rs`; promoted here so
/// multiple dispatch tests can share it without a refactor.
#[derive(Debug, Clone)]
pub struct MockAdapter {
    pub calls: Arc<Mutex<usize>>,
}

impl MockAdapter {
    pub fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(0)),
        }
    }

    pub fn call_count(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

#[async_trait]
impl TargetAdapter for MockAdapter {
    async fn run(
        &self,
        _invocation: TargetInvocation,
    ) -> Result<TargetOutcome, TargetAdapterError> {
        *self.calls.lock().unwrap() += 1;
        Ok(TargetOutcome {
            kind: TargetOutcomeKind::Succeeded,
            turn_count: Some(1),
            exit_code: Some(0),
            duration_ms: 1,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            session_log_error: None,
        })
    }
}

/// Phase 1d Task 11 fixture: extends [`seed_wake_context_fixture`] with
/// the dispatch wiring `wake_dispatch_idempotent.rs` previously did
/// inline (inference target + tier binding, recipe on disk, wake entry,
/// `Engine` built with mock adapter), then wraps the engine in `Arc`.
///
/// The caller invokes `engine.start()` themselves — the smoke test for
/// the dispatcher loop wants to observe what `start` does, so the
/// fixture stops short of starting it. `mcp_url` is pre-set to a stub
/// so `fire_wake_entry` doesn't error with `mcp_listener_not_started`,
/// and `goose_bin` is pinned to `echo` so [`Engine::start`]'s
/// `verify_goose_on_path` self-check passes without needing a real
/// goose on PATH (the actual goose subprocess is never spawned because
/// the `MockAdapter` short-circuits the fire path).
///
/// Returns `None` when no test PG is reachable — callers should treat
/// the `None` arm as a skip, matching `seed_wake_context_fixture`.
pub struct DispatchEngineFixture {
    pub engine: Arc<Engine>,
    pub owner: Owner,
    pub instance_id: PersonalityInstanceId,
    pub change_event_seq: Uuid,
    pub wake_entry_id: Uuid,
    pub mock: MockAdapter,
    pub pg: PgFixture,
    // Recipes tempdir kept alive for the duration of the fixture so
    // `set_wake_entries`'s recipe-resolution layer keeps seeing the
    // file we just wrote.
    pub _recipes_dir: tempfile::TempDir,
}

impl DispatchEngineFixture {
    /// PG-side count of invocation rows for this fixture's
    /// (instance_id, wake_entry_id) pair. Independent of which
    /// `change_event_seq` the dispatcher landed on, so tests that just
    /// want to confirm "the loop fired at least once" don't have to
    /// re-derive the seq.
    pub async fn count_invocation_rows(&self) -> i64 {
        let row = sqlx::query(
            "SELECT COUNT(*) AS n FROM proxima_core.personality_wake_invocations \
             WHERE personality_instance_id = $1 AND wake_entry_id = $2",
        )
        .bind(self.instance_id.into_inner())
        .bind(self.wake_entry_id)
        .fetch_one(self.pg.pg.pool())
        .await
        .expect("count invocations");
        row.try_get::<i64, _>("n").expect("read count")
    }

    pub async fn cleanup(self) {
        self.pg.cleanup().await;
    }
}

/// Build a `DispatchEngineFixture` ready for `engine.start()`. See the
/// struct docs for what's wired and why. `dispatch_interval` controls
/// the engine builder's `with_dispatch_interval`; tests typically pick
/// 100ms so a `sleep(350ms)` window observes ≥2 ticks.
pub async fn seed_dispatch_fixture_with_match_and_engine(
    dispatch_interval: Duration,
) -> Option<DispatchEngineFixture> {
    let (storage, owner, instance_id, change_event_seq, pg) = seed_wake_context_fixture().await?;

    // 1. Inference target + tier binding so set_wake_entries +
    //    dispatcher resolve_target both succeed.
    let target_ref = "test/local-cli";
    storage
        .register_inference_target(&RegisterInferenceTargetRequest {
            owner: owner.clone(),
            target_ref: target_ref.into(),
            config: InferenceTargetConfig::LocalCli(LocalCliConfig {
                command: "echo".into(),
                profile: None,
                env_overrides: Vec::new(),
            }),
        })
        .await
        .expect("register target");
    storage
        .bind_inference_tier(&BindInferenceTierRequest {
            owner: owner.clone(),
            tier: ModelTier::Standard,
            target_ref: target_ref.into(),
        })
        .await
        .expect("bind tier");

    // 2. Recipe on disk under owner_recipes_root so
    //    resolve_recipe_ref("user:smoke.yaml") succeeds.
    let recipes_dir = tempfile::tempdir().expect("tempdir");
    let recipes_root = recipes_dir.path().to_path_buf();
    let principal_id = match &owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    let owner_recipes = recipes_root.join(principal_id.to_string());
    std::fs::create_dir_all(&owner_recipes).expect("mkdir owner recipes");
    let recipe_path = owner_recipes.join("smoke.yaml");
    std::fs::write(&recipe_path, b"name: smoke\nversion: 1\n").expect("write recipe");

    // 3. WakeEntry that matches the seeded fact's schema.
    let wake_entry_id = Uuid::now_v7();
    let wake_entry = WakeEntryDraft::new(
        wake_entry_id,
        instance_id,
        WakeEntryTriggerKind::OnMemory,
        "proxima-test/wake-context-fact-v1",
        "smoke-trigger",
        WakeEntryAuthoredBy::Any,
        1000, // probability_promille — always-fire
        "user:smoke.yaml",
        ModelTier::Standard,
        None,
        Vec::new(),
        4,
    )
    .expect("build wake entry");
    storage
        .set_wake_entries(&SetWakeEntriesRequest {
            owner: owner.clone(),
            personality_instance_id: instance_id,
            entries: vec![wake_entry],
        })
        .await
        .expect("set wake entries");

    // 4. Engine wired with: live PG handle, recipes_root, mock target
    //    adapter (no real goose), `echo` as the goose bin so the boot
    //    self-check passes (`echo --version` prints "--version" and
    //    exits 0), and the requested dispatch interval.
    let principal = owner.principal.clone();
    let resolver = NoAuth::new(principal, owner.clone());
    let mock = MockAdapter::new();
    let echo_bin = which::which("echo").expect("echo on PATH");
    let engine = Arc::new(
        Engine::new(
            FlavorRegistryFrozen::new(),
            MemoryStore::new(),
            Box::new(resolver),
        )
        .with_storage(storage.clone())
        .with_recipes_root(recipes_root)
        .with_target_adapter(Arc::new(mock.clone()) as Arc<dyn TargetAdapter>)
        .with_dispatch_interval(dispatch_interval)
        .with_goose_bin(echo_bin),
    );
    // `fire_wake_entry` reads `mcp_url()` and refuses to fire without
    // one. The fixture deliberately does NOT attach an MCP listener
    // (so `Engine::start` only spawns the dispatcher task — exactly
    // what the loop-driver smoke test wants to observe), so we stub
    // the URL via the test seam instead.
    engine
        .set_mcp_url("http://127.0.0.1:1/mcp".to_string())
        .await;

    Some(DispatchEngineFixture {
        engine,
        owner,
        instance_id,
        change_event_seq,
        wake_entry_id,
        mock,
        pg,
        _recipes_dir: recipes_dir,
    })
}

fn rand_content_hash() -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, byte) in Uuid::now_v7()
        .as_bytes()
        .iter()
        .chain(Uuid::now_v7().as_bytes().iter())
        .take(32)
        .enumerate()
    {
        out[i] = *byte;
    }
    out
}

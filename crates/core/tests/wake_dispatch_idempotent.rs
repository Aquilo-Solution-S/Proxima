//! Phase 1d Task 9: dispatch_tick over the same change-event window
//! twice creates exactly one invocation row per match. The PRIMARY KEY
//! on `personality_wake_invocations` (owner, instance, wake_entry_id,
//! change_event_seq) does the work — `start_wake_invocation` returns
//! `Ok(false)` on conflict and the dispatcher doesn't double-count.

#![allow(clippy::too_many_lines)]

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use proxima_core::auth::NoAuth;
use proxima_core::engine::Engine;
use proxima_core::personality::{SetWakeEntriesRequest, WakeEntryDraft};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::schema::FlavorRegistryFrozen;
use proxima_core::wake::target_adapter::{
    TargetAdapter, TargetAdapterError, TargetInvocation, TargetOutcome, TargetOutcomeKind,
};
use proxima_core::{
    InferenceTargetConfig, LocalCliConfig, ModelTier, RegisterInferenceTargetRequest,
    WakeEntryAuthoredBy, WakeEntryTriggerKind,
};
use sqlx::Row;
use uuid::Uuid;

#[derive(Debug, Clone)]
struct MockAdapter {
    calls: Arc<Mutex<usize>>,
}

impl MockAdapter {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(0)),
        }
    }
    fn call_count(&self) -> usize {
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
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_ticks_same_window_one_invocation_per_match() {
    let Some((storage, owner, instance_id, change_event_seq, fixture)) =
        common::seed_wake_context_fixture().await
    else {
        eprintln!("skipping: PG unavailable");
        return;
    };

    // 1. Register an inference target + tier binding so set_wake_entries
    //    + the dispatcher's resolve_target both succeed.
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
        .bind_inference_tier(&proxima_core::BindInferenceTierRequest {
            owner: owner.clone(),
            tier: ModelTier::Standard,
            target_ref: target_ref.into(),
        })
        .await
        .expect("bind tier");

    // 2. Drop a recipe under the engine's owner_recipes_root so
    //    resolve_recipe_ref(..."user:smoke.yaml") returns a real path.
    let recipe_dir = tempfile::tempdir().expect("tempdir");
    let recipes_root = recipe_dir.path().to_path_buf();
    let principal_id = match &owner.principal {
        proxima_core::Principal::User(u) => u.into_inner(),
        proxima_core::Principal::Group(g) => g.into_inner(),
    };
    let owner_recipes = recipes_root.join(principal_id.to_string());
    std::fs::create_dir_all(&owner_recipes).expect("mkdir owner recipes");
    let recipe_path = owner_recipes.join("smoke.yaml");
    std::fs::write(&recipe_path, b"name: smoke\nversion: 1\n").expect("write recipe");

    // 3. Append one WakeEntry that matches the seeded fact's schema.
    //    `seed_wake_context_fixture` ingests a Fact with schema_id
    //    "proxima-test/wake-context-fact-v1" — that's our trigger.
    let wake_entry = WakeEntryDraft::new(
        Uuid::now_v7(),
        instance_id,
        WakeEntryTriggerKind::OnMemory,
        "proxima-test/wake-context-fact-v1",
        "smoke-trigger",
        WakeEntryAuthoredBy::Any,
        1000, // probability_promille — always-fire
        "user:smoke.yaml",
        ModelTier::Standard,
        None,       // resolve via tier binding above
        Vec::new(), // empty palette is fine for this smoke
        4,
    )
    .expect("build wake entry");
    storage
        .set_wake_entries(&SetWakeEntriesRequest {
            owner: owner.clone(),
            personality_instance_id: instance_id,
            entries: vec![wake_entry.clone()],
        })
        .await
        .expect("set wake entries");

    // 4. Build engine with our mock target adapter + recipes_root +
    //    MCP URL stub. The mock storage is the live PG handle from
    //    seed_wake_context_fixture, so writes (invocation rows, cursor
    //    advance) actually land on disk where we can count them.
    let principal = owner.principal.clone();
    let resolver = NoAuth::new(principal, owner.clone());
    let mock = MockAdapter::new();
    let mock_for_assert = mock.clone();
    let engine = Arc::new(
        Engine::new(
            FlavorRegistryFrozen::new(),
            MemoryStore::new(),
            Box::new(resolver),
        )
        .with_storage(storage.clone())
        .with_recipes_root(recipes_root)
        .with_target_adapter(Arc::new(mock) as Arc<dyn TargetAdapter>),
    );
    engine
        .set_mcp_url("http://127.0.0.1:1/mcp".to_string())
        .await;

    // 5. First tick should fire one wake.
    let fired1 = engine.run_dispatcher_tick().await.expect("tick1");
    assert_eq!(fired1, 1, "first tick fires once");
    assert_eq!(mock_for_assert.call_count(), 1, "adapter ran once");

    // 6. Second tick over the same window should fire nothing — cursor
    //    advanced past `change_event_seq`, and even if it hadn't, the
    //    invocation PRIMARY KEY would dedupe.
    let fired2 = engine.run_dispatcher_tick().await.expect("tick2");
    assert_eq!(fired2, 0, "second tick fires nothing");
    assert_eq!(
        mock_for_assert.call_count(),
        1,
        "adapter not re-run on second tick"
    );

    // 7. PG-level row count: exactly one invocation row exists for
    //    this (instance, wake_entry, seq).
    let pool = fixture.pg.pool();
    let row = sqlx::query(
        "SELECT COUNT(*) AS n FROM proxima_core.personality_wake_invocations \
         WHERE personality_instance_id = $1 \
           AND wake_entry_id = $2 \
           AND change_event_seq = $3",
    )
    .bind(instance_id.into_inner())
    .bind(wake_entry.wake_entry_id)
    .bind(change_event_seq)
    .fetch_one(pool)
    .await
    .expect("count invocations");
    let n: i64 = row.try_get("n").expect("read count");
    assert_eq!(n, 1, "exactly one invocation row for the match");

    fixture.cleanup().await;
}

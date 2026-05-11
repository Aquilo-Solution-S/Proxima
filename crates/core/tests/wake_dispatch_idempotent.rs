//! Phase 1d Task 9: dispatch_tick over the same change-event window
//! twice creates exactly one invocation row per match. The PRIMARY KEY
//! on `personality_wake_invocations` (owner, instance, wake_entry_id,
//! change_event_seq) does the work — `start_wake_invocation` returns
//! `Ok(false)` on conflict and the dispatcher doesn't double-count.

#![allow(clippy::too_many_lines)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use proxima_core::auth::NoAuth;
use proxima_core::engine::Engine;
use proxima_core::personality::{ReplayWakeEventsRequest, SetWakeEntriesRequest, WakeEntryDraft};
use proxima_core::storage::Storage;
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::schema::FlavorRegistryFrozen;
use proxima_core::wake::target_adapter::{
    TargetAdapter, TargetAdapterError, TargetInvocation, TargetOutcome, TargetOutcomeKind,
};
use proxima_core::{
    InferenceTargetConfig, LocalCliConfig, ModelTier, RegisterInferenceTargetRequest,
    WakeEntryAuthoredBy, WakeEntryGoalScope, WakeEntryTriggerKind,
};
use sqlx::Row;
use uuid::Uuid;

#[derive(Debug, Clone)]
struct MockAdapter {
    calls: Arc<Mutex<usize>>,
}

#[tokio::test(flavor = "multi_thread")]
async fn assignment_scoped_entry_without_goal_id_records_failed_invocation() {
    let Some((storage, owner, instance_id, change_event_seq, fixture)) =
        common::seed_wake_context_fixture().await
    else {
        eprintln!("skipping: PG unavailable");
        return;
    };

    let mut wake_entry = WakeEntryDraft::new(
        Uuid::now_v7(),
        instance_id,
        WakeEntryTriggerKind::OnMemory,
        "proxima-test/wake-context-fact-v1",
        "goal-scoped-misconfigured",
        WakeEntryAuthoredBy::Any,
        1000,
        "user:unused.yaml",
        ModelTier::Standard,
        None,
        Vec::new(),
        4,
    )
    .expect("build wake entry");
    wake_entry.goal_scope = WakeEntryGoalScope::TriggerGoalAssigned;
    storage
        .set_wake_entries(&SetWakeEntriesRequest {
            owner: owner.clone(),
            personality_instance_id: instance_id,
            entries: vec![wake_entry.clone()],
        })
        .await
        .expect("set wake entries");

    let resolver = NoAuth::new(owner.principal.clone(), owner.clone());
    let engine = Arc::new(
        Engine::new(
            FlavorRegistryFrozen::new(),
            MemoryStore::new(),
            Box::new(resolver),
        )
        .with_storage(storage.clone()),
    );

    let fired = engine.run_dispatcher_tick().await.expect("tick");
    assert_eq!(fired, 0, "misconfigured scoped entry must not fire");

    let row = sqlx::query(
        "SELECT status, failure_reason
         FROM proxima_core.personality_wake_invocations
         WHERE personality_instance_id = $1
           AND wake_entry_id = $2
           AND change_event_seq = $3",
    )
    .bind(instance_id.into_inner())
    .bind(wake_entry.wake_entry_id)
    .bind(change_event_seq)
    .fetch_one(fixture.pg.pool())
    .await
    .expect("misconfiguration invocation row");
    let status: String = row.try_get("status").expect("status");
    let failure_reason: Option<String> = row.try_get("failure_reason").expect("reason");
    assert_eq!(status, "failed");
    assert_eq!(
        failure_reason.as_deref(),
        Some("wake_goal_scope_missing_goal_id"),
    );

    fixture.cleanup().await;
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
            session_log_error: None,
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

#[tokio::test(flavor = "multi_thread")]
async fn replay_missed_wake_fires_event_behind_cursor_without_moving_cursor() {
    let Some(fixture) =
        common::seed_dispatch_fixture_with_match_and_engine(Duration::from_secs(60)).await
    else {
        eprintln!("skipping: PG unavailable");
        return;
    };

    fixture
        .pg
        .pg
        .advance_wake_cursor(
            &fixture.owner,
            fixture.instance_id,
            fixture.change_event_seq,
        )
        .await
        .expect("advance cursor");
    let normal_tick = fixture.engine.run_dispatcher_tick().await.expect("tick");
    assert_eq!(normal_tick, 0, "cursor hides the historical event");
    assert_eq!(fixture.mock.call_count(), 0);

    let replay = fixture
        .engine
        .replay_missed_wakes(ReplayWakeEventsRequest {
            owner: fixture.owner.clone(),
            personality_instance_id: fixture.instance_id,
            wake_entry_id: Some(fixture.wake_entry_id),
            after_seq: Some(Uuid::nil()),
            until_seq: Some(fixture.change_event_seq),
            event_limit: 256,
            max_invocations: 1,
        })
        .await
        .expect("replay");
    assert_eq!(replay.started_invocations, 1);
    assert_eq!(replay.already_recorded, 0);
    assert_eq!(fixture.mock.call_count(), 1);

    let cursor: Uuid = sqlx::query_scalar(
        "SELECT last_considered_seq
         FROM proxima_core.personality_wake_cursor
         WHERE personality_instance_id = $1",
    )
    .bind(fixture.instance_id.into_inner())
    .fetch_one(fixture.pg.pg.pool())
    .await
    .expect("cursor");
    assert_eq!(
        cursor, fixture.change_event_seq,
        "replay must not mutate normal wake cursor"
    );

    let replay_again = fixture
        .engine
        .replay_missed_wakes(ReplayWakeEventsRequest {
            owner: fixture.owner.clone(),
            personality_instance_id: fixture.instance_id,
            wake_entry_id: Some(fixture.wake_entry_id),
            after_seq: Some(Uuid::nil()),
            until_seq: Some(fixture.change_event_seq),
            event_limit: 256,
            max_invocations: 1,
        })
        .await
        .expect("replay again");
    assert_eq!(replay_again.started_invocations, 0);
    assert_eq!(replay_again.already_recorded, 1);
    assert_eq!(
        fixture.mock.call_count(),
        1,
        "replay must not run adapter for existing invocation row"
    );

    fixture.cleanup().await;
}

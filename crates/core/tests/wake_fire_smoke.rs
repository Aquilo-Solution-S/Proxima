//! Phase 1d Task 8: fire_wake_entry on a single matched entry mints a
//! wake token, INSERTs an invocation row, drives a mock TargetAdapter,
//! and finalizes status. Workspace mode short-circuits with
//! `failure_reason = workspace_mode_not_yet_implemented`. Self-wake is
//! a defense-in-depth `Ok(false)` (the dispatcher's authored_by filter
//! is the primary guard).
//!
//! This test runs against a tightly-scoped in-memory mock storage —
//! the PG roundtrip is exercised by Tasks 9/12. The mock implements
//! only the trait methods `fire_wake_entry` actually calls; the
//! remaining methods either delegate to `NoopStorage` defaults or
//! `unimplemented!` so a future contract drift trips loudly.

#![allow(clippy::too_many_lines)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use proxima_core::FlavorRegistry;
use proxima_core::auth::NoAuth;
use proxima_core::engine::Engine;
use proxima_core::outbox::{ChangeEvent, ChangeEventKind, EntityKind, EntityRef};
use proxima_core::personality::workspace::{
    WorkspaceFinalizeInput, WorkspacePrepareInput, WorkspacePreparedRun, WorkspaceRunRecord,
    WorkspaceRunner, WorkspaceRunnerError,
};
use proxima_core::personality::{
    ChangeEventForWake, MemorySnapshot, PersonalityInstanceId, PersonalityRuntimeRow,
    RootPersonalityPerspectiveRow, SidecarSpec, WakeChainDepth, WakeEntryAuthoredBy,
    WakeEntryExecutionMode, WakeEntryRow, WakeEntryTriggerKind, WakeInvocationFinalize,
    WakeInvocationStart, WakeInvocationStatus,
};
use proxima_core::storage::{Storage, StorageError, WakeLockGuard};
use proxima_core::verbs::close_batch::CloseBatchOutcome;
use proxima_core::verbs::event_history::{EventHistoryRequest, EventHistoryResponse};
use proxima_core::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use proxima_core::verbs::goal_write::{GoalDraft, GoalWriteOutcome};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::query::{QueryRequest, QueryResponse};
use proxima_core::verbs::schema::SchemaInfo;
use proxima_core::wake::fire::{FireWakeEntryInput, fire_wake_entry};
use proxima_core::wake::target_adapter::{
    TargetAdapter, TargetAdapterError, TargetInvocation, TargetOutcome, TargetOutcomeKind,
};
use proxima_core::{
    ActiveGoalSummary, BindInferenceTierRequest, BindInferenceTierResponse, ChangeEventStream,
    InferenceTargetConfig, InferenceTargetRow, InferenceTierBindingRow,
    InstantiatePersonalityRequest, InstantiatePersonalityResponse, LocalCliConfig, MemoryId,
    ModelTier, OrgId, Owner, PersonalityRef, PersonalityWriteOutcome, PersonalityWriteRequest,
    Principal, RegisterInferenceTargetRequest, RegisterInferenceTargetResponse,
    RemoveInferenceTargetRequest, RemoveInferenceTargetResponse, SchemaId, SchemaVersion,
    SetWakeEntriesRequest, SetWakeEntriesResponse, SourceBatchId, TombstonePersonalityRequest,
    TombstonePersonalityResponse, UserId, WakeDispatchEntryRow,
};
use uuid::Uuid;

// ---------- Stub WorkspaceRunner ----------
//
// Returns Unimplemented so workspace-mode wakes finalize with
// `failure_reason = "workspace_mode_not_yet_implemented"` via the
// Phase 1 dispatch path in `wake/fire.rs`.

#[derive(Debug, Default)]
struct StubWorkspaceRunner;

#[async_trait]
impl WorkspaceRunner for StubWorkspaceRunner {
    async fn prepare(
        &self,
        _input: WorkspacePrepareInput<'_>,
    ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
        Err(WorkspaceRunnerError::Unimplemented)
    }

    async fn finalize(
        &self,
        _input: WorkspaceFinalizeInput<'_>,
    ) -> Result<WorkspaceRunRecord, WorkspaceRunnerError> {
        Err(WorkspaceRunnerError::Unimplemented)
    }
}

// ---------- Mock TargetAdapter ----------

#[derive(Debug, Clone)]
struct MockAdapter {
    calls: Arc<Mutex<Vec<TargetInvocation>>>,
    outcome_kind: TargetOutcomeKind,
}

impl MockAdapter {
    fn new(outcome_kind: TargetOutcomeKind) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            outcome_kind,
        }
    }
}

#[async_trait]
impl TargetAdapter for MockAdapter {
    async fn run(&self, invocation: TargetInvocation) -> Result<TargetOutcome, TargetAdapterError> {
        self.calls.lock().unwrap().push(invocation);
        Ok(TargetOutcome {
            kind: self.outcome_kind,
            turn_count: Some(2),
            exit_code: Some(0),
            duration_ms: 25,
            stdout_tail: "mock stdout".into(),
            stderr_tail: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
        })
    }
}

// ---------- Mock Storage ----------

#[derive(Debug, Clone)]
struct InvocationRowSnapshot {
    status: WakeInvocationStatus,
    wake_token: Uuid,
    recipe_sha256: String,
    resolved_inference_target_ref: String,
    turn_count: Option<u16>,
    failure_reason: Option<String>,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    stdout_tail: Option<String>,
    stderr_tail: Option<String>,
}

#[derive(Debug)]
struct MockStorage {
    owner: Owner,
    instance_id: PersonalityInstanceId,
    root_memory_id: MemoryId,
    triggering_memory_id: MemoryId,
    change_event_seq: Uuid,
    triggering_author: Mutex<Option<Uuid>>,
    targets: Vec<InferenceTargetRow>,
    bindings: Vec<InferenceTierBindingRow>,
    invocation: Mutex<Option<InvocationRowSnapshot>>,
}

impl MockStorage {
    fn fetch_invocation(&self) -> InvocationRowSnapshot {
        self.invocation
            .lock()
            .unwrap()
            .clone()
            .expect("invocation row recorded")
    }
}

#[async_trait]
impl Storage for MockStorage {
    async fn ingest_event_atomic(
        &self,
        _draft: &EventDraft,
    ) -> Result<EventIngestOutcome, StorageError> {
        unimplemented!("mock")
    }

    async fn write_goal_atomic(
        &self,
        _draft: &GoalDraft,
    ) -> Result<GoalWriteOutcome, StorageError> {
        unimplemented!("mock")
    }

    async fn supersede_goal_atomic(
        &self,
        _prior: proxima_core::GoalId,
        _draft: &GoalDraft,
    ) -> Result<GoalWriteOutcome, StorageError> {
        unimplemented!("mock")
    }

    async fn subscribe_changes(
        &self,
        _owner: &Owner,
        _since: Option<Uuid>,
    ) -> Result<ChangeEventStream, StorageError> {
        Ok(Box::pin(futures_util::stream::empty()))
    }

    async fn event_history(
        &self,
        _req: &EventHistoryRequest,
    ) -> Result<EventHistoryResponse, StorageError> {
        Ok(EventHistoryResponse {
            events: Vec::new(),
            seq_high_water: None,
        })
    }

    async fn query_memories(
        &self,
        _req: &QueryRequest,
        _schemas: &[SchemaInfo],
    ) -> Result<QueryResponse, StorageError> {
        unimplemented!("mock")
    }

    async fn list_active_goals(
        &self,
        _owner: &Owner,
        _self_perspective_memory_id: MemoryId,
        _limit: usize,
    ) -> Result<Vec<ActiveGoalSummary>, StorageError> {
        Ok(Vec::new())
    }

    async fn close_batch(
        &self,
        _owner: &Owner,
        _source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, StorageError> {
        unimplemented!("mock")
    }

    async fn register_inference_target(
        &self,
        _req: &RegisterInferenceTargetRequest,
    ) -> Result<RegisterInferenceTargetResponse, StorageError> {
        unimplemented!("mock")
    }

    async fn list_inference_targets(
        &self,
        _owner: &Owner,
    ) -> Result<Vec<InferenceTargetRow>, StorageError> {
        Ok(self.targets.clone())
    }

    async fn remove_inference_target(
        &self,
        _req: &RemoveInferenceTargetRequest,
    ) -> Result<RemoveInferenceTargetResponse, StorageError> {
        unimplemented!("mock")
    }

    async fn bind_inference_tier(
        &self,
        _req: &BindInferenceTierRequest,
    ) -> Result<BindInferenceTierResponse, StorageError> {
        unimplemented!("mock")
    }

    async fn unbind_inference_tier(
        &self,
        _owner: &Owner,
        _tier: ModelTier,
    ) -> Result<(), StorageError> {
        unimplemented!("mock")
    }

    async fn list_inference_tier_bindings(
        &self,
        _owner: &Owner,
    ) -> Result<Vec<InferenceTierBindingRow>, StorageError> {
        Ok(self.bindings.clone())
    }

    async fn list_personality_instances(
        &self,
        _owner: &Owner,
        _include_tombstoned: bool,
    ) -> Result<Vec<proxima_core::PersonalityInstanceRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn tombstone_personality(
        &self,
        _req: &TombstonePersonalityRequest,
    ) -> Result<TombstonePersonalityResponse, StorageError> {
        unimplemented!("mock")
    }

    async fn instantiate_personality(
        &self,
        _req: &InstantiatePersonalityRequest,
    ) -> Result<InstantiatePersonalityResponse, StorageError> {
        unimplemented!("mock")
    }

    async fn set_wake_entries(
        &self,
        _req: &SetWakeEntriesRequest,
    ) -> Result<SetWakeEntriesResponse, StorageError> {
        unimplemented!("mock")
    }

    async fn ensure_master_token_personality(
        &self,
        _owner: &Owner,
        _master_token_id: uuid::Uuid,
    ) -> Result<proxima_core::MasterTokenPersonality, proxima_core::StorageError> {
        Err(proxima_core::StorageError::Internal(
            "mock: ensure_master_token_personality not stubbed".into(),
        ))
    }

    async fn set_wake_entries_within(
        &self,
        _owner: &Owner,
        _personality_instance_id: proxima_core::PersonalityInstanceId,
        _mutate: proxima_core::WakeEntriesMutator,
    ) -> Result<SetWakeEntriesResponse, StorageError> {
        unimplemented!("mock")
    }

    async fn list_active_wake_entries(&self) -> Result<Vec<WakeDispatchEntryRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn list_change_events_after(
        &self,
        _owner: &Owner,
        _after: Uuid,
        _limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        Ok(Vec::new())
    }

    async fn advance_wake_cursor(
        &self,
        _owner: &Owner,
        _instance: PersonalityInstanceId,
        _last_considered_seq: Uuid,
    ) -> Result<(), StorageError> {
        Ok(())
    }

    async fn try_begin_wake_invocation(
        &self,
        _owner: &Owner,
        _instance: PersonalityInstanceId,
        _wake_entry_id: Uuid,
        _change_event_seq: Uuid,
    ) -> Result<bool, StorageError> {
        Ok(true)
    }

    async fn start_wake_invocation(
        &self,
        start: &WakeInvocationStart,
    ) -> Result<bool, StorageError> {
        let mut slot = self.invocation.lock().unwrap();
        *slot = Some(InvocationRowSnapshot {
            status: WakeInvocationStatus::Running,
            wake_token: start.wake_token,
            recipe_sha256: start.recipe_sha256.clone(),
            resolved_inference_target_ref: start.resolved_inference_target_ref.clone(),
            turn_count: None,
            failure_reason: None,
            exit_code: None,
            duration_ms: None,
            stdout_tail: None,
            stderr_tail: None,
        });
        Ok(true)
    }

    async fn finish_wake_invocation(
        &self,
        _owner: &Owner,
        _instance: PersonalityInstanceId,
        _wake_entry_id: Uuid,
        _change_event_seq: Uuid,
        _status: WakeInvocationStatus,
        _turn_count: u16,
        _cost_usd: f64,
    ) -> Result<(), StorageError> {
        Ok(())
    }

    async fn finalize_wake_invocation(
        &self,
        finalize: &WakeInvocationFinalize,
    ) -> Result<(), StorageError> {
        let mut slot = self.invocation.lock().unwrap();
        if let Some(row) = slot.as_mut() {
            row.status = finalize.status;
            row.turn_count = finalize.turn_count;
            row.failure_reason = finalize.failure_reason.clone();
            row.exit_code = finalize.exit_code;
            row.duration_ms = finalize.duration_ms;
            row.stdout_tail = finalize.stdout_tail.clone();
            row.stderr_tail = finalize.stderr_tail.clone();
        }
        Ok(())
    }

    async fn load_memory_batch_facts(
        &self,
        _owner: &Owner,
        _memory_id: MemoryId,
        _sidecars: &[SidecarSpec],
    ) -> Result<Vec<proxima_core::FactRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_abstraction_heads(
        &self,
        _owner: &Owner,
        _sidecars: &[SidecarSpec],
        _limit: usize,
    ) -> Result<Vec<proxima_core::AbstractionRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn lookup_prior_personality_head(
        &self,
        _owner: &Owner,
        _instance: &PersonalityRef,
        _schema_id: &SchemaId,
    ) -> Result<Option<MemoryId>, StorageError> {
        Ok(None)
    }

    async fn append_personality_memories(
        &self,
        _req: &PersonalityWriteRequest<'_>,
    ) -> Result<PersonalityWriteOutcome, StorageError> {
        unimplemented!("mock")
    }

    async fn load_memory_by_id(
        &self,
        _owner: &Owner,
        memory_id: MemoryId,
        _sidecars: &[SidecarSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError> {
        if memory_id == self.triggering_memory_id {
            Ok(Some(MemorySnapshot {
                memory_id,
                kind: "Fact".to_string(),
                schema_id: SchemaId::new("proxima-test/wake-fire-fact-v1".into()),
                schema_version: SchemaVersion::new(1),
                text: Some("smoke trigger".to_string()),
                wake_chain_depth: WakeChainDepth::new(0),
                payload_json: serde_json::json!({"label": "smoke"}),
            }))
        } else {
            Ok(None)
        }
    }

    async fn fetch_personality_runtime(
        &self,
        _owner: &Owner,
        instance_id: PersonalityInstanceId,
    ) -> Result<Option<PersonalityRuntimeRow>, StorageError> {
        if instance_id == self.instance_id {
            Ok(Some(PersonalityRuntimeRow {
                owner: self.owner.clone(),
                personality_instance_id: instance_id,
                current_root_perspective_memory_id: self.root_memory_id,
                display_name: "Fire Test Engineer".into(),
                status: "active".into(),
            }))
        } else {
            Ok(None)
        }
    }

    async fn fetch_root_personality_perspective(
        &self,
        _owner: &Owner,
        memory_id: MemoryId,
    ) -> Result<Option<RootPersonalityPerspectiveRow>, StorageError> {
        if memory_id == self.root_memory_id {
            Ok(Some(RootPersonalityPerspectiveRow {
                memory_id,
                display_name: "Fire Test Engineer".into(),
                purpose: "smoke-test the wake fire path".into(),
            }))
        } else {
            Ok(None)
        }
    }

    async fn fetch_change_event_for_wake(
        &self,
        owner: &Owner,
        seq: Uuid,
    ) -> Result<Option<ChangeEventForWake>, StorageError> {
        if seq != self.change_event_seq {
            return Ok(None);
        }
        let author = *self.triggering_author.lock().unwrap();
        Ok(Some(ChangeEventForWake {
            event: ChangeEvent {
                seq,
                owner: owner.clone(),
                kind: ChangeEventKind::EntityAppend {
                    entity_kind: EntityKind::Fact,
                    entity: EntityRef::Memory(self.triggering_memory_id),
                    schema_id: SchemaId::new("proxima-test/wake-fire-fact-v1".into()),
                    schema_version: SchemaVersion::new(1),
                    supersedes: None,
                },
                authoring_personality_instance_id: author,
                wake_chain_depth: 0,
            },
            authoring_personality_instance_id: author.map(PersonalityInstanceId::new),
            wake_chain_depth: WakeChainDepth::new(0),
        }))
    }

    async fn acquire_wake_lock(
        &self,
        _owner: &Owner,
        _instance: &PersonalityRef,
    ) -> Result<WakeLockGuard, StorageError> {
        Ok(WakeLockGuard::noop())
    }
}

// ---------- Fixture ----------

struct FireFixture {
    engine: Arc<Engine>,
    mock: Arc<MockStorage>,
    owner: Owner,
    instance_id: PersonalityInstanceId,
    wake_entry: WakeEntryRow,
    change_event_seq: Uuid,
    triggering_memory_id: Uuid,
    _recipe_dir: tempfile::TempDir,
}

impl FireFixture {
    async fn build() -> Self {
        let owner = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let instance_id = PersonalityInstanceId::new(Uuid::now_v7());
        let root_memory_id = MemoryId::new(Uuid::now_v7());
        let triggering_memory_id = MemoryId::new(Uuid::now_v7());
        let change_event_seq = Uuid::now_v7();

        let target_ref = "test/local-cli";
        let targets = vec![InferenceTargetRow {
            owner: owner.clone(),
            target_ref: target_ref.into(),
            config: InferenceTargetConfig::LocalCli(LocalCliConfig {
                command: "echo".into(),
                profile: Some("test".into()),
                env_overrides: vec![("FOO".into(), "bar".into())],
            }),
            created_at: time::OffsetDateTime::now_utc(),
            updated_at: time::OffsetDateTime::now_utc(),
        }];
        let bindings = vec![InferenceTierBindingRow {
            owner: owner.clone(),
            tier: ModelTier::Standard,
            target_ref: target_ref.into(),
        }];

        let mock = Arc::new(MockStorage {
            owner: owner.clone(),
            instance_id,
            root_memory_id,
            triggering_memory_id,
            change_event_seq,
            triggering_author: Mutex::new(None),
            targets,
            bindings,
            invocation: Mutex::new(None),
        });

        // Write a recipe under owner_recipes_root so resolve_recipe_ref
        // can find it via the `user:` scheme.
        let recipe_dir = tempfile::tempdir().expect("tempdir");
        let recipes_root = recipe_dir.path().to_path_buf();
        let principal_id = match &owner.principal {
            Principal::User(u) => u.into_inner(),
            Principal::Group(g) => g.into_inner(),
        };
        let owner_recipes = recipes_root.join(principal_id.to_string());
        std::fs::create_dir_all(&owner_recipes).expect("mkdir owner recipes");
        let recipe_path = owner_recipes.join("smoke.yaml");
        std::fs::write(&recipe_path, b"name: smoke\nversion: 1\n").expect("write recipe");

        let principal = owner.principal.clone();
        let resolver = NoAuth::new(principal, owner.clone());
        // Register a stub WorkspaceRunner under the `proxima-test`
        // flavor so workspace-mode wakes (whose trigger_id starts
        // with `proxima-test/`) hit the runner-dispatch path in
        // wake/fire.rs and route to `Unimplemented`. Without this,
        // the dispatch falls into the `NoRunner` arm with a
        // different `failure_reason`.
        let mut registry = FlavorRegistry::new();
        registry.add_workspace_runner(
            "proxima-test",
            Arc::new(StubWorkspaceRunner) as Arc<dyn WorkspaceRunner>,
        );
        registry.add_workspace_trigger("proxima-test/wake-fire-fact-v1");
        let frozen_registry = registry.freeze();
        let engine = Arc::new(
            Engine::new(frozen_registry, MemoryStore::new(), Box::new(resolver))
                .with_storage(mock.clone() as Arc<dyn Storage>)
                .with_recipes_root(recipes_root),
        );
        engine
            .set_mcp_url("http://127.0.0.1:1/mcp".to_string())
            .await;

        let wake_entry = WakeEntryRow {
            wake_entry_id: Uuid::now_v7(),
            trigger_kind: WakeEntryTriggerKind::OnMemory,
            trigger_id: "proxima-test/wake-fire-fact-v1".into(),
            label: "smoke".into(),
            enabled: true,
            execution_mode: WakeEntryExecutionMode::SubstrateOnly,
            authored_by: WakeEntryAuthoredBy::Any,
            probability_promille: 1000,
            recipe_ref: "user:smoke.yaml".into(),
            model_tier: ModelTier::Standard,
            inference_target_ref: None,
            substrate_tool_palette: vec!["proxima-core/append-event".into()],
            workspace_tool_palette: Vec::new(),
            max_rounds: 4,
            disabled_reason: None,
        };

        Self {
            engine,
            mock,
            owner,
            instance_id,
            wake_entry,
            change_event_seq,
            triggering_memory_id: triggering_memory_id.into_inner(),
            _recipe_dir: recipe_dir,
        }
    }

    fn input(&self) -> FireWakeEntryInput {
        FireWakeEntryInput {
            owner: self.owner.clone(),
            personality_instance_id: self.instance_id,
            wake_entry: self.wake_entry.clone(),
            change_event_seq: self.change_event_seq,
            triggering_memory_id: self.triggering_memory_id,
        }
    }
}

// ---------- Tests ----------

#[tokio::test(flavor = "multi_thread")]
async fn fires_single_entry_writes_invocation_row_and_finalizes() {
    let fixture = FireFixture::build().await;
    let adapter = MockAdapter::new(TargetOutcomeKind::Succeeded);

    let fired = fire_wake_entry(&fixture.engine, &adapter, fixture.input())
        .await
        .expect("fire ok");
    assert!(fired);

    let row = fixture.mock.fetch_invocation();
    assert_eq!(row.status, WakeInvocationStatus::Succeeded);
    assert_ne!(row.wake_token, Uuid::nil());
    assert!(!row.recipe_sha256.is_empty(), "recipe sha computed");
    assert_eq!(row.resolved_inference_target_ref, "test/local-cli");
    assert_eq!(row.turn_count, Some(2));
    assert!(row.failure_reason.is_none());
    let calls = adapter.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "adapter ran once");
    let invocation = &calls[0];
    assert_eq!(invocation.max_rounds, 4);
    assert!(invocation.params.contains_key("root_perspective"));
    assert!(invocation.params.contains_key("active_goals"));
    assert!(invocation.params.contains_key("trigger_event"));
    assert!(invocation.params.contains_key("triggering_memory"));
    assert!(invocation.env.contains_key("PROXIMA_WAKE_TOKEN"));
    assert_eq!(
        invocation.env.get("PROXIMA_MCP_URL").map(String::as_str),
        Some("http://127.0.0.1:1/mcp")
    );
    assert_eq!(
        invocation.env.get("GOOSE_PROFILE").map(String::as_str),
        Some("test")
    );
    assert_eq!(invocation.env.get("FOO").map(String::as_str), Some("bar"));
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_mode_fails_with_failure_reason() {
    let mut fixture = FireFixture::build().await;
    fixture.wake_entry.execution_mode = WakeEntryExecutionMode::Workspace;
    let adapter = MockAdapter::new(TargetOutcomeKind::Succeeded);

    let fired = fire_wake_entry(&fixture.engine, &adapter, fixture.input())
        .await
        .expect("fire ok");
    assert!(fired);

    let row = fixture.mock.fetch_invocation();
    assert_eq!(row.status, WakeInvocationStatus::Failed);
    assert_eq!(
        row.failure_reason.as_deref(),
        Some("workspace_mode_not_yet_implemented")
    );
    assert_eq!(adapter.calls.lock().unwrap().len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn self_wake_is_skipped() {
    let fixture = FireFixture::build().await;
    *fixture.mock.triggering_author.lock().unwrap() = Some(fixture.instance_id.into_inner());
    let adapter = MockAdapter::new(TargetOutcomeKind::Succeeded);

    let fired = fire_wake_entry(&fixture.engine, &adapter, fixture.input())
        .await
        .expect("fire ok");
    assert!(!fired, "self-wake skipped");
    assert!(fixture.mock.invocation.lock().unwrap().is_none());
    assert_eq!(adapter.calls.lock().unwrap().len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn truncated_outcome_records_max_rounds_reason() {
    let fixture = FireFixture::build().await;
    let adapter = MockAdapter::new(TargetOutcomeKind::Truncated);

    let fired = fire_wake_entry(&fixture.engine, &adapter, fixture.input())
        .await
        .expect("fire ok");
    assert!(fired);

    let row = fixture.mock.fetch_invocation();
    assert_eq!(row.status, WakeInvocationStatus::Truncated);
    assert_eq!(row.failure_reason.as_deref(), Some("max_rounds_reached"));
}

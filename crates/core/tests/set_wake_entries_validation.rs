//! Typed-error paths through the set_wake_entries validation pipeline.

use std::sync::Mutex;

use async_trait::async_trait;
use proxima_core::inference::set_wake_entries::{SetWakeEntriesContext, set_wake_entries};
use proxima_core::storage::{Storage, StorageError, WakeLockGuard};
use proxima_core::verbs::close_batch::CloseBatchOutcome;
use proxima_core::verbs::event_history::{EventHistoryRequest, EventHistoryResponse};
use proxima_core::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use proxima_core::verbs::goal_write::{GoalDraft, GoalWriteOutcome};
use proxima_core::verbs::query::{
    MemoryLineageRequest, MemoryLineageResponse, MemorySearchRequest, MemorySearchResult,
    QueryRequest, QueryResponse,
};
use proxima_core::verbs::schema::SchemaInfo;
use proxima_core::verbs::subscribe::ChangeEventStream;
use proxima_core::{
    AbstractionRow, ActiveGoalSummary, BindInferenceTierRequest, BindInferenceTierResponse,
    ChangeEventForWake, ErrorCode, FactRow, FlavorRegistry, InferenceTargetRow,
    InferenceTierBindingRow, InstantiatePersonalityRequest, InstantiatePersonalityResponse,
    ListReadScopeRequest, ListReadScopeResponse, MemoryId, MemorySnapshot, ModelTier, OrgId, Owner,
    PersonalityInstanceId, PersonalityInstanceRow, PersonalityRef, PersonalityRuntimeRow,
    PersonalityWriteOutcome, PersonalityWriteRequest, Principal, RegisterInferenceTargetRequest,
    RegisterInferenceTargetResponse, RemoveInferenceTargetRequest, RemoveInferenceTargetResponse,
    RootPersonalityPerspectiveRow, SetReadScopeRequest, SetReadScopeResponse,
    SetWakeEntriesRequest, SetWakeEntriesResponse, SidecarSpec, SourceBatchId,
    TombstonePersonalityRequest, TombstonePersonalityResponse, UserId, WakeDispatchEntryRow,
    WakeEntryAuthoredBy, WakeEntryDraft, WakeEntryTriggerKind, WakeExecutionMode,
    WakeInvocationFinalize, WakeInvocationStart, WakeInvocationStatus, WakeWorkspaceBinding,
    WakeWorkspaceFinalize,
};
use uuid::Uuid;

#[derive(Default)]
struct FixtureStorage {
    targets: Vec<InferenceTargetRow>,
    bindings: Vec<InferenceTierBindingRow>,
    set_calls: Mutex<usize>,
}

#[async_trait]
impl Storage for FixtureStorage {
    async fn ingest_event_atomic(
        &self,
        _draft: &EventDraft,
    ) -> Result<EventIngestOutcome, StorageError> {
        Err(StorageError::Internal("unused".into()))
    }

    async fn persist_wake_trace_atomic(
        &self,
        _registry: &proxima_core::FlavorRegistryFrozen,
        _input: &proxima_core::verbs::persist_wake_trace::WakeTracePersistInput,
    ) -> Result<proxima_core::verbs::persist_wake_trace::WakeTracePersistOutcome, StorageError>
    {
        Err(StorageError::Internal("unused".into()))
    }

    async fn write_goal_atomic(
        &self,
        _draft: &GoalDraft,
    ) -> Result<GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal("unused".into()))
    }

    async fn supersede_goal_atomic(
        &self,
        _prior: proxima_core::GoalId,
        _draft: &GoalDraft,
    ) -> Result<GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal("unused".into()))
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
        Err(StorageError::Internal("unused".into()))
    }

    async fn query_memories(
        &self,
        _req: &QueryRequest,
        _schemas: &[SchemaInfo],
    ) -> Result<QueryResponse, StorageError> {
        Err(StorageError::Internal("unused".into()))
    }

    async fn search_memories(
        &self,
        _req: &MemorySearchRequest,
        _schemas: &[SchemaInfo],
    ) -> Result<Vec<MemorySearchResult>, StorageError> {
        Ok(Vec::new())
    }

    async fn walk_memory_lineage(
        &self,
        _req: &MemoryLineageRequest,
    ) -> Result<MemoryLineageResponse, StorageError> {
        Ok(MemoryLineageResponse {
            nodes: Vec::new(),
            edges: Vec::new(),
            truncated: false,
        })
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
        Err(StorageError::Internal("unused".into()))
    }

    async fn register_inference_target(
        &self,
        _req: &RegisterInferenceTargetRequest,
    ) -> Result<RegisterInferenceTargetResponse, StorageError> {
        Err(StorageError::Internal("unused".into()))
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
        Err(StorageError::Internal("unused".into()))
    }

    async fn bind_inference_tier(
        &self,
        _req: &BindInferenceTierRequest,
    ) -> Result<BindInferenceTierResponse, StorageError> {
        Err(StorageError::Internal("unused".into()))
    }

    async fn unbind_inference_tier(
        &self,
        _owner: &Owner,
        _tier: ModelTier,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal("unused".into()))
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
    ) -> Result<Vec<PersonalityInstanceRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn tombstone_personality(
        &self,
        _req: &TombstonePersonalityRequest,
    ) -> Result<TombstonePersonalityResponse, StorageError> {
        Err(StorageError::Internal("unused".into()))
    }

    async fn instantiate_personality(
        &self,
        _req: &InstantiatePersonalityRequest,
    ) -> Result<InstantiatePersonalityResponse, StorageError> {
        Err(StorageError::Internal("unused".into()))
    }

    async fn set_wake_entries(
        &self,
        _req: &SetWakeEntriesRequest,
    ) -> Result<SetWakeEntriesResponse, StorageError> {
        *self.set_calls.lock().unwrap() += 1;
        Ok(SetWakeEntriesResponse { active_entries: 0 })
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
        Err(StorageError::Internal("unused".into()))
    }

    async fn list_read_scope(
        &self,
        _req: &ListReadScopeRequest,
    ) -> Result<ListReadScopeResponse, StorageError> {
        Ok(ListReadScopeResponse {
            readable_personality_instance_ids: Vec::new(),
        })
    }

    async fn set_read_scope(
        &self,
        _req: &SetReadScopeRequest,
    ) -> Result<SetReadScopeResponse, StorageError> {
        Err(StorageError::Internal("unused".into()))
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
        Ok(false)
    }

    async fn start_wake_invocation(
        &self,
        _start: &WakeInvocationStart,
    ) -> Result<bool, StorageError> {
        Ok(false)
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
        _finalize: &WakeInvocationFinalize,
    ) -> Result<(), StorageError> {
        Ok(())
    }

    async fn load_memory_batch_facts(
        &self,
        _owner: &Owner,
        _memory_id: MemoryId,
        _sidecars: &[SidecarSpec],
    ) -> Result<Vec<FactRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_abstraction_heads(
        &self,
        _owner: &Owner,
        _sidecars: &[SidecarSpec],
        _limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn lookup_prior_personality_head(
        &self,
        _owner: &Owner,
        _instance: &PersonalityRef,
        _schema_id: &proxima_core::SchemaId,
    ) -> Result<Option<MemoryId>, StorageError> {
        Ok(None)
    }

    async fn append_personality_memories(
        &self,
        _req: &PersonalityWriteRequest<'_>,
    ) -> Result<PersonalityWriteOutcome, StorageError> {
        Err(StorageError::Internal("unused".into()))
    }

    async fn load_memory_by_id(
        &self,
        _owner: &Owner,
        _memory_id: MemoryId,
        _reader_personality_instance_id: Option<PersonalityInstanceId>,
        _sidecars: &[SidecarSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError> {
        Ok(None)
    }

    async fn fetch_personality_runtime(
        &self,
        _owner: &Owner,
        _instance_id: PersonalityInstanceId,
    ) -> Result<Option<PersonalityRuntimeRow>, StorageError> {
        Ok(None)
    }

    async fn fetch_root_personality_perspective(
        &self,
        _owner: &Owner,
        _memory_id: MemoryId,
    ) -> Result<Option<RootPersonalityPerspectiveRow>, StorageError> {
        Ok(None)
    }

    async fn fetch_change_event_for_wake(
        &self,
        _owner: &Owner,
        _seq: Uuid,
    ) -> Result<Option<ChangeEventForWake>, StorageError> {
        Ok(None)
    }

    async fn acquire_wake_lock(
        &self,
        _owner: &Owner,
        _instance: &PersonalityRef,
    ) -> Result<WakeLockGuard, StorageError> {
        Ok(WakeLockGuard::noop())
    }
}

fn registry() -> proxima_core::FlavorRegistryFrozen {
    FlavorRegistry::new().freeze()
}

fn owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::nil())),
        org_id: OrgId::new(Uuid::nil()),
    }
}

fn instance_id() -> PersonalityInstanceId {
    PersonalityInstanceId::new(Uuid::from_u128(1))
}

fn entry(trigger: &str) -> WakeEntryDraft {
    WakeEntryDraft {
        wake_entry_id: Uuid::now_v7(),
        personality_instance_id: instance_id(),
        trigger_kind: WakeEntryTriggerKind::OnMemory,
        trigger_id: trigger.into(),
        label: "label".into(),
        enabled: true,
        execution_mode: WakeExecutionMode::SubstrateOnly,
        authored_by: WakeEntryAuthoredBy::Any,
        probability_promille: 1000,
        goal_scope: proxima_core::WakeEntryGoalScope::None,
        instructions: String::new(),
        model_tier: ModelTier::Standard,
        inference_target_ref: None,
        substrate_tool_palette: vec![],
        workspace_tool_palette: vec![],
        workspace_binding: None,
        max_rounds: 4,
        intervention_policy: None,
    }
}

fn req(entries: Vec<WakeEntryDraft>) -> SetWakeEntriesRequest {
    SetWakeEntriesRequest {
        owner: owner(),
        personality_instance_id: instance_id(),
        entries,
    }
}

#[tokio::test]
async fn duplicate_trigger_in_request_is_rejected() {
    let storage = FixtureStorage::default();
    let registry = registry();
    let ctx = SetWakeEntriesContext {
        storage: &storage,
        registry: &registry,
    };
    let err = set_wake_entries(&ctx, &req(vec![entry("schema-a"), entry("schema-a")]))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::DuplicateTriggerInRequest);
}

#[tokio::test]
async fn empty_label_is_rejected() {
    let storage = FixtureStorage::default();
    let registry = registry();
    let ctx = SetWakeEntriesContext {
        storage: &storage,
        registry: &registry,
    };
    let mut draft = entry("schema-a");
    draft.label = "   ".into();
    let err = set_wake_entries(&ctx, &req(vec![draft])).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn unregistered_tool_is_rejected() {
    let storage = FixtureStorage::default();
    let registry = registry();
    let ctx = SetWakeEntriesContext {
        storage: &storage,
        registry: &registry,
    };
    let mut draft = entry("schema-a");
    draft.substrate_tool_palette = vec!["proxima-test/missing".into()];
    let err = set_wake_entries(&ctx, &req(vec![draft])).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::ToolNotRegistered);
}

#[tokio::test]
async fn substrate_pack_tool_ids_are_registered_for_wake_entries() {
    let storage = FixtureStorage::default();
    let registry = registry();
    let ctx = SetWakeEntriesContext {
        storage: &storage,
        registry: &registry,
    };
    let mut draft = entry("schema-a");
    draft.substrate_tool_palette = vec![
        "core/fetch_memory".into(),
        "core/emit_abstraction".into(),
        "core/emit_intervention_decision".into(),
    ];
    let err = set_wake_entries(&ctx, &req(vec![draft])).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::TierUnbound);
}

#[tokio::test]
async fn workspace_tool_outside_catalog_is_rejected() {
    let storage = FixtureStorage::default();
    let registry = registry();
    let ctx = SetWakeEntriesContext {
        storage: &storage,
        registry: &registry,
    };
    let mut draft = entry("schema-a");
    draft.workspace_tool_palette = vec!["proxima-workspace/not-real".into()];
    let err = set_wake_entries(&ctx, &req(vec![draft])).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::ToolNotRegistered);
}

#[tokio::test]
async fn substrate_tool_id_in_workspace_palette_is_rejected() {
    // A registered MCP tool ID belongs in substrate, never workspace.
    let storage = FixtureStorage::default();
    let registry = registry();
    let ctx = SetWakeEntriesContext {
        storage: &storage,
        registry: &registry,
    };
    let mut draft = entry("schema-a");
    // Cross-tier: an MCP id placed in the workspace slot.
    draft.workspace_tool_palette = vec!["proxima-core/append-event".into()];
    let err = set_wake_entries(&ctx, &req(vec![draft])).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::ToolNotRegistered);
}

#[tokio::test]
async fn workspace_binding_allows_non_registry_workspace_trigger() {
    let storage = FixtureStorage::default();
    let registry = registry();
    let ctx = SetWakeEntriesContext {
        storage: &storage,
        registry: &registry,
    };
    let mut draft = entry("proxima-goal/goal-activated-v1");
    draft.execution_mode = WakeExecutionMode::Workspace;
    draft.workspace_tool_palette = vec!["core-workspace/text_editor".into()];
    draft.workspace_binding = Some(WakeWorkspaceBinding::GitWorktree {
        repo_path: "/tmp/repo".into(),
        base_ref: "HEAD".into(),
        finalize: WakeWorkspaceFinalize::CommitAll,
        worktrees_root: None,
    });
    let err = set_wake_entries(&ctx, &req(vec![draft])).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::TierUnbound);
}

#[tokio::test]
async fn workspace_binding_requires_workspace_mode() {
    let storage = FixtureStorage::default();
    let registry = registry();
    let ctx = SetWakeEntriesContext {
        storage: &storage,
        registry: &registry,
    };
    let mut draft = entry("proxima-goal/goal-activated-v1");
    draft.workspace_binding = Some(WakeWorkspaceBinding::GitWorktree {
        repo_path: "/tmp/repo".into(),
        base_ref: "HEAD".into(),
        finalize: WakeWorkspaceFinalize::CommitAll,
        worktrees_root: None,
    });
    let err = set_wake_entries(&ctx, &req(vec![draft])).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn unbound_tier_is_rejected_after_shape_validation() {
    let storage = FixtureStorage::default();
    let registry = registry();
    let ctx = SetWakeEntriesContext {
        storage: &storage,
        registry: &registry,
    };
    let err = set_wake_entries(&ctx, &req(vec![entry("schema-a")]))
        .await
        .unwrap_err();
    assert!(matches!(err.code, ErrorCode::TierUnbound));
}

#[tokio::test]
async fn pinned_missing_target_is_rejected() {
    let storage = FixtureStorage {
        targets: vec![],
        bindings: vec![InferenceTierBindingRow {
            owner: owner(),
            tier: ModelTier::Standard,
            target_ref: "local".into(),
        }],
        set_calls: Mutex::new(0),
    };
    let registry = registry();
    let ctx = SetWakeEntriesContext {
        storage: &storage,
        registry: &registry,
    };
    let mut draft = entry("schema-a");
    draft.inference_target_ref = Some("missing".into());
    let err = set_wake_entries(&ctx, &req(vec![draft])).await.unwrap_err();
    assert!(matches!(err.code, ErrorCode::InferenceTargetMissing));
}

#[tokio::test]
async fn valid_target_calls_storage() {
    let target_ref = "local";
    let storage = FixtureStorage {
        targets: vec![InferenceTargetRow {
            owner: owner(),
            target_ref: target_ref.into(),
            config: proxima_core::InferenceTargetConfig::MistralChat(
                proxima_core::MistralChatConfig {
                    base_url: "http://127.0.0.1:9".into(),
                    model_id: "test-model".into(),
                    api_key_env: "TEST_KEY".into(),
                    temperature: None,
                    max_completion_tokens: None,
                    reasoning_effort: None,
                },
            ),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            updated_at: time::OffsetDateTime::UNIX_EPOCH,
        }],
        bindings: Vec::new(),
        set_calls: Mutex::new(0),
    };
    let registry = registry();
    let ctx = SetWakeEntriesContext {
        storage: &storage,
        registry: &registry,
    };
    let mut draft = entry("schema-a");
    draft.inference_target_ref = Some(target_ref.into());
    let result = set_wake_entries(&ctx, &req(vec![draft])).await;
    result.unwrap();
    assert_eq!(*storage.set_calls.lock().unwrap(), 1);
}

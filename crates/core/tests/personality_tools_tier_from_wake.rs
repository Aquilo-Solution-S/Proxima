//! Phase 1d Task 10: substrate tools stamp memory provenance with the
//! resolved InferenceTarget's `model_id`, sourced from the active
//! `WakeTokenContext` rather than the static `ModelTier::Standard`
//! placeholder that lived at `personality/tools/shared.rs:82` through
//! the end of Phase 1c.
//!
//! The plan allows a unit-level test of the helper directly when the
//! full PG-backed emit path is too costly. The two contracts under test:
//!
//!   1. When `wake_invocation = Some(...)`, the helper returns the wake
//!      context's `model_id` verbatim.
//!   2. When `None`, it falls back to the engine's Standard-tier client
//!      so the memory row's `model_id` column is never null.
//!
//! Both arms exercise `model_id_from_wake_invocation` through a
//! crate-internal entry point (`__test_only_model_id_from_wake_invocation`)
//! so we don't have to spin up Postgres.

use std::sync::Arc;

use async_trait::async_trait;
use proxima_core::auth::NoAuth;
use proxima_core::llm::{AnthropicClient, LlmError, MessagesRequest, MessagesResponse};
use proxima_core::personality::{PersonalityInstanceId, PersonalityToolContext};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::wake::token_store::WakeTokenContext;
use proxima_core::{
    Engine, FlavorRegistry, HandleTable, MemoryId, ModelTier, OrgId, Owner, Principal, UserId,
    WakeChainDepth,
};

#[derive(Debug)]
struct StubAnthropic {
    label: String,
}

#[async_trait]
impl AnthropicClient for StubAnthropic {
    async fn messages_create(
        &self,
        _request: MessagesRequest,
    ) -> Result<MessagesResponse, LlmError> {
        Err(LlmError::Internal("stub".into()))
    }

    fn model_id_for(&self, _tier: ModelTier) -> &str {
        &self.label
    }
}

fn engine() -> Engine {
    let owner = Owner {
        principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
        org_id: OrgId::new(uuid::Uuid::now_v7()),
    };
    Engine::new(
        FlavorRegistry::new().freeze(),
        MemoryStore::new(),
        Box::new(NoAuth::new(owner.principal.clone(), owner.clone())),
    )
}

fn owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
        org_id: OrgId::new(uuid::Uuid::now_v7()),
    }
}

fn wake_ctx(model_id: &str) -> WakeTokenContext {
    WakeTokenContext {
        invocation_id: uuid::Uuid::new_v4(),
        personality_instance_id: uuid::Uuid::now_v7(),
        wake_entry_id: uuid::Uuid::now_v7(),
        change_event_seq: uuid::Uuid::now_v7(),
        owner: owner(),
        palette: vec!["core/emit_abstraction".into()],
        model_id: model_id.into(),
        max_rounds: 4,
        current_root_perspective_memory_id: MemoryId::new(uuid::Uuid::now_v7()),
        triggering_event_memory_id: MemoryId::new(uuid::Uuid::now_v7()),
        triggering_event_depth: WakeChainDepth::new(0),
        read_log: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        handles: Arc::new(HandleTable::new()),
    }
}

#[tokio::test]
async fn returns_wake_context_model_id_when_bound() {
    let engine = engine();
    let owner = owner();
    let palette: Vec<Arc<dyn proxima_core::personality::PersonalityTool>> = Vec::new();
    let wake = wake_ctx("anthropic/claude-3-5-sonnet-20241022");
    let ctx = PersonalityToolContext::new(
        &engine,
        &owner,
        "test/personality",
        PersonalityInstanceId::new(uuid::Uuid::now_v7()),
        MemoryId::new(uuid::Uuid::now_v7()),
        MemoryId::new(uuid::Uuid::now_v7()),
        WakeChainDepth::new(0),
        Vec::new(),
        Vec::new(),
        &palette,
    )
    .with_wake_invocation(&wake);
    let anthropic = StubAnthropic {
        label: "should/not/be/used".into(),
    };
    let resolved =
        proxima_core::personality::__test_only_model_id_from_wake_invocation(&ctx, &anthropic);
    assert_eq!(resolved, "anthropic/claude-3-5-sonnet-20241022");
}

#[tokio::test]
async fn falls_back_to_standard_tier_without_wake_context() {
    let engine = engine();
    let owner = owner();
    let palette: Vec<Arc<dyn proxima_core::personality::PersonalityTool>> = Vec::new();
    let ctx = PersonalityToolContext::new(
        &engine,
        &owner,
        "test/personality",
        PersonalityInstanceId::new(uuid::Uuid::now_v7()),
        MemoryId::new(uuid::Uuid::now_v7()),
        MemoryId::new(uuid::Uuid::now_v7()),
        WakeChainDepth::new(0),
        Vec::new(),
        Vec::new(),
        &palette,
    );
    let anthropic = StubAnthropic {
        label: "anthropic/claude-default-standard".into(),
    };
    let resolved =
        proxima_core::personality::__test_only_model_id_from_wake_invocation(&ctx, &anthropic);
    assert_eq!(resolved, "anthropic/claude-default-standard");
}

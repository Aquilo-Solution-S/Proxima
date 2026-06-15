//! Engine smoke tests for the personality substrate.

use std::sync::Arc;

#[path = "../src/test_fixtures.rs"]
mod test_fixtures;

use proxima_core::engine::{EmbeddingClientReloader, Engine};
use proxima_core::error::ErrorCode;
use proxima_core::ids::{OrgId, SourceBatchId, UserId};
use proxima_core::llm::{EMBEDDING_DIM, EmbeddingClient};
use proxima_core::owner::{Owner, Principal};
use proxima_core::verbs::event_history::EventHistoryRequest;
use proxima_core::verbs::query::QueryRequest;
use proxima_core::verbs::schema::{FlavorRegistryFrozen, SchemaRequest};
use proxima_core::{AuthPath, AuthzContext, McpCallLogInput, RoleSet};
use test_fixtures::ConstantEmbedding;
use uuid::Uuid;

fn fresh_owner() -> (Principal, Owner) {
    let user = UserId::new(Uuid::now_v7());
    let principal = Principal::User(user);
    let owner = Owner {
        principal: principal.clone(),
        org_id: OrgId::new(Uuid::now_v7()),
    };
    (principal, owner)
}

fn boot_engine(principal: Principal, owner: Owner) -> Engine {
    let _ = (principal, owner);
    Engine::new(FlavorRegistryFrozen::new())
}

#[derive(Debug)]
struct FixedEmbeddingReloader;

impl EmbeddingClientReloader for FixedEmbeddingReloader {
    fn reload<'a>(
        &'a self,
        _owner: &'a Owner,
    ) -> futures::future::BoxFuture<'a, Result<Option<Arc<dyn EmbeddingClient>>, String>> {
        Box::pin(async {
            Ok(Some(
                Arc::new(ConstantEmbedding::zero("test-embedding")) as Arc<dyn EmbeddingClient>
            ))
        })
    }
}

#[test]
fn schema_verb_returns_empty_registry() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    let resp = engine.schema(&SchemaRequest);
    assert!(
        resp.schemas.is_empty(),
        "empty registry must list no schemas"
    );
}

#[tokio::test]
async fn reload_embedding_client_replaces_engine_slot() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner.clone())
        .with_embedding_reloader(Arc::new(FixedEmbeddingReloader));

    assert!(engine.embed_client().is_none());
    let outcome = engine
        .reload_embedding_client(&owner)
        .await
        .expect("reload hook must install client");

    assert!(outcome.active);
    assert_eq!(outcome.model_id.as_deref(), Some("test-embedding"));
    assert_eq!(outcome.dim, Some(EMBEDDING_DIM));
    assert_eq!(
        engine.embed_client().expect("client installed").model_id(),
        "test-embedding"
    );
}

#[tokio::test]
async fn drain_embedding_jobs_without_client_is_noop() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);

    let outcome = engine
        .drain_embedding_jobs(10)
        .await
        .expect("missing embedding client is a no-op");

    assert_eq!(outcome, proxima_core::EmbeddingDrainOutcome::default());
}

#[tokio::test]
async fn query_verb_returns_empty_for_configured_owner() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner.clone());
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);

    let resp = engine
        .query(
            &authz,
            &QueryRequest::for_principal(owner.principal.clone()),
        )
        .await
        .expect("single-owner query must succeed");

    assert!(resp.memories.is_empty());
    assert!(
        resp.seq_high_water.is_none(),
        "no events have been written; high-water mark must be None"
    );
}

#[tokio::test]
async fn query_verb_allows_same_principal_with_different_org() {
    let (principal, configured) = fresh_owner();
    let engine = boot_engine(principal, configured.clone());
    let authz = AuthzContext::single_owner(&configured, AuthPath::System);
    let same_principal_different_org = Owner {
        principal: configured.principal,
        org_id: OrgId::new(Uuid::now_v7()),
    };

    let resp = engine
        .query(
            &authz,
            &QueryRequest::for_principal(same_principal_different_org.principal.clone()),
        )
        .await
        .expect("access is scoped by principal, not org_id");

    assert!(resp.memories.is_empty());
}

#[tokio::test]
async fn query_verb_rejects_foreign_owner_with_forbidden() {
    let (principal, configured) = fresh_owner();
    let engine = boot_engine(principal, configured.clone());
    let authz = AuthzContext::single_owner(&configured, AuthPath::System);
    let (_, foreign) = fresh_owner();

    let err = engine
        .query(
            &authz,
            &QueryRequest::for_principal(foreign.principal.clone()),
        )
        .await
        .expect_err("foreign owner must be rejected");
    assert_eq!(err.code, ErrorCode::Forbidden);
}

#[tokio::test]
async fn tombstone_personality_rejects_noop_storage_write() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner.clone());
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let err = engine
        .tombstone_personality(
            &authz,
            proxima_core::TombstonePersonalityRequest {
                principal: owner.principal.clone(),
                org_id: None,
                personality_instance_id: proxima_core::PersonalityInstanceId::new(Uuid::now_v7()),
            },
        )
        .await
        .expect_err("NoopStorage rejects writes");
    assert_eq!(err.code, ErrorCode::Internal);
}

#[tokio::test]
async fn wake_shaped_context_denied_ingest_and_admin_but_not_goal_write() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner.clone());
    let mut authz = AuthzContext::single_owner(&owner, AuthPath::Wake);
    authz.capabilities.roles = RoleSet {
        graph_read: true,
        graph_write: true,
        source_ingest: false,
        admin: false,
    };

    let ingest_err = engine
        .close_batch(
            &authz,
            owner.principal.clone(),
            SourceBatchId::new(Uuid::now_v7()),
        )
        .await
        .expect_err("wake context must not close batches");
    assert!(
        ingest_err
            .to_string()
            .contains("requires source_ingest role")
    );

    let admin_err = engine
        .list_personality_instances(&authz, &owner.principal, false)
        .await
        .expect_err("wake context must not touch config verbs");
    assert!(admin_err.to_string().contains("requires admin role"));
}

#[tokio::test]
async fn cross_owner_context_is_forbidden_on_graph_read() {
    let (principal, owner_a) = fresh_owner();
    let engine = boot_engine(principal, owner_a.clone());
    let (_, owner_b) = fresh_owner();
    let authz = AuthzContext::single_owner(&owner_a, AuthPath::System);
    let err = engine
        .event_history(
            &authz,
            &EventHistoryRequest {
                principal: owner_b.principal,
                limit: 1,
                before: None,
            },
        )
        .await
        .expect_err("cross-owner access must be forbidden");
    assert!(
        err.to_string()
            .contains("principal cannot access requested principal")
    );
}

fn sample_mcp_input(owner: &Owner) -> McpCallLogInput {
    McpCallLogInput {
        owner: owner.clone(),
        actor_oid: "oid-1".into(),
        actor_upn: "agent@example.com".into(),
        tool_name: "core/search_memories".into(),
        ok: true,
        error: None,
        latency_ms: 42,
        io_body: b"{}".to_vec(),
        io_byte_len_original: 2,
        io_truncated: false,
        observed_at: time::OffsetDateTime::UNIX_EPOCH,
        occurred_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}

#[tokio::test]
async fn persist_mcp_call_rejects_owner_the_context_cannot_access() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner.clone());
    // A context scoped to a different owner must not write the log,
    // even though the caller supplied `owner` in the input.
    let (_, stranger_owner) = fresh_owner();
    let stranger = AuthzContext::single_owner(&stranger_owner, AuthPath::System);

    let err = engine
        .persist_mcp_call(&stranger, sample_mcp_input(&owner))
        .await
        .expect_err("foreign owner must be rejected");
    assert_eq!(err.code, ErrorCode::Forbidden);
}

#[tokio::test]
async fn persist_mcp_call_rejects_context_without_source_ingest_role() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner.clone());
    // Right owner, but no source-ingest role.
    let mut authz = AuthzContext::single_owner(&owner, AuthPath::System);
    authz.capabilities.roles = RoleSet::none();

    let err = engine
        .persist_mcp_call(&authz, sample_mcp_input(&owner))
        .await
        .expect_err("missing source-ingest role must be rejected");
    assert_eq!(err.code, ErrorCode::Forbidden);
}

#[tokio::test]
async fn persist_mcp_call_authorized_context_clears_the_gate() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner.clone());
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    // A context that clears the authz gate reaches storage; NoopStorage
    // then rejects the write with Internal — distinguishing "gate
    // opened" from "gate blocked" (Forbidden).
    let err = engine
        .persist_mcp_call(&authz, sample_mcp_input(&owner))
        .await
        .expect_err("NoopStorage rejects writes");
    assert_eq!(err.code, ErrorCode::Internal);
}

#[tokio::test]
async fn fact_retention_rejects_owner_the_context_cannot_access() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner.clone());
    let (_, stranger_owner) = fresh_owner();
    let stranger = AuthzContext::single_owner(&stranger_owner, AuthPath::System);

    let err = engine
        .set_fact_retention(&stranger, &owner, 86_400)
        .await
        .expect_err("foreign owner must be rejected");
    assert_eq!(err.code, ErrorCode::Forbidden);

    let err = engine
        .get_fact_retention(&stranger, &owner)
        .await
        .expect_err("foreign owner must be rejected");
    assert_eq!(err.code, ErrorCode::Forbidden);

    let err = engine
        .clear_fact_retention(&stranger, &owner)
        .await
        .expect_err("foreign owner must be rejected");
    assert_eq!(err.code, ErrorCode::Forbidden);

    let err = engine
        .cleanup_due_facts(&stranger, &owner)
        .await
        .expect_err("foreign owner must be rejected");
    assert_eq!(err.code, ErrorCode::Forbidden);
}

#[tokio::test]
async fn fact_retention_rejects_context_without_admin_role() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner.clone());
    let mut authz = AuthzContext::single_owner(&owner, AuthPath::System);
    authz.capabilities.roles = RoleSet::none();

    let err = engine
        .set_fact_retention(&authz, &owner, 86_400)
        .await
        .expect_err("missing admin role must be rejected");
    assert_eq!(err.code, ErrorCode::Forbidden);

    let err = engine
        .get_fact_retention(&authz, &owner)
        .await
        .expect_err("missing admin role must be rejected");
    assert_eq!(err.code, ErrorCode::Forbidden);

    let err = engine
        .clear_fact_retention(&authz, &owner)
        .await
        .expect_err("missing admin role must be rejected");
    assert_eq!(err.code, ErrorCode::Forbidden);

    let err = engine
        .cleanup_due_facts(&authz, &owner)
        .await
        .expect_err("missing admin role must be rejected");
    assert_eq!(err.code, ErrorCode::Forbidden);
}

#[tokio::test]
async fn fact_retention_authorized_context_clears_the_gate() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner.clone());
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);

    let err = engine
        .set_fact_retention(&authz, &owner, 86_400)
        .await
        .expect_err("NoopStorage rejects writes");
    assert_eq!(err.code, ErrorCode::Internal);

    let err = engine
        .get_fact_retention(&authz, &owner)
        .await
        .expect_err("NoopStorage rejects writes");
    assert_eq!(err.code, ErrorCode::Internal);

    let err = engine
        .clear_fact_retention(&authz, &owner)
        .await
        .expect_err("NoopStorage rejects writes");
    assert_eq!(err.code, ErrorCode::Internal);

    let err = engine
        .cleanup_due_facts(&authz, &owner)
        .await
        .expect_err("NoopStorage rejects writes");
    assert_eq!(err.code, ErrorCode::Internal);
}

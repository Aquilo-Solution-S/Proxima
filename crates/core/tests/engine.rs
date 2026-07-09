//! Engine smoke tests for the core substrate.

use std::sync::Arc;

#[path = "../src/test_fixtures.rs"]
mod test_fixtures;

use proxima_core::engine::{EmbeddingClientReloader, Engine};
use proxima_core::error::ErrorCode;
use proxima_core::ids::{SourceBatchId, UserId};
use proxima_core::llm::{EMBEDDING_DIM, EmbeddingClient};
use proxima_core::owner::{Owner, OwnerRef};
use proxima_core::verbs::change_history::ChangeHistoryRequest;
use proxima_core::verbs::mcp_call_history::McpCallHistoryRequest;
use proxima_core::verbs::query::QueryRequest;
use proxima_core::verbs::schema::{FlavorRegistryFrozen, SchemaRequest};
use proxima_core::{AuthPath, AuthzContext, McpCallLogInput};
use test_fixtures::ConstantEmbedding;
use uuid::Uuid;

type ResolvedAuthz = AuthzContext;

fn fresh_owner() -> (OwnerRef, Owner) {
    let user = UserId::new(Uuid::now_v7());
    let principal = OwnerRef::Personal(user);
    let owner = principal;
    (principal, owner)
}

fn boot_engine(principal: OwnerRef, owner: Owner) -> Engine {
    let _ = (principal, owner);
    Engine::new(FlavorRegistryFrozen::new())
}

fn fresh_caller() -> OwnerRef {
    OwnerRef::Personal(UserId::new(Uuid::now_v7()))
}

fn granted_no_access_authz(auth_path: AuthPath) -> ResolvedAuthz {
    let OwnerRef::Personal(user) = fresh_caller() else {
        unreachable!("fresh caller is personal");
    };
    AuthzContext::for_subject(user, auth_path)
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
    let engine =
        boot_engine(principal, owner).with_embedding_reloader(Arc::new(FixedEmbeddingReloader));

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
    let engine = boot_engine(principal, owner);
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

    let resp = engine
        .query(&authz, &QueryRequest::for_owner(owner))
        .await
        .expect("single-owner query must succeed");

    assert!(resp.memories.is_empty());
    assert!(
        resp.seq_high_water.is_none(),
        "no events have been written; high-water mark must be None"
    );
}

#[tokio::test]
async fn query_scopes_reads_to_authz_context_not_client_principal() {
    let (principal, configured) = fresh_owner();
    let engine = boot_engine(principal, configured);
    let authz = AuthzContext::single_owner(&configured, AuthPath::HostBearer);
    let (_, foreign) = fresh_owner();

    // Group-ownership read model: reads are scoped to the authenticated
    // context's access set (S_read), never to a client-supplied
    // `QueryRequest::principal`. Unlike write/admin verbs — which reject a
    // foreign owner — a foreign principal in a read request is not an access
    // vector: it can never widen what the caller sees, so the verb returns the
    // caller's accessible subset (empty here under RejectingStorage) rather than
    // Forbidden. Cross-principal no-leak against real data is proven in the PG
    // integration suite (owner_columns_pg).
    let resp = engine
        .query(&authz, &QueryRequest::for_owner(foreign))
        .await
        .expect("a foreign client principal is scoped away, not rejected");
    assert!(resp.memories.is_empty() && resp.goals.is_empty() && resp.edges.is_empty());
}

#[tokio::test]
async fn wake_shaped_context_denied_ingest_and_admin_but_not_goal_write() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    let authz = granted_no_access_authz(AuthPath::Wake);

    let ingest_err = engine
        .close_batch(&authz, owner, SourceBatchId::new(Uuid::now_v7()))
        .await
        .expect_err("wake context must not close batches");
    assert!(
        ingest_err
            .to_string()
            .contains("requires ingest on this owner")
    );
}

#[tokio::test]
async fn change_history_ignores_client_principal_as_access_vector() {
    let (principal, owner_a) = fresh_owner();
    let engine = boot_engine(principal, owner_a);
    let (_, owner_b) = fresh_owner();
    let authz = AuthzContext::single_owner(&owner_a, AuthPath::HostBearer);
    let response = engine
        .change_history(
            &authz,
            &ChangeHistoryRequest {
                owner: owner_b,
                limit: 1,
                before: None,
            },
        )
        .await
        .expect("client principal must not participate in read authorization");
    assert!(response.events.is_empty());
    assert!(response.seq_high_water.is_none());
}

fn sample_mcp_input(owner: &Owner) -> McpCallLogInput {
    McpCallLogInput {
        owner: *owner,
        actor_oid: "oid-1".into(),
        actor_upn: "agent@example.com".into(),
        tool_name: "core_search_memories".into(),
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
async fn persist_mcp_call_rejects_context_without_ingest_grant() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    let authz = granted_no_access_authz(AuthPath::HostBearer);

    let err = engine
        .persist_mcp_call(&authz, sample_mcp_input(&owner))
        .await
        .expect_err("ingest grant required");
    assert!(err.to_string().contains("requires ingest on this owner"));
}

#[tokio::test]
async fn read_mcp_call_history_rejects_context_without_read_grant() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    let authz = granted_no_access_authz(AuthPath::HostBearer);

    let err = engine
        .read_mcp_call_history(
            &authz,
            &McpCallHistoryRequest {
                owner,
                actor_oid: None,
                limit: 10,
                include_body: false,
                before: None,
            },
        )
        .await
        .expect_err("read grant required");
    assert!(err.to_string().contains("requires viewer on this owner"));
}

#[tokio::test]
async fn persist_mcp_call_rejects_owner_the_context_cannot_access() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    // A context scoped to a different owner must not write the log,
    // even though the caller supplied `owner` in the input.
    let (_, stranger_owner) = fresh_owner();
    let stranger = AuthzContext::single_owner(&stranger_owner, AuthPath::HostBearer);

    let err = engine
        .persist_mcp_call(&stranger, sample_mcp_input(&owner))
        .await
        .expect_err("foreign owner must be rejected");
    assert_eq!(err.code, ErrorCode::Forbidden);
}

#[tokio::test]
async fn persist_mcp_call_rejects_context_without_graph_write_role() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    let authz = granted_no_access_authz(AuthPath::HostBearer);

    let err = engine
        .persist_mcp_call(&authz, sample_mcp_input(&owner))
        .await
        .expect_err("missing graph-write role must be rejected");
    assert_eq!(err.code, ErrorCode::Forbidden);
}

#[tokio::test]
async fn persist_mcp_call_authorized_context_clears_the_gate() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    // A context that clears the authz gate reaches storage; RejectingStorage
    // then rejects the write with Internal — distinguishing "gate
    // opened" from "gate blocked" (Forbidden).
    let err = engine
        .persist_mcp_call(&authz, sample_mcp_input(&owner))
        .await
        .expect_err("RejectingStorage rejects writes");
    assert_eq!(err.code, ErrorCode::Internal);
}

#[tokio::test]
async fn read_mcp_call_history_rejects_context_without_graph_read_role() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    let authz = granted_no_access_authz(AuthPath::HostBearer);

    let err = engine
        .read_mcp_call_history(
            &authz,
            &McpCallHistoryRequest {
                owner,
                actor_oid: None,
                limit: 1,
                include_body: false,
                before: None,
            },
        )
        .await
        .expect_err("missing graph-read role must be rejected");
    assert_eq!(err.code, ErrorCode::Forbidden);
}

#[tokio::test]
async fn read_mcp_call_history_rejects_zero_limit_as_invalid_argument() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

    let err = engine
        .read_mcp_call_history(
            &authz,
            &McpCallHistoryRequest {
                owner,
                actor_oid: None,
                limit: 0,
                include_body: false,
                before: None,
            },
        )
        .await
        .expect_err("zero limit must be rejected before storage");
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(err.message.contains("limit"));
    assert!(err.message.contains("must be > 0"));
}

#[tokio::test]
async fn fact_retention_rejects_owner_the_context_cannot_access() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    let (_, stranger_owner) = fresh_owner();
    let stranger = AuthzContext::single_owner(&stranger_owner, AuthPath::HostBearer);

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
}

#[tokio::test]
async fn fact_retention_rejects_context_without_admin_role() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    let authz = granted_no_access_authz(AuthPath::HostBearer);

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
}

#[tokio::test]
async fn fact_retention_authorized_context_clears_the_gate() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

    let err = engine
        .set_fact_retention(&authz, &owner, 86_400)
        .await
        .expect_err("RejectingStorage rejects writes");
    assert_eq!(err.code, ErrorCode::Internal);

    let err = engine
        .get_fact_retention(&authz, &owner)
        .await
        .expect_err("RejectingStorage rejects writes");
    assert_eq!(err.code, ErrorCode::Internal);

    let err = engine
        .clear_fact_retention(&authz, &owner)
        .await
        .expect_err("RejectingStorage rejects writes");
    assert_eq!(err.code, ErrorCode::Internal);
}

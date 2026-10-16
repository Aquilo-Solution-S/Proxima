//! Engine smoke tests for the core substrate.

use std::sync::Arc;

#[path = "../src/test_fixtures.rs"]
mod test_fixtures;

use proxima_core::engine::Engine;
use proxima_core::llm::SingleClientRouter;
use proxima_core::owner::OwnerId;
use proxima_core::verbs::query::QueryRequest;
use proxima_core::verbs::schema::SchemaRequest;
use proxima_core::{AuthPath, AuthzContext, FlavorRegistry};
use test_fixtures::ConstantEmbedding;
use uuid::Uuid;

fn fresh_owner() -> (OwnerId, OwnerId) {
    let user = proxima_core::ids::UserId::new(Uuid::now_v7());
    let principal = OwnerId::from(user);
    (principal, principal)
}

fn boot_engine(principal: OwnerId, owner: OwnerId) -> Engine {
    let _ = (principal, owner);
    Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
}

fn fixed_router() -> Arc<SingleClientRouter> {
    Arc::new(
        SingleClientRouter::bind(Arc::new(ConstantEmbedding::zero("test-embedding")))
            .expect("supported width"),
    )
}

#[test]
fn schema_verb_projects_the_validated_engine_registry() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    let expected = engine
        .registry()
        .schemas()
        .iter()
        .map(|schema| (schema.schema_id.clone(), schema.schema_version, schema.kind))
        .collect::<Vec<_>>();
    let actual = engine
        .schema(&SchemaRequest)
        .schemas
        .into_iter()
        .map(|schema| (schema.schema_id, schema.schema_version, schema.kind))
        .collect::<Vec<_>>();
    assert!(!expected.is_empty());
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn purge_without_a_router_is_refused() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    let err = engine
        .purge_embedding_spaces(&owner)
        .await
        .expect_err("no router is not an empty route");
    assert!(err.to_string().contains("no embedding router"), "{err}");
}

#[tokio::test]
async fn reconcile_embeddings_with_client_reaches_maintenance_port() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner).with_embedding_router(fixed_router());
    let err = engine
        .reconcile_embeddings(proxima_core::EmbeddingReconcileScope::MissingOnly, Some(10))
        .await
        .expect_err("rejecting storage must reject the forwarded reconcile");
    assert!(err.to_string().contains("embedding maintenance"), "{err}");
}

#[tokio::test]
async fn query_verb_returns_empty_for_configured_owner() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let resp = engine
        .query(&authz, &QueryRequest::readable())
        .await
        .expect("single-owner query must succeed");
    assert!(resp.memories.is_empty());
}

#[tokio::test]
async fn query_scopes_reads_to_authz_context_not_client_principal() {
    let (principal, configured) = fresh_owner();
    let engine = boot_engine(principal, configured);
    let authz = AuthzContext::single_owner(&configured, AuthPath::HostBearer);
    let (_, foreign) = fresh_owner();
    let mut legacy = serde_json::to_value(QueryRequest::readable()).unwrap();
    legacy["owner"] = serde_json::to_value(foreign).unwrap();
    legacy["read_owners"] = serde_json::to_value([foreign]).unwrap();
    let request: QueryRequest = serde_json::from_value(legacy).unwrap();
    let resp = engine
        .query(&authz, &request)
        .await
        .expect("legacy owner fields cannot affect readable scope");
    assert!(resp.memories.is_empty() && resp.goals.is_empty() && resp.edges.is_empty());
}

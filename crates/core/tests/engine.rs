//! Engine smoke tests for the personality substrate.

use proxima_core::auth::NoAuth;
use proxima_core::engine::Engine;
use proxima_core::error::ErrorCode;
use proxima_core::ids::{OrgId, UserId};
use proxima_core::owner::{Owner, Principal};
use proxima_core::verbs::query::{MemoryStore, QueryRequest};
use proxima_core::verbs::schema::{FlavorRegistryFrozen, SchemaRequest};
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
    let resolver = NoAuth::new(principal, owner);
    Engine::new(
        FlavorRegistryFrozen::new(),
        MemoryStore::new(),
        Box::new(resolver),
    )
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
async fn query_verb_returns_empty_for_configured_owner() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner.clone());

    let resp = engine
        .query(
            &proxima_core::Credentials::None,
            &QueryRequest::for_owner(owner),
        )
        .await
        .expect("NoAuth single-owner query must succeed");

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
    let same_principal_different_org = Owner {
        principal: configured.principal,
        org_id: OrgId::new(Uuid::now_v7()),
    };

    let resp = engine
        .query(
            &proxima_core::Credentials::None,
            &QueryRequest::for_owner(same_principal_different_org),
        )
        .await
        .expect("access is scoped by principal, not org_id");

    assert!(resp.memories.is_empty());
}

#[tokio::test]
async fn query_verb_rejects_foreign_owner_with_forbidden() {
    let (principal, configured) = fresh_owner();
    let engine = boot_engine(principal, configured);
    let (_, foreign) = fresh_owner();

    let err = engine
        .query(
            &proxima_core::Credentials::None,
            &QueryRequest::for_owner(foreign),
        )
        .await
        .expect_err("foreign owner must be rejected");
    assert_eq!(err.code, ErrorCode::Forbidden);
}

#[tokio::test]
async fn dispatcher_tick_is_noop_without_wake_configs() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    assert_eq!(engine.run_dispatcher_tick().await.unwrap(), 0);
}

#[tokio::test]
async fn tombstone_personality_rejects_noop_storage_write() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner.clone());
    let err = engine
        .tombstone_personality(proxima_core::TombstonePersonalityRequest {
            owner,
            personality_type_id: "missing/type".into(),
            personality_instance_id: proxima_core::PersonalityInstanceId::new(Uuid::now_v7()),
        })
        .await
        .expect_err("NoopStorage rejects writes");
    assert_eq!(err.code, ErrorCode::Internal);
}

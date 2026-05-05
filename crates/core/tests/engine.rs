//! M1 done-when proof — boot the Engine, Schema returns empty,
//! Query over the configured Owner returns empty, Query for a
//! foreign Owner returns Forbidden.

use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::engine::Engine;
use proxima_core::error::ErrorCode;
use proxima_core::ids::{OrgId, UserId};
use proxima_core::owner::{Owner, Principal};
use proxima_core::verbs::query::{MemoryStore, QueryRequest};
use proxima_core::verbs::schema::{SchemaRegistry, SchemaRequest};
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
        SchemaRegistry::new(),
        MemoryStore::new(),
        Box::new(resolver),
    )
}

#[test]
fn schema_verb_returns_empty_registry() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    let resp = engine.schema(&SchemaRequest);
    assert!(resp.schemas.is_empty(), "M1 registry must be empty");
}

#[tokio::test]
async fn query_verb_returns_empty_for_configured_owner() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner.clone());

    let resp = engine
        .query(&Credentials::None, &QueryRequest::for_owner(owner))
        .await
        .expect("NoAuth single-Owner query must succeed");

    assert!(resp.memories.is_empty(), "M1 store must be empty");
    assert!(
        resp.seq_high_water.is_none(),
        "no events have been written; seq_high_water must be None"
    );
}

#[tokio::test]
async fn query_verb_rejects_foreign_owner_with_forbidden() {
    let (principal, configured) = fresh_owner();
    let engine = boot_engine(principal, configured);

    // A different Owner — same shape, fresh ids, NOT in
    // NoAuth's accessible set.
    let (_, foreign) = fresh_owner();

    let err = engine
        .query(&Credentials::None, &QueryRequest::for_owner(foreign))
        .await
        .expect_err("foreign Owner must be rejected");
    assert_eq!(err.code, ErrorCode::Forbidden);
}

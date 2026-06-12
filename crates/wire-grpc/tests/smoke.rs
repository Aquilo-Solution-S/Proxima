//! Smoke test for wire-grpc — exercises the gRPC service trait
//! against an in-memory engine. No transport binding; the trait
//! methods are invoked directly.

use std::sync::Arc;

use proxima_core::engine::Engine;
use proxima_core::owner::Principal;
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::schema::FlavorRegistryFrozen;
use proxima_core::{AuthPath, AuthzContext};
use proxima_core::{OrgId, Owner, UserId};
use tonic::Request;
use uuid::Uuid;

use proxima_wire_grpc::pb::engine_server::Engine as EngineTrait;
use proxima_wire_grpc::{
    EngineGrpcServer,
    pb::{self, QueryRequest, ReadFilter, ReadPagination, SchemaRequest},
};

fn fresh_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

fn build_engine() -> Engine {
    Engine::new(FlavorRegistryFrozen::new(), MemoryStore::new())
}

fn pb_owner(owner: &Owner) -> pb::Owner {
    let principal = match &owner.principal {
        Principal::User(u) => pb::Principal {
            kind: Some(pb::principal::Kind::UserId(u.into_inner().to_string())),
        },
        Principal::Group(g) => pb::Principal {
            kind: Some(pb::principal::Kind::GroupId(g.into_inner().to_string())),
        },
    };
    pb::Owner {
        principal: Some(principal),
        org_id: owner.org_id.into_inner().to_string(),
    }
}

#[tokio::test]
async fn schema_returns_empty_registry() {
    let owner = fresh_owner();
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let server = EngineGrpcServer::new(Arc::new(build_engine()), authz);

    let resp = EngineTrait::schema(&server, Request::new(SchemaRequest {}))
        .await
        .expect("schema rpc should succeed on empty registry");
    let body = resp.into_inner();
    assert!(body.schemas.is_empty());
    assert!(body.relations.is_empty());
}

#[tokio::test]
async fn query_returns_empty_memories_for_fresh_owner() {
    let owner = fresh_owner();
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let server = EngineGrpcServer::new(Arc::new(build_engine()), authz);

    let req = Request::new(QueryRequest {
        owner: Some(pb_owner(&owner)),
        filter: Some(ReadFilter::default()),
        pagination: Some(ReadPagination { limit: 0 }),
    });

    let resp = EngineTrait::query(&server, req)
        .await
        .expect("query rpc should succeed");
    let body = resp.into_inner();
    assert!(body.memories.is_empty());
    assert!(body.goals.is_empty());
}

#[tokio::test]
async fn missing_owner_yields_invalid_argument_not_internal() {
    let owner = fresh_owner();
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let server = EngineGrpcServer::new(Arc::new(build_engine()), authz);

    let req = Request::new(QueryRequest {
        owner: None,
        filter: Some(ReadFilter::default()),
        pagination: Some(ReadPagination::default()),
    });

    let err = EngineTrait::query(&server, req)
        .await
        .expect_err("missing owner should fail");
    assert_eq!(
        err.code(),
        tonic::Code::InvalidArgument,
        "expected InvalidArgument; got {:?}",
        err.code()
    );
    assert_ne!(
        err.code(),
        tonic::Code::Internal,
        "missing owner must not surface as Internal (would indicate panic-leak)",
    );
}

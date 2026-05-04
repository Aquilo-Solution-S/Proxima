//! `proxima-engine` binary — single-Owner, NoAuth, in-memory
//! demo of the M1 Schema and Query verb surfaces.

use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::engine::Engine;
use proxima_core::ids::{OrgId, UserId};
use proxima_core::owner::{Owner, Principal};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::query::QueryRequest;
use proxima_core::verbs::schema::{SchemaRegistry, SchemaRequest};
use uuid::Uuid;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let user = UserId::new(Uuid::now_v7());
    let org = OrgId::new(Uuid::now_v7());
    let owner = Owner {
        principal: Principal::User(user),
        org_id: org,
    };

    let resolver = NoAuth::new(Principal::User(user), owner.clone());
    let engine = Engine::new(
        SchemaRegistry::new(),
        MemoryStore::new(),
        Box::new(resolver),
    );

    let schema_resp = engine.schema(&SchemaRequest);
    let query_resp = engine
        .query(&Credentials::None, &QueryRequest::for_owner(owner))
        .await
        .expect("NoAuth single-Owner query must succeed");

    println!(
        "proxima-engine ready — schemas: {}, memories: {}",
        schema_resp.schemas.len(),
        query_resp.memories.len(),
    );
}

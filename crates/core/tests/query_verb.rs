use proxima_core::verbs::query::{
    MemoryStore, QueryRequest, SupersessionStatus, TombstoneFilter,
};
use proxima_core::{OrgId, Owner, Principal, UserId};
use uuid::Uuid;

#[test]
fn empty_store_returns_empty_response() {
    let owner = Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    };
    let store = MemoryStore::new();
    let resp = store.query(&QueryRequest::for_owner(owner));
    assert!(resp.memories.is_empty());
    assert!(resp.seq_high_water.is_none());
}

#[test]
fn query_request_defaults_to_present_only() {
    let owner = Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    };
    let req = QueryRequest::for_owner(owner);
    assert_eq!(req.supersession, SupersessionStatus::HeadsOnly);
    assert_eq!(req.tombstones, TombstoneFilter::PresentOnly);
}

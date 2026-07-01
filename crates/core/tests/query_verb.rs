use proxima_core::verbs::query::{
    QueryCursor, QueryPage, QueryRequest, SupersessionStatus, TombstoneFilter,
};
use proxima_core::{MemoryId, Owner, UserId};
use uuid::Uuid;

#[test]
fn query_request_defaults_to_present_only() {
    let owner = Owner::Personal(UserId::new(Uuid::now_v7()));
    let req = QueryRequest::for_owner(owner);
    assert_eq!(req.supersession, SupersessionStatus::HeadsOnly);
    assert_eq!(req.tombstones, TombstoneFilter::PresentOnly);
    assert_eq!(req.page, QueryPage::default());
    assert!(req.stateful_heads.is_empty());
}

#[test]
fn query_request_deserializes_missing_page_as_default() {
    let owner = Owner::Personal(UserId::new(Uuid::now_v7()));
    let req = QueryRequest::for_owner(owner);
    let mut value = serde_json::to_value(&req).expect("QueryRequest serializes");
    value
        .as_object_mut()
        .expect("QueryRequest serializes as object")
        .remove("page");

    let decoded: QueryRequest =
        serde_json::from_value(value).expect("QueryRequest accepts missing page");
    assert_eq!(decoded.page, QueryPage::default());
}

#[test]
fn query_cursor_round_trips() {
    let cursor = QueryCursor::Memory {
        created_at: time::OffsetDateTime::now_utc(),
        memory_id: MemoryId::new(Uuid::now_v7()),
    };
    let encoded = serde_json::to_value(&cursor).expect("QueryCursor serializes");
    let decoded: QueryCursor = serde_json::from_value(encoded).expect("QueryCursor deserializes");
    assert_eq!(decoded, cursor);
}

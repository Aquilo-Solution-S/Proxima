use proxima_core::verbs::query::{QueryPage, QueryRequest, SupersessionStatus};

#[test]
fn query_request_defaults_to_heads_only_without_dead_tombstone_axis() {
    let req = QueryRequest::readable();
    assert_eq!(req.supersession, SupersessionStatus::HeadsOnly);
    assert!(
        !serde_json::to_value(&req)
            .expect("QueryRequest serializes")
            .as_object()
            .expect("QueryRequest serializes as an object")
            .contains_key("tombstones")
    );
    assert_eq!(req.page, QueryPage::default());
    assert_eq!(req.assignment, None);
    assert_eq!(req.evidence_contains, None);
}

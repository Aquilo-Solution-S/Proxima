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
    assert!(req.goal_states.is_empty());
    assert!(req.goal_handles.is_empty());
    assert!(req.referenced_goal_handles.is_empty());
}

#[test]
fn goal_selectors_decode_as_typed_sets_and_series_handles() {
    use proxima_core::verbs::goal_write::GoalState;

    let handle = uuid::Uuid::now_v7();
    let mut value = serde_json::to_value(QueryRequest::readable()).unwrap();
    value["goal_states"] = serde_json::json!(["Paused", "Active", "Paused"]);
    value["goal_handles"] = serde_json::json!([handle]);
    value["referenced_goal_handles"] = serde_json::json!([handle]);
    let req: QueryRequest = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        req.goal_states,
        [GoalState::Active, GoalState::Paused].into()
    );
    assert_eq!(req.goal_handles, [handle]);
    assert_eq!(req.referenced_goal_handles, [handle]);

    value["goal_states"] = serde_json::json!("Active");
    assert!(serde_json::from_value::<QueryRequest>(value.clone()).is_err());
    value["goal_states"] = serde_json::json!(["Superseded"]);
    assert!(serde_json::from_value::<QueryRequest>(value.clone()).is_err());
    value["goal_states"] = serde_json::json!([]);
    value["goal_handles"] = serde_json::json!(["not-a-uuid"]);
    assert!(serde_json::from_value::<QueryRequest>(value.clone()).is_err());
    value["goal_handles"] = serde_json::json!([]);
    value["referenced_goal_handles"] = serde_json::json!(["not-a-uuid"]);
    assert!(serde_json::from_value::<QueryRequest>(value).is_err());
}

#[test]
fn omitted_goal_selectors_remain_unfiltered() {
    let mut value = serde_json::to_value(QueryRequest::readable()).unwrap();
    let fields = value.as_object_mut().unwrap();
    fields.remove("goal_states");
    fields.remove("goal_handles");
    fields.remove("referenced_goal_handles");
    assert_eq!(
        serde_json::from_value::<QueryRequest>(value).unwrap(),
        QueryRequest::readable()
    );
}

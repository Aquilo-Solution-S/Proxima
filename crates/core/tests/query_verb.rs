use proxima_core::verbs::query::{
    PersonalityRootFilter, QueryRequest, SupersessionStatus, TombstoneFilter,
};
use proxima_core::{Owner, UserId};
use uuid::Uuid;

#[test]
fn query_request_defaults_to_present_only() {
    let owner = Owner::Personal(UserId::new(Uuid::now_v7()));
    let req = QueryRequest::for_principal(owner);
    assert_eq!(req.supersession, SupersessionStatus::HeadsOnly);
    assert_eq!(req.tombstones, TombstoneFilter::PresentOnly);
    assert_eq!(
        req.personality_roots,
        PersonalityRootFilter::IncludeInactive
    );
}

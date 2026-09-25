//! Smoke tests for core type serialization.

use proxima_core::{GroupId, Owner, UserId};
use uuid::Uuid;

#[test]
fn test_owner_principal_roundtrip() {
    let user_id = UserId::new(Uuid::now_v7());

    let owner = Owner::Personal(user_id);

    let json = serde_json::to_string(&owner).unwrap();
    let decoded: Owner = serde_json::from_str(&json).unwrap();

    assert_eq!(owner, decoded);

    let group_id = GroupId::new(Uuid::now_v7());
    let owner_group = Owner::Group(group_id);

    let json_group = serde_json::to_string(&owner_group).unwrap();
    let decoded_group: Owner = serde_json::from_str(&json_group).unwrap();

    assert_eq!(owner_group, decoded_group);
}

//! Smoke tests for core type serialization.

use proxima_core::{GoalId, GroupId, MemoryId, Owner, SchemaId, SchemaVersion, UserId};
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

#[test]
fn test_id_newtypes_roundtrip() {
    let memory_id = MemoryId::new(Uuid::now_v7());
    let goal_id = GoalId::new(Uuid::now_v7());
    let schema_id = SchemaId::new("code/forgejo-commit".to_string());
    let schema_version = SchemaVersion::new(42);

    let json_memory = serde_json::to_string(&memory_id).unwrap();
    let decoded_memory: MemoryId = serde_json::from_str(&json_memory).unwrap();
    assert_eq!(memory_id, decoded_memory);

    let json_goal = serde_json::to_string(&goal_id).unwrap();
    let decoded_goal: GoalId = serde_json::from_str(&json_goal).unwrap();
    assert_eq!(goal_id, decoded_goal);

    let json_schema = serde_json::to_string(&schema_id).unwrap();
    let decoded_schema: SchemaId = serde_json::from_str(&json_schema).unwrap();
    assert_eq!(schema_id, decoded_schema);

    let json_version = serde_json::to_string(&schema_version).unwrap();
    let decoded_version: SchemaVersion = serde_json::from_str(&json_version).unwrap();
    assert_eq!(schema_version, decoded_version);
}

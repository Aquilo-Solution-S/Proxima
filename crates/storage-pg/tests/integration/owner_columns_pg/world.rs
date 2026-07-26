//! World ownership: transfers, the raw-verb rejections, and publish/query behaviour.

use super::{
    assert_no_live_entity_lacks_home, assert_single_home, authz_with_role, fresh_goal_draft,
    granted_authz, insert_self, owner_write_permit, read_owners, seed_memory_owned,
};

use crate::common;
use proxima_core::storage_ports::*;
use proxima_core::verbs::goal_write::{
    CreateGoalAtomicRequest, GoalAssignmentTarget, GoalAtomicContext, GoalTopologyWrite,
};
use proxima_core::verbs::query::QueryRequest;
use proxima_core::{
    Engine, EntityId, EntityKind, ErrorCode, FlavorRegistry, GroupId, OwnerRef, Role, UserId,
};
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test]
async fn transfer_to_world_moves_memory_row_and_stale_owner_replay_no_ops() {
    let (pg, db) = common::fresh_pg().await;
    let group = GroupId::new(Uuid::now_v7());
    let owner = OwnerRef::Group(group);
    let permit = owner_write_permit(&owner, proxima_core::AccessKind::Goal).await;
    let entity = seed_memory_owned(&pg, OwnerRef::Group(group)).await;

    let moved = pg.transfer_to_world(&permit, entity).await.unwrap();
    assert!(
        moved,
        "first transfer moves the row under its current owner"
    );
    assert_single_home(&pg, entity.uuid(), &OwnerRef::World).await;
    assert_no_live_entity_lacks_home(&pg).await;

    let stale_replay = pg.transfer_to_world(&permit, entity).await.unwrap();
    assert!(
        !stale_replay,
        "a transfer keyed on the now-stale prior owner finds no matching row"
    );
    assert_single_home(&pg, entity.uuid(), &OwnerRef::World).await;

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn transfer_to_world_moves_goal_row() {
    let (pg, db) = common::fresh_pg().await;
    let group = GroupId::new(Uuid::now_v7());
    let owner = OwnerRef::Group(group);
    let self_id = insert_self(&pg, &owner).await;
    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let mut draft = fresh_goal_draft(owner);
    let permit = owner_write_permit(&owner, proxima_core::AccessKind::Goal).await;
    draft.topology = GoalTopologyWrite::new(
        GoalAssignmentTarget::perspective(self_id),
        Vec::new(),
        Vec::new(),
    )
    .expect("empty test topology is valid");

    let outcome = pg
        .create_goal_atomic(
            &CreateGoalAtomicRequest {
                draft,
                context: GoalAtomicContext {
                    registry: &registry,
                    embedding_model_id: None,
                    author_self_perspective_id: Some(self_id),
                },
            },
            &permit,
        )
        .await
        .unwrap();
    let entity = EntityId::Goal(outcome.goal_id);

    let moved = pg.transfer_to_world(&permit, entity).await.unwrap();
    assert!(moved, "goal row transfers under its current owner");
    assert_single_home(&pg, entity.uuid(), &OwnerRef::World).await;
    assert_no_live_entity_lacks_home(&pg).await;

    common::drop_db(&db).await.unwrap();
}

/// Raw-verb backstop: `flavors/code`-style direct storage calls (no engine,
/// no `authorize_write`) must not be able to CREATE a memories row under
/// World now that the DDL check is gone (`0008_v005.sql`).
#[tokio::test]
async fn world_owner_rejected_on_raw_fact_ingest_verb() {
    let (pg, db) = common::fresh_pg().await;

    common::owner_write_permit(&OwnerRef::World, proxima_core::AccessKind::Fact)
        .await
        .expect_err("World Fact write permit must not be minted");

    let world_memories: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.memories WHERE owner_kind = 'world'")
            .fetch_one(pg.pool_for_tests())
            .await
            .unwrap();
    assert_eq!(world_memories, 0, "no World-owned memories row was created");

    common::drop_db(&db).await.unwrap();
}

/// Raw-verb backstop for the derived-memory surface `flavors/code` calls
/// directly (`append_derived_with_edges_in_tx`).
#[tokio::test]
async fn world_owner_rejected_on_raw_derive_append_verb() {
    let (_pg, db) = common::fresh_pg().await;

    common::owner_write_permit(&OwnerRef::World, proxima_core::AccessKind::Abstraction)
        .await
        .expect_err("World Abstraction write permit must not be minted");

    common::drop_db(&db).await.unwrap();
}

/// Raw-verb backstop for goal creation: the goals DDL check is also gone,
/// so the verb layer must reject a NEW World-owned goal row.
#[tokio::test]
async fn world_owner_rejected_on_raw_goal_write_verb() {
    let (pg, db) = common::fresh_pg().await;

    common::owner_write_permit(&OwnerRef::World, proxima_core::AccessKind::Goal)
        .await
        .expect_err("World Goal write permit must not be minted");

    let world_goals: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.goals WHERE owner_kind = 'world'")
            .fetch_one(pg.pool_for_tests())
            .await
            .unwrap();
    assert_eq!(world_goals, 0, "no World-owned goals row was created");

    common::drop_db(&db).await.unwrap();
}

/// Raw-verb backstop for the MCP-call-log Fact row (also a new memories row).
#[tokio::test]
async fn world_owner_rejected_on_raw_persist_mcp_call_verb() {
    let (_pg, db) = common::fresh_pg().await;

    common::owner_write_permit(&OwnerRef::World, proxima_core::AccessKind::Fact)
        .await
        .expect_err("World MCP-call Fact write permit must not be minted");

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn engine_publish_to_world_gates_admin_denies_rewrite_and_allows_ordinary_reads() {
    let (pg, db) = common::fresh_pg().await;
    let group = GroupId::new(Uuid::now_v7());
    let admin = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let editor = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let outsider = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let entity = seed_memory_owned(&pg, OwnerRef::Group(group)).await;

    let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports());

    let editor_err = engine
        .publish_to_world(
            &authz_with_role(&editor, OwnerRef::Group(group), Role::editor()),
            entity,
        )
        .await
        .expect_err("editor write ceiling never reaches Relation::Admin");
    assert_eq!(editor_err.code, ErrorCode::Forbidden);

    let outsider_err = engine
        .publish_to_world(&granted_authz(&outsider), entity)
        .await
        .expect_err("a for_subject context with no group role is denied");
    assert_eq!(outsider_err.code, ErrorCode::Forbidden);
    assert_single_home(&pg, entity.uuid(), &OwnerRef::Group(group)).await;

    engine
        .publish_to_world(
            &authz_with_role(&admin, OwnerRef::Group(group), Role::admin()),
            entity,
        )
        .await
        .expect("group admin may publish");
    assert_single_home(&pg, entity.uuid(), &OwnerRef::World).await;
    assert_no_live_entity_lacks_home(&pg).await;

    let republish_err = engine
        .publish_to_world(
            &authz_with_role(&admin, OwnerRef::Group(group), Role::admin()),
            entity,
        )
        .await
        .expect_err("World is never a write owner again — re-publish fails closed");
    assert_eq!(republish_err.code, ErrorCode::Forbidden);

    let outsider_reads = read_owners(&pg, &outsider).await;
    assert!(
        pg.visible_to_any(entity, &outsider_reads).await.unwrap(),
        "a World-owned entity is readable by an ordinary subject with zero group role"
    );

    common::drop_db(&db).await.unwrap();
}

/// Personal-owner publish: a bare `for_subject` context (no resolver group
/// roles) may publish the subject's OWN content — `Role::personal()`'s write
/// ceiling clears `authorize_write(..., Relation::Admin)` for the subject's
/// Personal owner — while another user cannot publish it, and post-publish
/// the entity is World-owned and readable by strangers.
#[tokio::test]
async fn engine_publish_to_world_personal_owner_self_publish() {
    let (pg, db) = common::fresh_pg().await;
    let author = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let stranger = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let entity = seed_memory_owned(&pg, author).await;

    let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports());

    let stranger_err = engine
        .publish_to_world(&granted_authz(&stranger), entity)
        .await
        .expect_err("another user cannot publish someone else's personal content");
    assert_eq!(stranger_err.code, ErrorCode::Forbidden);
    assert_single_home(&pg, entity.uuid(), &author).await;

    engine
        .publish_to_world(&granted_authz(&author), entity)
        .await
        .expect("a personal owner may publish their own content with no explicit roles");
    assert_single_home(&pg, entity.uuid(), &OwnerRef::World).await;
    assert_no_live_entity_lacks_home(&pg).await;

    let republish_err = engine
        .publish_to_world(&granted_authz(&author), entity)
        .await
        .expect_err("World is never a write owner again — re-publish fails closed");
    assert_eq!(republish_err.code, ErrorCode::Forbidden);

    let stranger_reads = read_owners(&pg, &stranger).await;
    assert!(
        pg.visible_to_any(entity, &stranger_reads).await.unwrap(),
        "a published personal entity is readable by an unrelated user"
    );

    common::drop_db(&db).await.unwrap();
}

/// Read half of Goal publish: `query_goals`'s
/// owner join used plain `g.owner_id = s.id`, and both sides are NULL for the
/// World slot (`goals.owner_id` after a publish transfer; `s.id` for the World
/// member of every caller's read-owner set), so `NULL = NULL → NULL` silently
/// hid every published Goal from every reader — the exact `memories.rs` trap,
/// one file over. This proves a published Goal surfaces through `Engine::query`
/// for a caller with no relationship to the original owner.
#[tokio::test]
async fn engine_query_surfaces_world_published_goal_to_non_owner() {
    let (pg, db) = common::fresh_pg().await;
    let author = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let stranger = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let self_id = insert_self(&pg, &author).await;
    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let mut draft = fresh_goal_draft(author);
    let permit = owner_write_permit(&author, proxima_core::AccessKind::Goal).await;
    draft.topology = GoalTopologyWrite::new(
        GoalAssignmentTarget::perspective(self_id),
        Vec::new(),
        Vec::new(),
    )
    .expect("empty test topology is valid");
    let outcome = pg
        .create_goal_atomic(
            &CreateGoalAtomicRequest {
                draft,
                context: GoalAtomicContext {
                    registry: &registry,
                    embedding_model_id: None,
                    author_self_perspective_id: Some(self_id),
                },
            },
            &permit,
        )
        .await
        .unwrap();
    let goal_id = outcome.goal_id;

    let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports());

    // Pre-publish control: the stranger cannot see the author's Goal.
    let mut req = QueryRequest::for_owner(stranger);
    req.entity_kind = Some(EntityKind::Goal);
    req.goal_ids = vec![goal_id];
    let before = engine.query(&granted_authz(&stranger), &req).await.unwrap();
    assert!(
        before.goals.is_empty(),
        "an unpublished Goal must not be visible to an unrelated user"
    );

    engine
        .publish_to_world(&granted_authz(&author), EntityId::Goal(goal_id))
        .await
        .expect("author publishes their own goal");
    assert_single_home(&pg, goal_id.into_inner(), &OwnerRef::World).await;

    // Post-publish: same stranger, same query — the World-owned Goal must
    // surface (World is in every caller's authz-resolved read set).
    let after = engine.query(&granted_authz(&stranger), &req).await.unwrap();
    assert_eq!(
        after.goals.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![goal_id],
        "a World-published Goal must surface through Engine::query for a non-owner reader"
    );
    assert_eq!(after.goals[0].owner, OwnerRef::World);

    // The pure listing path (no goal_ids hydration) shares the same owner
    // join; prove it too so the fix is covered for discovery-style reads.
    let mut list_req = QueryRequest::for_owner(stranger);
    list_req.entity_kind = Some(EntityKind::Goal);
    let listed = engine
        .query(&granted_authz(&stranger), &list_req)
        .await
        .unwrap();
    assert!(
        listed.goals.iter().any(|row| row.id == goal_id),
        "a World-published Goal must also surface in an unhydrated Goal listing"
    );

    common::drop_db(&db).await.unwrap();
}

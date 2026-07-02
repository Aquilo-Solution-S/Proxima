use crate::common;
use proxima_core::FactReceiptDraft;
use proxima_core::storage_ports::*;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::{
    CreateGoalAtomicRequest, GoalAssignmentTarget, GoalAtomicContext, GoalAuthorship, GoalDraft,
    GoalState, GoalTopologyWrite,
};
use proxima_core::verbs::query::{
    MemorySearchRequest, QueryRequest, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
};
use proxima_core::{
    AuthPath, AuthzContext, Engine, EntityId, EntityKind, ErrorCode, FlavorRegistry, GroupId,
    MemoryId, MemoryOperatorKind, Owner, OwnerAccessPort, OwnerRef, OwnerRefKind, Relation, Role,
    SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId,
};
use proxima_storage_pg::{PgOwnerAccessResolver, PgStorage};
use std::sync::Arc;
use uuid::Uuid;

type ResolvedAuthz = AuthzContext;

fn fresh_fact_draft(_owner: Owner) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: SchemaId::new("test/entity-owner-fact".into()),
        schema_version: SchemaVersion::new(1),
        payload: Uuid::now_v7().as_bytes().to_vec(),
        rendered_text: Some("entity owner fact".to_string()),
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new("test/entity-owner-source"),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
        citation: None,
    }
}

fn fresh_goal_draft(owner: Owner) -> GoalDraft {
    GoalDraft {
        owner,
        schema_id: SchemaId::new("core/simple-text-v1".into()),
        schema_version: SchemaVersion::new(1),
        title: "Home-row goal".to_string(),
        text: "Goal created by owner_columns_pg".to_string(),
        payload: b"{}".to_vec(),
        sidecar_payload: None,
        state: GoalState::Active,
        topology: GoalTopologyWrite::new(
            GoalAssignmentTarget::perspective(MemoryId::new(Uuid::nil())),
            Vec::new(),
            Vec::new(),
        )
        .expect("empty test topology is valid"),
        wake: None,
        supersedes_goal_id: None,
        authorship: GoalAuthorship::User,
        request_id: format!("entity-owner-home:{}", Uuid::now_v7()),
    }
}

async fn assert_single_home(pg: &PgStorage, entity_id: Uuid, owner: &OwnerRef) -> Option<Uuid> {
    let rows: Vec<(OwnerRefKind, Option<Uuid>)> = sqlx::query_as(
        "SELECT owner_kind, owner_id
           FROM proxima_core.memories
          WHERE memory_id = $1
         UNION ALL
         SELECT owner_kind, owner_id
           FROM proxima_core.goals
          WHERE goal_id = $1",
    )
    .bind(entity_id)
    .fetch_all(pg.pool_for_tests())
    .await
    .unwrap();

    assert_eq!(rows.len(), 1, "entity must have exactly one owned row");
    let expected = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    assert_eq!(rows[0], expected);
    None
}

async fn assert_no_live_entity_lacks_home(pg: &PgStorage) {
    let missing: i64 = sqlx::query_scalar(
        "SELECT (
             SELECT count(*) FROM proxima_core.memories m
              WHERE m.tombstoned_at IS NULL
                AND NOT ((m.owner_kind = 'world' AND m.owner_id IS NULL)
                     OR (m.owner_kind IN ('personal', 'group') AND m.owner_id IS NOT NULL))
         ) + (
             SELECT count(*) FROM proxima_core.goals g
              WHERE NOT ((g.owner_kind = 'world' AND g.owner_id IS NULL)
                     OR (g.owner_kind IN ('personal', 'group') AND g.owner_id IS NOT NULL))
         )",
    )
    .fetch_one(pg.pool_for_tests())
    .await
    .unwrap();
    assert_eq!(
        missing, 0,
        "live memories/goals must have valid owner columns"
    );
}

async fn insert_self(pg: &PgStorage, owner: &Owner) -> MemoryId {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    let memory_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'test/self', 1, $4,
                 'self', $5, '00000000-0000-0000-0000-000000000351'::uuid,
                 '00000000-0000-0000-0000-000000000352'::uuid, NULL,
                 'test-model', 'v1')"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(EntityKind::Perspective)
    .bind(MemoryOperatorKind::AtoP)
    .execute(pg.pool_for_tests())
    .await
    .unwrap();
    MemoryId::new(memory_id)
}

#[tokio::test]
async fn migration_creates_owner_columns_and_membership() {
    let (pg, db) = common::fresh_pg().await;
    let pool = pg.pool_for_tests();

    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM information_schema.tables
          WHERE table_schema='proxima_core'
            AND table_name IN ('group_memberships')",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(n, 1, "group membership table exists");
    let stale_owner_table: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('proxima_core.' || 'entity_' || 'owner')::text")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(stale_owner_table, None);

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn visible_to_any_respects_membership() {
    let (pg, db) = common::fresh_pg().await;
    let p = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let q = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let g1 = GroupId::new(uuid::Uuid::now_v7());

    seed_membership(&pg, g1, &p, Relation::Viewer).await;
    seed_membership(&pg, g1, &q, Relation::Viewer).await;
    let f1 = seed_memory_owned(&pg, OwnerRef::Group(g1)).await;
    let a = seed_memory_owned(&pg, p).await;
    let s_p = read_owners(&pg, &p).await;
    let s_q = read_owners(&pg, &q).await;

    assert!(pg.visible_to_any(f1, &s_p).await.unwrap());
    assert!(pg.visible_to_any(a, &s_p).await.unwrap());
    assert!(pg.visible_to_any(f1, &s_q).await.unwrap());
    assert!(
        !pg.visible_to_any(a, &s_q).await.unwrap(),
        "A is personal to P"
    );

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn group_membership_verbs_round_trip_and_engine_gates_admin_editor() {
    let (pg, db) = common::fresh_pg().await;
    let admin = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let viewer = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let outsider = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let group = GroupId::new(uuid::Uuid::now_v7());
    let OwnerRef::Personal(admin_id) = admin else {
        unreachable!("admin is user")
    };
    let OwnerRef::Personal(viewer_id) = viewer else {
        unreachable!("viewer is user")
    };
    let OwnerRef::Personal(outsider_id) = outsider else {
        unreachable!("outsider is user")
    };

    pg.add_group_member(group, viewer_id, Relation::Viewer, Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(
        pg.list_group_members(group).await.unwrap(),
        vec![(viewer_id, Relation::Viewer)]
    );

    let group_entity = seed_memory_owned(&pg, OwnerRef::Group(group)).await;
    let viewer_read_owners = read_owners(&pg, &viewer).await;
    assert!(
        pg.visible_to_any(group_entity, &viewer_read_owners)
            .await
            .unwrap(),
        "viewer membership enters S_read and reaches group-owned entity"
    );

    pg.remove_group_member(group, viewer_id).await.unwrap();
    assert!(pg.list_group_members(group).await.unwrap().is_empty());

    pg.add_group_member(group, admin_id, Relation::Admin, Uuid::now_v7())
        .await
        .unwrap();
    pg.add_group_member(group, viewer_id, Relation::Viewer, Uuid::now_v7())
        .await
        .unwrap();
    let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports());
    let outsider_err = engine
        .add_member(&granted_authz(&outsider), group, admin_id, Relation::Viewer)
        .await
        .expect_err("non-admin add_member must be forbidden");
    assert_eq!(outsider_err.code, ErrorCode::Forbidden);

    engine
        .add_member(
            &authz_with_role(&admin, OwnerRef::Group(group), Role::admin()),
            group,
            outsider_id,
            Relation::Viewer,
        )
        .await
        .unwrap();
    assert!(
        engine
            .list_members(
                &authz_with_role(&admin, OwnerRef::Group(group), Role::admin(),),
                group
            )
            .await
            .unwrap()
            .contains(&(outsider_id, Relation::Viewer))
    );
    engine
        .remove_member(
            &authz_with_role(&admin, OwnerRef::Group(group), Role::admin()),
            group,
            outsider_id,
        )
        .await
        .unwrap();
    assert!(
        !engine
            .list_members(
                &authz_with_role(&admin, OwnerRef::Group(group), Role::admin(),),
                group
            )
            .await
            .unwrap()
            .iter()
            .any(|(member, _)| *member == outsider_id)
    );

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn discovery_reads_filter_by_owner_read_set() {
    let (pg, db) = common::fresh_pg().await;
    let p = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let q = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let g1 = GroupId::new(uuid::Uuid::now_v7());

    seed_membership(&pg, g1, &q, Relation::Viewer).await;

    let mut f1_draft = fresh_fact_draft(OwnerRef::Group(g1));
    f1_draft.rendered_text = Some("boundaryneedle group fact".to_string());
    let f1 = pg
        .ingest_fact_atomic(&OwnerRef::Group(g1), &f1_draft, None)
        .await
        .unwrap()
        .memory_id;
    let mut hidden_target_draft = fresh_fact_draft(OwnerRef::Group(g1));
    hidden_target_draft.rendered_text = Some("unreadable edge target".to_string());
    let hidden_target = pg
        .ingest_fact_atomic(&OwnerRef::Group(g1), &hidden_target_draft, None)
        .await
        .unwrap()
        .memory_id;
    move_home_row(&pg, hidden_target, &p).await;
    let a = seed_abstraction_memory(
        &pg,
        &p,
        OwnerRef::Group(g1),
        "boundaryneedle personal abstraction",
    )
    .await;
    let leaky_edge = seed_edge_between_memories(&pg, OwnerRef::Group(g1), f1, hidden_target).await;
    let q_read_owners = read_owners(&pg, &q).await;

    let query = pg
        .query_memories(
            &QueryRequest {
                owner: q,
                read_owners: q_read_owners.clone(),
                entity_kind: None,
                schema_id: None,
                supersession: SupersessionStatus::HeadsOnly,
                tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
                limit: 50,
                page: proxima_core::verbs::query::QueryPage::default(),
                include_payloads: false,
                memory_ids: Vec::new(),
                goal_ids: Vec::new(),
                edge_ids: Vec::new(),
                stateful_heads: Vec::new(),
            },
            &[],
        )
        .await
        .unwrap();
    let query_ids = query.memories.iter().map(|row| row.id).collect::<Vec<_>>();
    assert!(query_ids.contains(&f1));
    assert!(
        !query_ids.contains(&hidden_target),
        "Q must not query P-owned target Fact"
    );
    assert!(!query_ids.contains(&a), "Q must not query P's singleton A");
    assert!(
        query.edges.iter().all(|edge| edge.id != leaky_edge),
        "query_memories must not return an edge whose target is unreadable"
    );

    let edge_by_id = pg
        .query_memories(
            &QueryRequest {
                owner: q,
                read_owners: q_read_owners.clone(),
                entity_kind: None,
                schema_id: None,
                supersession: SupersessionStatus::HeadsOnly,
                tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
                limit: 50,
                page: proxima_core::verbs::query::QueryPage::default(),
                include_payloads: false,
                memory_ids: Vec::new(),
                goal_ids: Vec::new(),
                edge_ids: vec![leaky_edge],
                stateful_heads: Vec::new(),
            },
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        edge_by_id.edges.len(),
        1,
        "edge-id hydration is source-owned and keeps a target stub"
    );
    assert_eq!(edge_by_id.edges[0].id, leaky_edge);
    assert_eq!(
        edge_by_id.edges[0].target,
        proxima_core::verbs::query::EdgeTargetProjection::Redacted,
        "unreadable edge target is redacted"
    );

    let search = pg
        .search_memories(
            &MemorySearchRequest {
                owner: q,
                read_owners: q_read_owners,
                query: "boundaryneedle".to_string(),
                mode: SearchMode::Lexical,
                supersession: SupersessionStatus::HeadsOnly,
                limit: 10,
                kind: None,
                schema_id: None,
                tags: Vec::new(),
                tag_match: TagMatch::Any,
                since: None,
                until: None,
                order: SearchOrder::Relevance,
                query_embedding: None,
                embedding_model_id: None,
            },
            &[],
        )
        .await
        .unwrap();
    let search_ids = search.iter().map(|row| row.memory_id).collect::<Vec<_>>();
    assert!(search_ids.contains(&f1));
    assert!(
        !search_ids.contains(&a),
        "Q must not search P's singleton A"
    );

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn owner_columns_written_on_fact_ingest() {
    let (pg, db) = common::fresh_pg().await;
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));

    let outcome = pg
        .ingest_fact_atomic(&owner, &fresh_fact_draft(owner), None)
        .await
        .unwrap();

    assert_single_home(&pg, outcome.memory_id.into_inner(), &owner).await;
    assert_no_live_entity_lacks_home(&pg).await;

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn owner_columns_written_on_goal_create() {
    let (pg, db) = common::fresh_pg().await;
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let self_id = insert_self(&pg, &owner).await;
    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let mut draft = fresh_goal_draft(owner);
    draft.topology = GoalTopologyWrite::new(
        GoalAssignmentTarget::perspective(self_id),
        Vec::new(),
        Vec::new(),
    )
    .expect("empty test topology is valid");

    let outcome = pg
        .create_goal_atomic(&CreateGoalAtomicRequest {
            draft,
            context: GoalAtomicContext {
                registry: &registry,
                embedding_model_id: None,
                author_self_perspective_id: Some(self_id),
            },
        })
        .await
        .unwrap();

    assert_single_home(&pg, outcome.goal_id.into_inner(), &owner).await;
    assert_no_live_entity_lacks_home(&pg).await;

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn transfer_to_world_moves_memory_row_and_stale_owner_replay_no_ops() {
    let (pg, db) = common::fresh_pg().await;
    let group = GroupId::new(Uuid::now_v7());
    let entity = seed_memory_owned(&pg, OwnerRef::Group(group)).await;

    let moved = pg
        .transfer_to_world(entity, OwnerRef::Group(group))
        .await
        .unwrap();
    assert!(
        moved,
        "first transfer moves the row under its current owner"
    );
    assert_single_home(&pg, entity.uuid(), &OwnerRef::World).await;
    assert_no_live_entity_lacks_home(&pg).await;

    let stale_replay = pg
        .transfer_to_world(entity, OwnerRef::Group(group))
        .await
        .unwrap();
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
    draft.topology = GoalTopologyWrite::new(
        GoalAssignmentTarget::perspective(self_id),
        Vec::new(),
        Vec::new(),
    )
    .expect("empty test topology is valid");

    let outcome = pg
        .create_goal_atomic(&CreateGoalAtomicRequest {
            draft,
            context: GoalAtomicContext {
                registry: &registry,
                embedding_model_id: None,
                author_self_perspective_id: Some(self_id),
            },
        })
        .await
        .unwrap();
    let entity = EntityId::Goal(outcome.goal_id);

    let moved = pg.transfer_to_world(entity, owner).await.unwrap();
    assert!(moved, "goal row transfers under its current owner");
    assert_single_home(&pg, entity.uuid(), &OwnerRef::World).await;
    assert_no_live_entity_lacks_home(&pg).await;

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

#[tokio::test]
async fn pg_owner_access_resolver_resolves_group_roles_via_owner_access_port() {
    let (pg, db) = common::fresh_pg().await;
    let subject = UserId::new(Uuid::now_v7());
    let group = GroupId::new(Uuid::now_v7());

    seed_membership(&pg, group, &OwnerRef::Personal(subject), Relation::Editor).await;

    let resolver = PgOwnerAccessResolver::new(pg.pool_for_tests().clone());
    let roles = resolver
        .resolve_roles_for_subject(subject)
        .await
        .expect("resolves group roles");

    assert!(roles.may_write(
        &OwnerRef::Group(group),
        proxima_core::AccessKind::Perspective
    ));
    assert!(!roles.may_manage(&OwnerRef::Group(group)));
    assert!(roles.may_read(&OwnerRef::World, proxima_core::AccessKind::Goal));

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn pg_owner_access_resolver_resolve_roles_is_empty_for_unknown_subject() {
    let (pg, db) = common::fresh_pg().await;
    let subject = UserId::new(Uuid::now_v7());

    let resolver = PgOwnerAccessResolver::new(pg.pool_for_tests().clone());
    let roles = resolver
        .resolve_roles_for_subject(subject)
        .await
        .expect("resolves with no membership rows");

    assert!(
        roles
            .writable_owners(proxima_core::AccessKind::Perspective)
            .iter()
            .all(|owner| !matches!(owner, OwnerRef::Group(_)))
    );

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn has_role_for_owner_is_a_positive_and_negative_point_in_time_probe() {
    let (pg, db) = common::fresh_pg().await;
    let admin = UserId::new(Uuid::now_v7());
    let viewer = UserId::new(Uuid::now_v7());
    let group = GroupId::new(Uuid::now_v7());
    let other_group = GroupId::new(Uuid::now_v7());

    seed_membership(&pg, group, &OwnerRef::Personal(admin), Relation::Admin).await;
    seed_membership(&pg, group, &OwnerRef::Personal(viewer), Relation::Viewer).await;

    let resolver = PgOwnerAccessResolver::new(pg.pool_for_tests().clone());

    assert!(
        resolver
            .has_role_for_owner(admin, OwnerRef::Group(group), Relation::Admin)
            .await
            .unwrap()
    );
    assert!(
        !resolver
            .has_role_for_owner(admin, OwnerRef::Group(group), Relation::Editor)
            .await
            .unwrap(),
        "exact-relation probe must not treat Admin as satisfying an Editor probe"
    );
    assert!(
        !resolver
            .has_role_for_owner(viewer, OwnerRef::Group(group), Relation::Admin)
            .await
            .unwrap()
    );
    assert!(
        !resolver
            .has_role_for_owner(admin, OwnerRef::Group(other_group), Relation::Admin)
            .await
            .unwrap(),
        "membership on a different group must not satisfy the probe"
    );
    assert!(
        !resolver
            .has_role_for_owner(admin, OwnerRef::Personal(admin), Relation::Admin)
            .await
            .unwrap(),
        "Personal owners carry no membership row; the probe fails closed"
    );
    assert!(
        !resolver
            .has_role_for_owner(admin, OwnerRef::World, Relation::Viewer)
            .await
            .unwrap(),
        "World carries no membership row; the probe fails closed"
    );

    common::drop_db(&db).await.unwrap();
}

async fn seed_membership(
    pg: &proxima_storage_pg::PgStorage,
    group: GroupId,
    member: &OwnerRef,
    relation: Relation,
) {
    let OwnerRef::Personal(user) = member else {
        panic!("seed_membership only accepts user members");
    };

    sqlx::query(
        "INSERT INTO proxima_core.group_memberships
            (group_id, member_user_id, relation)
         VALUES ($1,$2,$3::proxima_core.membership_relation)",
    )
    .bind(group.into_inner())
    .bind(user.into_inner())
    .bind(relation)
    .execute(pg.pool_for_tests())
    .await
    .unwrap();
}

async fn seed_memory_owned(pg: &proxima_storage_pg::PgStorage, owner: OwnerRef) -> EntityId {
    let entity_id = uuid::Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'test/owned-memory-v1', 1, 'Abstraction', 'owned',
                 'AtoA', '00000000-0000-0000-0000-000000000353'::uuid,
                 '00000000-0000-0000-0000-000000000354'::uuid, NULL,
                 'test-model', 'v1')"
    )
    .bind(entity_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await
    .unwrap();
    EntityId::Memory(MemoryId::new(entity_id))
}

async fn seed_abstraction_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &OwnerRef,
    _ignored_owner_stamp: OwnerRef,
    text: &str,
) -> MemoryId {
    let memory_id = uuid::Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'test/entity-owner-abstraction-v1', 1,
                 'Abstraction', $4, 'AtoA',
                 '00000000-0000-0000-0000-000000000355'::uuid,
                 '00000000-0000-0000-0000-000000000356'::uuid, NULL,
                 'test-model', 'v1')"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(text)
    .execute(pg.pool_for_tests())
    .await
    .unwrap();
    MemoryId::new(memory_id)
}

async fn move_home_row(pg: &proxima_storage_pg::PgStorage, memory_id: MemoryId, owner: &OwnerRef) {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "UPDATE proxima_core.memories
            SET owner_kind = $2::proxima_core.owner_ref_kind,
                owner_id = $3
          WHERE memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await
    .unwrap();
}

async fn seed_edge_between_memories(
    pg: &proxima_storage_pg::PgStorage,
    owner: OwnerRef,
    source: MemoryId,
    target: MemoryId,
) -> uuid::Uuid {
    let edge_id = uuid::Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
    sqlx::query(
        "INSERT INTO proxima_core.edges
           (edge_id, owner_kind, owner_id, relation, relation_class,
            source_kind, source_memory_id, source_goal_id,
            target_kind, target_memory_id, target_goal_id,
            authorship_kind, authorship_owner_memory_id)
         VALUES
           ($1, $2, $3, 'test/leaky-edge', 'Structural',
            'Fact', $4, NULL,
            'Fact', $5, NULL,
            'SourceIngest', NULL)",
    )
    .bind(edge_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(source.into_inner())
    .bind(target.into_inner())
    .execute(pg.pool_for_tests())
    .await
    .unwrap();
    edge_id
}

async fn read_owners(pg: &proxima_storage_pg::PgStorage, principal: &OwnerRef) -> Vec<OwnerRef> {
    let mut owners = vec![*principal];
    owners.extend(
        pg.resolve_membership(principal)
            .await
            .unwrap()
            .into_iter()
            .map(|membership| OwnerRef::Group(membership.group)),
    );
    owners.push(proxima_core::access::world());
    owners
}

fn granted_authz(principal: &OwnerRef) -> ResolvedAuthz {
    let OwnerRef::Personal(user) = *principal else {
        panic!("test principal must be a user");
    };
    AuthzContext::for_subject(user, AuthPath::HostBearer)
}

fn authz_with_role(principal: &OwnerRef, owner: OwnerRef, role: Role) -> ResolvedAuthz {
    let OwnerRef::Personal(user) = *principal else {
        panic!("test principal must be a user");
    };
    AuthzContext::for_subject_with_role(user, [(owner, role)], AuthPath::HostBearer)
}

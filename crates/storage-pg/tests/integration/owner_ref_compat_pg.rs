use crate::common;
use proxima_core::verbs::event_ingest::EventDraft;
use proxima_core::verbs::goal_write::{
    CreateGoalAtomicRequest, GoalAtomicContext, GoalAuthorship, GoalDraft, GoalState,
};
use proxima_core::verbs::query::{
    MemorySearchRequest, QueryRequest, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
};
use proxima_core::{
    AuthPath, AuthzContext, Engine, EntityId, EntityKind, ErrorCode, FlavorRegistry, GroupId,
    MemoryId, MemoryOperatorKind, Owner, OwnerRef, OwnerRefKind, Relation, Role, SchemaId,
    SchemaVersion, SourceBatchId, SourceId, Storage, UserId,
};
use proxima_storage_pg::PgStorage;
use std::sync::Arc;
use uuid::Uuid;

type ResolvedAuthz = AuthzContext;

fn fresh_event_draft(owner: Owner) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("test/entity-owner-source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: owner,
        author_personality_instance_id: None,
        schema_id: SchemaId::new("test/entity-owner-fact".into()),
        schema_version: SchemaVersion::new(1),
        payload: Uuid::now_v7().as_bytes().to_vec(),
        rendered_text: Some("entity owner fact".to_string()),
        observed_at: now,
        occurred_at: now,
        citation: None,
    }
}

fn fresh_goal_draft(owner: Owner) -> GoalDraft {
    GoalDraft {
        principal: owner,
        schema_id: SchemaId::new("core/simple-text-v1".into()),
        schema_version: SchemaVersion::new(1),
        title: "Home-row goal".to_string(),
        text: "Goal created by owner_ref_compat_pg".to_string(),
        payload: b"{}".to_vec(),
        sidecar_payload: None,
        state: GoalState::Active,
        parent_goal_ids: vec![],
        supersedes_goal_id: None,
        authorship: GoalAuthorship::User,
        request_id: format!("entity-owner-home:{}", Uuid::now_v7()),
    }
}

async fn assert_single_home(pg: &PgStorage, entity_id: Uuid, owner: &OwnerRef) -> Option<Uuid> {
    let rows: Vec<(OwnerRefKind, Uuid, bool, Option<Uuid>)> =
        sqlx::query_as(proxima_storage_pg::access::owner_ref_compat::sql(
            "SELECT owner_principal_kind, owner_principal_id, is_home, granted_by
           FROM __PROXIMA_ENTITY_OWNER__
          WHERE entity_id = $1",
        ))
        .bind(entity_id)
        .fetch_all(pg.pool())
        .await
        .unwrap();

    assert_eq!(rows.len(), 1, "entity must have exactly one owner row");
    let (owner_kind, owner_principal_id, is_home, granted_by) = rows[0];
    let expected = owner.columns();
    assert_eq!((owner_kind, owner_principal_id), expected);
    assert!(is_home, "owner row must be home");
    granted_by
}

async fn assert_no_live_entity_lacks_home(pg: &PgStorage) {
    let missing: i64 = sqlx::query_scalar(proxima_storage_pg::access::owner_ref_compat::sql(
        "SELECT (
             SELECT count(*) FROM proxima_core.memories m
              WHERE m.tombstoned_at IS NULL
                AND NOT EXISTS (
                    SELECT 1 FROM __PROXIMA_ENTITY_OWNER__ eo
                     WHERE eo.entity_id = m.memory_id AND eo.is_home
                )
         ) + (
             SELECT count(*) FROM proxima_core.goals g
              WHERE NOT EXISTS (
                    SELECT 1 FROM __PROXIMA_ENTITY_OWNER__ eo
                     WHERE eo.entity_id = g.goal_id AND eo.is_home
              )
         )",
    ))
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert_eq!(missing, 0, "live memories/goals must have a home row");
}

async fn insert_self(pg: &PgStorage, owner: &Owner) -> MemoryId {
    let (owner_kind, owner_principal_id) = owner.columns();
    let memory_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id)
         VALUES ($1, 'test/self', 1, $2,
                 'self', $3, 'test-model', 'v1', $4)",
    )
    .bind(memory_id)
    .bind(EntityKind::Perspective)
    .bind(MemoryOperatorKind::AtoP)
    .bind(Uuid::nil())
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query(proxima_storage_pg::access::owner_ref_compat::sql(
        "INSERT INTO __PROXIMA_ENTITY_OWNER__
            (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
         VALUES ($1, $2, $3, true, $4)",
    ))
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(Uuid::nil())
    .execute(pg.pool())
    .await
    .unwrap();
    MemoryId::new(memory_id)
}

#[tokio::test]
async fn migration_creates_owner_rows_and_membership() {
    let (pg, db) = common::fresh_pg().await;
    let pool = pg.pool();

    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM information_schema.tables
          WHERE table_schema='proxima_core'
            AND table_name IN ('entity_' || 'owner','group_membership')",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(n, 2, "both access tables exist");

    let eid = uuid::Uuid::now_v7();
    let ins = |kind: &str, home: bool| {
        sqlx::query(proxima_storage_pg::access::owner_ref_compat::sql(
            "INSERT INTO __PROXIMA_ENTITY_OWNER__
                (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
             VALUES ($1,$2::proxima_core.owner_principal_kind,$3,$4,$5)",
        ))
        .bind(eid)
        .bind(kind.to_string())
        .bind(uuid::Uuid::now_v7())
        .bind(home)
        .bind(uuid::Uuid::now_v7())
    };
    ins("User", true).execute(pool).await.unwrap();
    let dup = ins("Group", true).execute(pool).await;
    assert!(
        dup.is_err(),
        "second home row must violate home-owner uniqueness"
    );

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
    let engine = Engine::new(FlavorRegistry::new().freeze()).with_storage(Arc::new(pg.clone()));
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

    let mut f1_draft = fresh_event_draft(OwnerRef::Group(g1));
    f1_draft.rendered_text = Some("boundaryneedle group fact".to_string());
    let f1 = pg
        .ingest_event_atomic(&f1_draft, None)
        .await
        .unwrap()
        .memory_id;
    let mut hidden_target_draft = fresh_event_draft(OwnerRef::Group(g1));
    hidden_target_draft.rendered_text = Some("unreadable edge target".to_string());
    let hidden_target = pg
        .ingest_event_atomic(&hidden_target_draft, None)
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
                principal: q,
                read_owners: q_read_owners.clone(),
                entity_kind: None,
                schema_id: None,
                supersession: SupersessionStatus::HeadsOnly,
                tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
                personality_roots:
                    proxima_core::verbs::query::PersonalityRootFilter::IncludeInactive,
                limit: 50,
                include_payloads: false,
                memory_ids: Vec::new(),
                goal_ids: Vec::new(),
                edge_ids: Vec::new(),
                stateful_heads: Vec::new(),
                reader_personality_instance_id: None,
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
                principal: q,
                read_owners: q_read_owners.clone(),
                entity_kind: None,
                schema_id: None,
                supersession: SupersessionStatus::HeadsOnly,
                tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
                personality_roots:
                    proxima_core::verbs::query::PersonalityRootFilter::IncludeInactive,
                limit: 50,
                include_payloads: false,
                memory_ids: Vec::new(),
                goal_ids: Vec::new(),
                edge_ids: vec![leaky_edge],
                stateful_heads: Vec::new(),
                reader_personality_instance_id: None,
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
    assert!(
        !edge_by_id.edges[0].target_readable,
        "unreadable edge target is redacted"
    );

    let search = pg
        .search_memories(
            &MemorySearchRequest {
                principal: q,
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
                reader_personality_instance_id: None,
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
async fn home_row_created_on_event_ingest() {
    let (pg, db) = common::fresh_pg().await;
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));

    let outcome = pg
        .ingest_event_atomic(&fresh_event_draft(owner), None)
        .await
        .unwrap();

    let granted_by = assert_single_home(&pg, outcome.memory_id.into_inner(), &owner).await;
    assert_eq!(granted_by, Some(Uuid::nil()));
    assert_no_live_entity_lacks_home(&pg).await;

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn home_row_created_on_goal_create() {
    let (pg, db) = common::fresh_pg().await;
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let self_id = insert_self(&pg, &owner).await;
    let registry = FlavorRegistry::new().freeze();
    let draft = fresh_goal_draft(owner);

    let outcome = pg
        .create_goal_atomic(&CreateGoalAtomicRequest {
            draft,
            context: GoalAtomicContext {
                registry: &registry,
                embedding_model_id: None,
                author_self_perspective_id: Some(self_id),
            },
            target_self_perspective_id: self_id,
            evidence: Vec::new(),
        })
        .await
        .unwrap();

    let granted_by = assert_single_home(&pg, outcome.goal_id.into_inner(), &owner).await;
    assert_eq!(granted_by, None);
    assert_no_live_entity_lacks_home(&pg).await;

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
        "INSERT INTO proxima_core.group_membership
            (group_id, member_user_id, relation, granted_by)
         VALUES ($1,$2,$3::proxima_core.membership_relation,$4)",
    )
    .bind(group.into_inner())
    .bind(user.into_inner())
    .bind(relation)
    .bind(uuid::Uuid::now_v7())
    .execute(pg.pool())
    .await
    .unwrap();
}

async fn seed_memory_owned(pg: &proxima_storage_pg::PgStorage, owner: OwnerRef) -> EntityId {
    let entity_id = uuid::Uuid::now_v7();
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(proxima_storage_pg::access::owner_ref_compat::sql(
        "INSERT INTO __PROXIMA_ENTITY_OWNER__
            (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
         VALUES ($1,$2::proxima_core.owner_principal_kind,$3,true,$4)",
    ))
    .bind(entity_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(uuid::Uuid::now_v7())
    .execute(pg.pool())
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
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id)
         VALUES ($1, 'test/entity-owner-abstraction-v1', 1,
                 'Abstraction', $2, 'FtoA', 'test-model', 'v1', $3)",
    )
    .bind(memory_id)
    .bind(text)
    .bind(uuid::Uuid::nil())
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query(proxima_storage_pg::access::owner_ref_compat::sql(
        "INSERT INTO __PROXIMA_ENTITY_OWNER__
            (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
         VALUES ($1,$2::proxima_core.owner_principal_kind,$3,true,$4)",
    ))
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(uuid::Uuid::nil())
    .execute(pg.pool())
    .await
    .unwrap();
    MemoryId::new(memory_id)
}

async fn move_home_row(pg: &proxima_storage_pg::PgStorage, memory_id: MemoryId, owner: &OwnerRef) {
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(proxima_storage_pg::access::owner_ref_compat::sql(
        "UPDATE __PROXIMA_ENTITY_OWNER__
            SET owner_principal_kind = $2::proxima_core.owner_principal_kind,
                owner_principal_id = $3
          WHERE entity_id = $1 AND is_home",
    ))
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool())
    .await
    .unwrap();
}

async fn seed_edge_between_memories(
    pg: &proxima_storage_pg::PgStorage,
    _owner: OwnerRef,
    source: MemoryId,
    target: MemoryId,
) -> uuid::Uuid {
    let edge_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.edges
           (edge_id, relation, relation_class,
            source_kind, source_memory_id, source_goal_id,
            target_kind, target_memory_id, target_goal_id,
            authorship_kind, authorship_owner_memory_id)
         VALUES
           ($1, 'test/leaky-edge', 'Structural',
            'Fact', $2, NULL,
            'Fact', $3, NULL,
            'EventSource', NULL)",
    )
    .bind(edge_id)
    .bind(source.into_inner())
    .bind(target.into_inner())
    .execute(pg.pool())
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

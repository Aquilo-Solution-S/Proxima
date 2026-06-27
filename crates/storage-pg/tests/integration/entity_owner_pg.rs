use crate::common;
use proxima_core::verbs::event_ingest::EventDraft;
use proxima_core::verbs::goal_write::{
    CreateGoalAtomicRequest, GoalAtomicContext, GoalAuthorship, GoalDraft, GoalState,
};
use proxima_core::verbs::query::{
    MemorySearchRequest, QueryRequest, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
};
use proxima_core::{
    AccessScope, AuthPath, AuthzContext, CapabilitySet, Engine, EntityId, EntityKind, ErrorCode,
    FlavorRegistry, GroupId, Identity, MemoryId, MemoryOperatorKind, Owner, OwnerPrincipalKind,
    Principal, Relation, RemoveOwnerOutcome, SchemaId, SchemaVersion, SourceBatchId, SourceId,
    Storage, ToolScope, UserId,
};
use proxima_storage_pg::PgStorage;
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

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
        text: "Goal created by entity_owner_pg".to_string(),
        payload: b"{}".to_vec(),
        sidecar_payload: None,
        state: GoalState::Active,
        parent_goal_ids: vec![],
        supersedes_goal_id: None,
        authorship: GoalAuthorship::User,
        request_id: format!("entity-owner-home:{}", Uuid::now_v7()),
    }
}

async fn assert_single_home(pg: &PgStorage, entity_id: Uuid, owner: &Principal) -> Option<Uuid> {
    let rows: Vec<(OwnerPrincipalKind, Uuid, bool, Option<Uuid>)> = sqlx::query_as(
        "SELECT owner_principal_kind, owner_principal_id, is_home, granted_by
           FROM proxima_core.entity_owner
          WHERE entity_id = $1",
    )
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
    let missing: i64 = sqlx::query_scalar(
        "SELECT (
             SELECT count(*) FROM proxima_core.memories m
              WHERE m.tombstoned_at IS NULL
                AND NOT EXISTS (
                    SELECT 1 FROM proxima_core.entity_owner eo
                     WHERE eo.entity_id = m.memory_id AND eo.is_home
                )
         ) + (
             SELECT count(*) FROM proxima_core.goals g
              WHERE NOT EXISTS (
                    SELECT 1 FROM proxima_core.entity_owner eo
                     WHERE eo.entity_id = g.goal_id AND eo.is_home
              )
         )",
    )
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
            (memory_id, owner_principal_kind, owner_principal_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id)
         VALUES ($1, $2, $3, 'test/self', 1, $4,
                 'self', $5, 'test-model', 'v1', $6)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(EntityKind::Perspective)
    .bind(MemoryOperatorKind::AtoP)
    .bind(Uuid::nil())
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO proxima_core.entity_owner
            (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
         VALUES ($1, $2, $3, true, $4)",
    )
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
async fn migration_creates_entity_owner_and_membership() {
    let (pg, db) = common::fresh_pg().await;
    let pool = pg.pool();

    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM information_schema.tables
          WHERE table_schema='proxima_core'
            AND table_name IN ('entity_owner','group_membership')",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(n, 2, "both access tables exist");

    let eid = uuid::Uuid::now_v7();
    let ins = |kind: &str, home: bool| {
        sqlx::query(
            "INSERT INTO proxima_core.entity_owner
                (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
             VALUES ($1,$2::proxima_core.owner_principal_kind,$3,$4,$5)",
        )
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
        "second home row must violate uq_entity_owner_home"
    );

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn entity_is_readable_respects_membership() {
    let (pg, db) = common::fresh_pg().await;
    let p = Principal::User(UserId::new(uuid::Uuid::now_v7()));
    let q = Principal::User(UserId::new(uuid::Uuid::now_v7()));
    let g1 = GroupId::new(uuid::Uuid::now_v7());

    seed_membership(&pg, g1, &p, Relation::Viewer).await;
    seed_membership(&pg, g1, &q, Relation::Viewer).await;
    let f1 = seed_memory_owned(&pg, Principal::Group(g1)).await;
    let a = seed_memory_owned(&pg, p.clone()).await;
    let s_p = read_owners(&pg, &p).await;
    let s_q = read_owners(&pg, &q).await;

    assert!(pg.entity_is_readable(f1, &s_p).await.unwrap());
    assert!(pg.entity_is_readable(a, &s_p).await.unwrap());
    assert!(pg.entity_is_readable(f1, &s_q).await.unwrap());
    assert!(
        !pg.entity_is_readable(a, &s_q).await.unwrap(),
        "A is personal to P"
    );

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn entity_owner_share_verbs_manage_reachability_and_refuse_home() {
    let (pg, db) = common::fresh_pg().await;
    let home = Principal::User(UserId::new(uuid::Uuid::now_v7()));
    let shared = Principal::User(UserId::new(uuid::Uuid::now_v7()));
    let entity = seed_memory_owned(&pg, home.clone()).await;

    assert!(
        !pg.entity_is_readable(entity, std::slice::from_ref(&shared))
            .await
            .unwrap()
    );

    pg.add_entity_owner_share(entity, &shared, Some(Uuid::now_v7()))
        .await
        .unwrap();
    assert!(
        pg.entity_is_readable(entity, std::slice::from_ref(&shared))
            .await
            .unwrap(),
        "share row makes entity readable to shared principal"
    );
    let owners = pg.list_entity_owners(entity).await.unwrap();
    assert_eq!(owners.len(), 2);
    assert!(owners.iter().any(|row| row.owner == home && row.is_home));
    assert!(owners.iter().any(|row| row.owner == shared && !row.is_home));

    assert_eq!(
        pg.remove_entity_owner_share(entity, &shared).await.unwrap(),
        RemoveOwnerOutcome::Removed
    );
    assert!(
        !pg.entity_is_readable(entity, std::slice::from_ref(&shared))
            .await
            .unwrap()
    );
    assert_eq!(
        pg.remove_entity_owner_share(entity, &home).await.unwrap(),
        RemoveOwnerOutcome::RefusedLastOwner
    );
    let owners = pg.list_entity_owners(entity).await.unwrap();
    assert_eq!(owners.len(), 1);
    assert!(owners.iter().any(|row| row.owner == home && row.is_home));

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn publish_entry_adds_world_row_and_world_listing_tracks_tombstone() {
    let (pg, db) = common::fresh_pg().await;
    let owner = Principal::User(UserId::new(uuid::Uuid::now_v7()));
    let entity = EntityId::Memory(
        seed_abstraction_memory(&pg, &owner, owner.clone(), "worldlisted abstraction").await,
    );
    let engine = Engine::new(FlavorRegistry::new().freeze()).with_storage(Arc::new(pg.clone()));

    engine
        .publish_entry(
            &AuthzContext::single_owner(&owner, AuthPath::System),
            entity,
        )
        .await
        .unwrap();

    assert!(
        pg.entity_is_readable(entity, &[proxima_core::access::world()])
            .await
            .unwrap(),
        "World-only read set reaches the published entity"
    );
    let world_entities = engine
        .list_world_entities(&AuthzContext::single_owner(&owner, AuthPath::System), 10)
        .await
        .unwrap();
    assert_eq!(
        world_entities
            .iter()
            .map(|snapshot| snapshot.memory_id)
            .collect::<Vec<_>>(),
        vec![MemoryId::new(entity.uuid())]
    );

    sqlx::query(
        "UPDATE proxima_core.memories
            SET tombstoned_at = now()
          WHERE memory_id = $1",
    )
    .bind(entity.uuid())
    .execute(pg.pool())
    .await
    .unwrap();

    assert!(
        !pg.entity_is_readable(entity, &[proxima_core::access::world()])
            .await
            .unwrap(),
        "tombstone trigger removes World reachability"
    );
    let world_entities = engine
        .list_world_entities(&AuthzContext::single_owner(&owner, AuthPath::System), 10)
        .await
        .unwrap();
    assert!(world_entities.is_empty());

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
async fn group_membership_verbs_round_trip_and_engine_gates_admin_editor() {
    let (pg, db) = common::fresh_pg().await;
    let admin = Principal::User(UserId::new(uuid::Uuid::now_v7()));
    let viewer = Principal::User(UserId::new(uuid::Uuid::now_v7()));
    let outsider = Principal::User(UserId::new(uuid::Uuid::now_v7()));
    let group = GroupId::new(uuid::Uuid::now_v7());
    let Principal::User(admin_id) = admin.clone() else {
        unreachable!("admin is user")
    };
    let Principal::User(viewer_id) = viewer.clone() else {
        unreachable!("viewer is user")
    };
    let Principal::User(outsider_id) = outsider.clone() else {
        unreachable!("outsider is user")
    };

    pg.add_group_member(group, viewer_id, Relation::Viewer, Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(
        pg.list_group_members(group).await.unwrap(),
        vec![(viewer_id, Relation::Viewer)]
    );

    let group_entity = seed_memory_owned(&pg, Principal::Group(group)).await;
    let viewer_read_owners = read_owners(&pg, &viewer).await;
    assert!(
        pg.entity_is_readable(group_entity, &viewer_read_owners)
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
        .add_member(&granted_authz(&admin), group, outsider_id, Relation::Viewer)
        .await
        .unwrap();
    assert!(
        engine
            .list_members(&granted_authz(&admin), group)
            .await
            .unwrap()
            .contains(&(outsider_id, Relation::Viewer))
    );
    engine
        .remove_member(&granted_authz(&admin), group, outsider_id)
        .await
        .unwrap();
    assert!(
        !engine
            .list_members(&granted_authz(&admin), group)
            .await
            .unwrap()
            .iter()
            .any(|(member, _)| *member == outsider_id)
    );

    let share_err = engine
        .share_entry(&granted_authz(&viewer), group_entity, outsider.clone())
        .await
        .expect_err("viewer membership must not authorize share_entry");
    assert_eq!(share_err.code, ErrorCode::Forbidden);

    common::drop_db(&db).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn discovery_reads_filter_by_read_owners_not_legacy_memory_owner() {
    let (pg, db) = common::fresh_pg().await;
    let p = Principal::User(UserId::new(uuid::Uuid::now_v7()));
    let q = Principal::User(UserId::new(uuid::Uuid::now_v7()));
    let g1 = GroupId::new(uuid::Uuid::now_v7());

    seed_membership(&pg, g1, &q, Relation::Viewer).await;

    let mut f1_draft = fresh_event_draft(Principal::Group(g1));
    f1_draft.rendered_text = Some("boundaryneedle group fact".to_string());
    let f1 = pg
        .ingest_event_atomic(&f1_draft, None)
        .await
        .unwrap()
        .memory_id;
    let mut hidden_target_draft = fresh_event_draft(Principal::Group(g1));
    hidden_target_draft.rendered_text = Some("unreadable edge target".to_string());
    let hidden_target = pg
        .ingest_event_atomic(&hidden_target_draft, None)
        .await
        .unwrap()
        .memory_id;
    move_entity_owner_home(&pg, hidden_target, &p).await;
    let a = seed_abstraction_memory(
        &pg,
        &p,
        Principal::Group(g1),
        "boundaryneedle personal abstraction",
    )
    .await;
    let leaky_edge = seed_edge_between_memories(&pg, Principal::Group(g1), f1, hidden_target).await;
    let q_read_owners = read_owners(&pg, &q).await;

    let query = pg
        .query_memories(
            &QueryRequest {
                principal: q.clone(),
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
                principal: q.clone(),
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
    let owner = Principal::User(UserId::new(Uuid::now_v7()));

    let outcome = pg
        .ingest_event_atomic(&fresh_event_draft(owner.clone()), None)
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
    let owner = Principal::User(UserId::new(Uuid::now_v7()));
    let self_id = insert_self(&pg, &owner).await;
    let registry = FlavorRegistry::new().freeze();
    let draft = fresh_goal_draft(owner.clone());

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
    member: &Principal,
    relation: Relation,
) {
    let Principal::User(user) = member else {
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

async fn seed_memory_owned(pg: &proxima_storage_pg::PgStorage, owner: Principal) -> EntityId {
    let entity_id = uuid::Uuid::now_v7();
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.entity_owner
            (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
         VALUES ($1,$2::proxima_core.owner_principal_kind,$3,true,$4)",
    )
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
    owner: &Principal,
    legacy_owner: Principal,
    text: &str,
) -> MemoryId {
    let memory_id = uuid::Uuid::now_v7();
    let (legacy_owner_kind, legacy_owner_id) = legacy_owner.columns();
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id)
         VALUES ($1, $2, $3, 'test/entity-owner-abstraction-v1', 1,
                 'Abstraction', $4, 'FtoA', 'test-model', 'v1', $5)",
    )
    .bind(memory_id)
    .bind(legacy_owner_kind)
    .bind(legacy_owner_id)
    .bind(text)
    .bind(uuid::Uuid::nil())
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO proxima_core.entity_owner
            (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
         VALUES ($1,$2::proxima_core.owner_principal_kind,$3,true,$4)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(uuid::Uuid::nil())
    .execute(pg.pool())
    .await
    .unwrap();
    MemoryId::new(memory_id)
}

async fn move_entity_owner_home(
    pg: &proxima_storage_pg::PgStorage,
    memory_id: MemoryId,
    owner: &Principal,
) {
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "UPDATE proxima_core.entity_owner
            SET owner_principal_kind = $2::proxima_core.owner_principal_kind,
                owner_principal_id = $3
          WHERE entity_id = $1 AND is_home",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool())
    .await
    .unwrap();
}

async fn seed_edge_between_memories(
    pg: &proxima_storage_pg::PgStorage,
    owner: Principal,
    source: MemoryId,
    target: MemoryId,
) -> uuid::Uuid {
    let edge_id = uuid::Uuid::now_v7();
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.edges
           (edge_id, relation, relation_class,
            source_kind, source_memory_id, source_goal_id,
            target_kind, target_memory_id, target_goal_id,
            authorship_kind, authorship_owner_memory_id,
            owner_principal_kind, owner_principal_id)
         VALUES
           ($1, 'test/leaky-edge', 'Structural',
            'Fact', $2, NULL,
            'Fact', $3, NULL,
            'EventSource', NULL,
            $4, $5)",
    )
    .bind(edge_id)
    .bind(source.into_inner())
    .bind(target.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool())
    .await
    .unwrap();
    edge_id
}

async fn read_owners(pg: &proxima_storage_pg::PgStorage, principal: &Principal) -> Vec<Principal> {
    let mut owners = vec![principal.clone()];
    owners.extend(
        pg.resolve_membership(principal)
            .await
            .unwrap()
            .into_iter()
            .map(|membership| Principal::Group(membership.group)),
    );
    owners.push(proxima_core::access::world());
    owners
}

fn granted_authz(principal: &Principal) -> AuthzContext {
    AuthzContext {
        identity: Identity {
            principal: principal.clone(),
            accessible_principals: HashSet::from([principal.clone()]),
            expires_at: None,
            auth_epoch: 0,
        },
        capabilities: CapabilitySet {
            tool_scope: ToolScope::All,
            access: AccessScope::Granted,
        },
        auth_path: AuthPath::HostBearer,
    }
}

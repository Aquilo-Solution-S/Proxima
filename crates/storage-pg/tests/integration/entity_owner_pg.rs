use crate::common;
use proxima_core::verbs::event_ingest::EventDraft;
use proxima_core::verbs::goal_write::{
    CreateGoalAtomicRequest, GoalAtomicContext, GoalAuthorship, GoalDraft, GoalState,
};
use proxima_core::{
    EntityId, EntityKind, FlavorRegistry, GroupId, MemoryId, MemoryOperatorKind, Owner,
    OwnerPrincipalKind, Principal, Relation, SchemaId, SchemaVersion, SourceBatchId, SourceId,
    Storage, UserId,
};
use proxima_storage_pg::PgStorage;
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

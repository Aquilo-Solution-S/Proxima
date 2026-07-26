use crate::common;
use proxima_core::FactReceiptDraft;
use proxima_core::storage_ports::*;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::{
    GoalAssignmentTarget, GoalAuthorship, GoalDraft, GoalState, GoalTopologyWrite,
};
use proxima_core::{
    AuthPath, AuthzContext, EntityId, EntityKind, ErrorCode, GroupId, MemoryId, MemoryOperatorKind,
    Owner, OwnerRef, OwnerRefKind, Relation, Role, SchemaId, SchemaVersion, SourceBatchId,
    SourceId, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

type ResolvedAuthz = AuthzContext;

async fn owner_write_permit(owner: &Owner, kind: proxima_core::AccessKind) -> OwnerWritePermit {
    common::owner_write_permit(owner, kind)
        .await
        .expect("test write permit")
}

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

fn system_authz() -> ResolvedAuthz {
    AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::System)
}

fn authz_with_role(principal: &OwnerRef, owner: OwnerRef, role: Role) -> ResolvedAuthz {
    let OwnerRef::Personal(user) = *principal else {
        panic!("test principal must be a user");
    };
    AuthzContext::for_subject_with_role(user, [(owner, role)], AuthPath::HostBearer)
}

fn assert_bootstrap_conflict(err: &proxima_core::error::ProtocolError) {
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(
        err.message.contains("group already has an Admin"),
        "unexpected bootstrap conflict message: {}",
        err.message
    );
}

async fn admin_members(pg: &PgStorage, group: GroupId) -> Vec<UserId> {
    let rows: Vec<Uuid> = sqlx::query_scalar(
        "SELECT member_user_id
           FROM proxima_core.group_memberships
          WHERE group_id = $1
            AND relation = $2
          ORDER BY member_user_id",
    )
    .bind(group.into_inner())
    .bind(Relation::Admin)
    .fetch_all(pg.pool_for_tests())
    .await
    .expect("read admin members");

    rows.into_iter().map(UserId::new).collect()
}

async fn admin_member_count(pg: &PgStorage, group: GroupId) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.group_memberships
          WHERE group_id = $1
            AND relation = $2",
    )
    .bind(group.into_inner())
    .bind(Relation::Admin)
    .fetch_one(pg.pool_for_tests())
    .await
    .expect("count admin members")
}

mod bootstrap;
mod columns;
mod membership;
mod read_sets;
mod world;

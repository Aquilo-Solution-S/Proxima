use crate::common::{drop_db, fresh_pg, owner_fixture};

use proxima_core::relation::CORE_INSPIRES_RELATION;
use proxima_core::storage_ports::*;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{
    EdgeAuthorshipKind, FlavorRegistry, GoalId, GroupId, MemoryId, Owner, OwnerRef, OwnerRefKind,
    Relation, UserId,
};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::edge_write::{CheckedEdgeEndpoint, append_owner_checked_edge};
use uuid::Uuid;

fn owner_parts(owner: &Owner) -> (OwnerRefKind, Option<Uuid>) {
    owner.columns()
}

fn other_owner() -> Owner {
    OwnerRef::Personal(UserId::new(Uuid::now_v7()))
}

async fn insert_self(
    pg: &PgStorage,
    owner: &Owner,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'test/self', 1, $4,
                 'self', $5, '00000000-0000-0000-0000-000000000361'::uuid,
                 '00000000-0000-0000-0000-000000000362'::uuid, NULL,
                 'test-model', 'v1')"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(proxima_core::EntityKind::Perspective)
    .bind(proxima_core::MemoryOperatorKind::AtoP)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(MemoryId::new(memory_id))
}

async fn insert_goal(
    pg: &PgStorage,
    owner: &Owner,
    state: GoalState,
    supersedes: Option<GoalId>,
    request_id: &str,
) -> Result<GoalId, Box<dyn std::error::Error>> {
    let (owner_kind, owner_id) = owner_parts(owner);
    let goal_id = GoalId::new(Uuid::now_v7());
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, owner_kind, owner_id, schema_id, schema_version,
             title, text, payload, state, supersedes,
             authorship_kind, request_id, idempotency_key)
         VALUES ($1, $2, $3, 'core/simple-text-v1', 1,
                 $4, $4, convert_to('{}', 'UTF8'), $5, $6,
                 'User', $7,
                 md5($2::text || ':' || $3::text || ':' || $7))",
    )
    .bind(goal_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(request_id)
    .bind(state)
    .bind(supersedes.map(GoalId::into_inner))
    .bind(request_id)
    .execute(pg.pool_for_tests())
    .await?;
    if state == GoalState::Active {
        insert_goal_activated_fact(pg, owner, goal_id).await?;
    }
    Ok(goal_id)
}

async fn link_goal_to_self(
    pg: &PgStorage,
    owner: &Owner,
    goal_id: GoalId,
    self_id: MemoryId,
) -> Result<(), Box<dyn std::error::Error>> {
    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let relation = registry
        .resolve_relation(CORE_INSPIRES_RELATION)
        .expect("core/inspires relation");
    let mut tx = pg.pool_for_tests().begin().await?;
    append_owner_checked_edge(
        &mut tx,
        owner,
        proxima_core::EdgeId::new(Uuid::now_v7()),
        relation,
        CheckedEdgeEndpoint::goal(goal_id),
        CheckedEdgeEndpoint::perspective(self_id),
        EdgeAuthorshipKind::PerspectiveGoalLink,
        Some(self_id),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[tokio::test]
async fn list_active_goals_follows_inspires_and_goal_supersession()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let other = other_owner();
        let self_a = insert_self(&pg, &owner).await?;
        let self_b = insert_self(&pg, &other).await?;

        let base_a = insert_goal(&pg, &owner, GoalState::Active, None, "a-base").await?;
        link_goal_to_self(&pg, &owner, base_a, self_a).await?;
        let active_a =
            insert_goal(&pg, &owner, GoalState::Active, Some(base_a), "a-active").await?;

        let pending_base =
            insert_goal(&pg, &owner, GoalState::Active, None, "a-pending-base").await?;
        link_goal_to_self(&pg, &owner, pending_base, self_a).await?;
        let paused_pending = insert_goal(
            &pg,
            &owner,
            GoalState::Paused,
            Some(pending_base),
            "a-pending",
        )
        .await?;
        link_goal_to_self(&pg, &owner, paused_pending, self_a).await?;

        let unconnected = insert_goal(&pg, &owner, GoalState::Active, None, "a-unlinked").await?;
        let _ = unconnected;

        let paused_base = insert_goal(&pg, &owner, GoalState::Active, None, "a-pause-base").await?;
        link_goal_to_self(&pg, &owner, paused_base, self_a).await?;
        let _paused = insert_goal(
            &pg,
            &owner,
            GoalState::Paused,
            Some(paused_base),
            "a-paused",
        )
        .await?;

        let active_b = insert_goal(&pg, &other, GoalState::Active, None, "b-active").await?;
        link_goal_to_self(&pg, &other, active_b, self_b).await?;

        let goals_a = pg
            .list_active_goals(std::slice::from_ref(&owner), self_a, 100)
            .await?;
        assert_eq!(goals_a.len(), 1);
        assert_eq!(goals_a[0].goal_id, active_a);
        assert_eq!(goals_a[0].title, "a-active");
        assert!(goals_a[0].goal_activated_memory_id.is_some());

        let goals_b = pg
            .list_active_goals(std::slice::from_ref(&other), self_b, 100)
            .await?;
        assert_eq!(goals_b.len(), 1);
        assert_eq!(goals_b[0].goal_id, active_b);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

async fn insert_dummy_fact_refs(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: &Owner,
    memory_id: Uuid,
) -> Result<(Vec<u8>, Uuid), Box<dyn std::error::Error>> {
    let (owner_kind, owner_id) = owner_parts(owner);
    let receipt_id = Uuid::now_v7().as_bytes().to_vec();
    let source_id = "proxima-test/source";
    let source_batch_id = Uuid::now_v7();
    let now = time::OffsetDateTime::now_utc();
    sqlx::query(
        "INSERT INTO proxima_core.source_batches
            (id, owner_kind, owner_id,
             source_id)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(source_batch_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(source_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.fact_receipts
            (receipt_id, owner_kind, owner_id,
             source_batch_id, source, schema_id, schema_version,
             observed_at, occurred_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(&receipt_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(source_batch_id)
    .bind(source_id)
    .bind("core/goal-activated-v1")
    .bind(1_i32)
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    let citation_mapping_id = Uuid::now_v7();
    let cited_object_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.cited_objects
            (cited_object_id, schema_id, owner_kind,
             owner_id, content_hash)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(cited_object_id)
    .bind("proxima-test/cited-object-v1")
    .bind(owner_kind)
    .bind(owner_id)
    .bind(Uuid::now_v7().as_bytes().repeat(2))
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.citation_mappings
            (citation_mapping_id, schema_id, memory_id, cited_object_id,
             owner_kind, owner_id)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(citation_mapping_id)
    .bind("proxima-test/citation-mapping-v1")
    .bind(memory_id)
    .bind(cited_object_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(&mut **tx)
    .await?;
    Ok((receipt_id, citation_mapping_id))
}

async fn insert_goal_activated_fact(
    pg: &PgStorage,
    owner: &Owner,
    goal_id: GoalId,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let mut tx = pg.pool_for_tests().begin().await?;

    // Minimal Fact-shaped memory row (requires receipt_id + citation_mapping_id
    // per the variant check). Insert dummy receipt/citation rows just so the
    // FK + CHECK constraints accept the row.
    let (owner_kind, owner_id) = owner_parts(owner);
    let (receipt_id, citation_mapping_id) =
        insert_dummy_fact_refs(&mut tx, owner, memory_id).await?;
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, receipt_id,
             citation_mapping_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind("core/goal-activated-v1")
    .bind(1_i32)
    .bind(&receipt_id)
    .bind(citation_mapping_id)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO proxima_core.goal_activated_v1
            (memory_id, goal_id, transitioned_at)
         VALUES ($1, $2, $3)",
    )
    .bind(memory_id)
    .bind(goal_id.into_inner())
    .bind(time::OffsetDateTime::now_utc())
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(MemoryId::new(memory_id))
}

#[tokio::test]
async fn list_active_goals_surfaces_goal_activated_memory_when_present()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        pg.run_migrations().await?;

        let owner = owner_fixture();
        let self_a = insert_self(&pg, &owner).await?;
        let active = insert_goal(&pg, &owner, GoalState::Active, None, "p-active").await?;
        link_goal_to_self(&pg, &owner, active, self_a).await?;

        let goals = pg
            .list_active_goals(std::slice::from_ref(&owner), self_a, 100)
            .await?;
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].goal_id, active);
        assert!(goals[0].goal_activated_memory_id.is_some());
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn list_active_goals_filters_by_read_owners() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        pg.run_migrations().await?;
        let p = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let q = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let g1 = OwnerRef::Group(GroupId::new(Uuid::now_v7()));

        seed_membership(&pg, &g1, &q, Relation::Viewer).await?;

        let self_g1 = insert_self(&pg, &g1).await?;
        let group_goal = insert_goal(&pg, &g1, GoalState::Active, None, "g1-active").await?;
        link_goal_to_self(&pg, &g1, group_goal, self_g1).await?;

        let self_p = insert_self(&pg, &p).await?;
        let p_goal = insert_goal(&pg, &p, GoalState::Active, None, "p-active").await?;
        link_goal_to_self(&pg, &p, p_goal, self_p).await?;

        let q_read_owners = vec![q, g1];
        let group_goals = pg.list_active_goals(&q_read_owners, self_g1, 100).await?;
        assert_eq!(group_goals.len(), 1);
        assert_eq!(group_goals[0].goal_id, group_goal);

        let p_goals = pg.list_active_goals(&q_read_owners, self_p, 100).await?;
        assert!(p_goals.is_empty(), "Q must not see P's personal goal");
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn list_active_goals_rejects_cross_owner_inspires_edges()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        pg.run_migrations().await?;
        let source_owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let target_owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let private_self = insert_self(&pg, &target_owner).await?;
        let goal = insert_goal(
            &pg,
            &source_owner,
            GoalState::Active,
            None,
            "private-active",
        )
        .await?;
        let err = link_goal_to_self(&pg, &source_owner, goal, private_self)
            .await
            .expect_err("core/inspires is SameOwner in PR4");
        assert!(
            err.to_string().contains("same owner")
                || err.to_string().contains("same Owner")
                || err.to_string().contains("same-owner"),
            "unexpected error: {err}"
        );

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

async fn seed_membership(
    pg: &PgStorage,
    group: &OwnerRef,
    member: &OwnerRef,
    relation: Relation,
) -> Result<(), Box<dyn std::error::Error>> {
    let OwnerRef::Group(group_id) = group else {
        panic!("seed_membership group must be a group principal");
    };
    let OwnerRef::Personal(member_id) = member else {
        panic!("seed_membership member must be a user principal");
    };
    sqlx::query(
        "INSERT INTO proxima_core.group_memberships
            (group_id, member_user_id, relation)
         VALUES ($1, $2, $3::proxima_core.membership_relation)",
    )
    .bind(group_id.into_inner())
    .bind(member_id.into_inner())
    .bind(relation)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(())
}

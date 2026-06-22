use crate::common::{drop_db, fresh_pg, owner_fixture};

use proxima_core::relation::CORE_INSPIRES_RELATION;
use proxima_core::storage::Storage;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{
    EdgeAuthorshipKind, EntityKind, FlavorRegistry, GoalId, MemoryId, Owner, OwnerPrincipalKind,
    Principal, UserId,
};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use uuid::Uuid;

fn owner_parts(owner: &Owner) -> (OwnerPrincipalKind, Uuid) {
    owner.columns()
}

fn other_owner() -> Owner {
    Principal::User(UserId::new(Uuid::now_v7()))
}

async fn insert_self(
    pg: &PgStorage,
    owner: &Owner,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let (owner_kind, owner_principal_id) = owner_parts(owner);
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
    .bind(proxima_core::EntityKind::Perspective)
    .bind(proxima_core::MemoryOperatorKind::AtoP)
    .bind(Uuid::nil())
    .execute(pg.pool())
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
    let (owner_kind, owner_principal_id) = owner_parts(owner);
    let goal_id = GoalId::new(Uuid::now_v7());
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, schema_id, schema_version,
             owner_principal_kind, owner_principal_id,
             title, text, payload, state, supersedes,
             authorship_kind, request_id)
         VALUES ($1, 'core/simple-text-v1', 1,
                 $2, $3,
                 $4, $4, convert_to('{}', 'UTF8'), $5, $6,
                 'User', $7)",
    )
    .bind(goal_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(request_id)
    .bind(state)
    .bind(supersedes.map(GoalId::into_inner))
    .bind(request_id)
    .execute(pg.pool())
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
    let registry = FlavorRegistry::new().freeze();
    let relation = registry
        .resolve_relation(CORE_INSPIRES_RELATION)
        .expect("core/inspires relation");
    let mut tx = pg.pool().begin().await?;
    append_edge_in_tx(
        &mut tx,
        &EdgeDraft {
            edge_id: Uuid::now_v7(),
            relation,
            source_kind: EntityKind::Goal,
            source_memory_id: None,
            source_goal_id: Some(goal_id.into_inner()),
            source_fact_entity_id: None,
            target_kind: EntityKind::Perspective,
            target_memory_id: Some(self_id.into_inner()),
            target_goal_id: None,
            target_fact_entity_id: None,
            authorship_kind: EdgeAuthorshipKind::ExternalAgent,
            authorship_owner_memory_id: Some(self_id.into_inner()),
            owner,
        },
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

        let paused_pending = insert_goal(&pg, &owner, GoalState::Paused, None, "a-pending").await?;
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

        let goals_a = pg.list_active_goals(&owner, self_a, 100).await?;
        assert_eq!(goals_a.len(), 1);
        assert_eq!(goals_a[0].goal_id, active_a);
        assert_eq!(goals_a[0].title, "a-active");
        assert!(goals_a[0].goal_activated_memory_id.is_some());

        let goals_b = pg.list_active_goals(&other, self_b, 100).await?;
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
    let (owner_kind, owner_principal_id) = owner_parts(owner);
    let event_id = Uuid::now_v7().as_bytes().to_vec();
    let source_id = "proxima-test/source";
    let source_batch_id = Uuid::now_v7();
    let now = time::OffsetDateTime::now_utc();
    sqlx::query(
        "INSERT INTO proxima_core.source_batches
            (id, owner_principal_kind, owner_principal_id,
             source_id)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(source_batch_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(source_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.events
            (event_id, owner_principal_kind, owner_principal_id,
             source_batch_id, source_id, schema_id, schema_version,
             observed_at, occurred_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(&event_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
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
            (cited_object_id, schema_id, owner_principal_kind,
             owner_principal_id, content_hash)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(cited_object_id)
    .bind("proxima-test/cited-object-v1")
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(Uuid::now_v7().as_bytes().repeat(2))
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.citation_mappings
            (citation_mapping_id, schema_id, memory_id, cited_object_id,
             owner_principal_kind, owner_principal_id)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(citation_mapping_id)
    .bind("proxima-test/citation-mapping-v1")
    .bind(memory_id)
    .bind(cited_object_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .execute(&mut **tx)
    .await?;
    Ok((event_id, citation_mapping_id))
}

async fn insert_goal_activated_fact(
    pg: &PgStorage,
    owner: &Owner,
    goal_id: GoalId,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let (owner_kind, owner_principal_id) = owner_parts(owner);
    let memory_id = Uuid::now_v7();
    let mut tx = pg.pool().begin().await?;

    // Minimal Fact-shaped memory row (requires event_id + citation_mapping_id
    // per the variant check). Insert dummy events/citation rows just so the
    // FK + CHECK constraints accept the row.
    let (event_id, citation_mapping_id) = insert_dummy_fact_refs(&mut tx, owner, memory_id).await?;
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id,
             schema_id, schema_version, event_id, citation_mapping_id,
             personality_instance_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7,
                 '00000000-0000-0000-0000-000000000000'::uuid)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind("core/goal-activated-v1")
    .bind(1_i32)
    .bind(&event_id)
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

        let goals = pg.list_active_goals(&owner, self_a, 100).await?;
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].goal_id, active);
        assert!(goals[0].goal_activated_memory_id.is_some());
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

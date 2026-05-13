mod common;

use common::{drop_db, fresh_pg, owner_fixture};

use proxima_core::relation::CORE_INSPIRES_RELATION;
use proxima_core::storage::Storage;
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, GoalState};
use proxima_core::{
    FlavorRegistry, GoalId, MemoryId, OrgId, Owner, Principal, SchemaId, SchemaVersion, UserId,
};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use sqlx::Executor;
use uuid::Uuid;

fn owner_parts(owner: &Owner) -> (&'static str, Uuid, Uuid) {
    let (kind, principal_id) = match owner.principal {
        Principal::User(user) => ("User", user.into_inner()),
        Principal::Group(group) => ("Group", group.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

fn other_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::nil()),
    }
}

fn draft(
    owner: &Owner,
    state: GoalState,
    supersedes: Option<GoalId>,
    request_id: &str,
) -> GoalDraft {
    GoalDraft {
        owner: owner.clone(),
        schema_id: SchemaId::new("test/goal".into()),
        schema_version: SchemaVersion::new(1),
        title: request_id.into(),
        text: request_id.into(),
        payload: request_id.as_bytes().to_vec(),
        state,
        parent_goal_ids: Vec::new(),
        supersedes_goal_id: supersedes,
        authorship: match state {
            GoalState::Proposed => GoalAuthorship::External,
            _ => GoalAuthorship::User,
        },
        request_id: request_id.into(),
    }
}

async fn insert_self(
    pg: &PgStorage,
    owner: &Owner,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_parts(owner);
    let memory_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_id)
         VALUES ($1, $2, $3, $4, 'test/self', 1, 'Perspective',
                 'self', 'AtoP', 'test-model', 'v1', 'test/personality')",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(pg.pool())
    .await?;
    Ok(MemoryId::new(memory_id))
}

async fn write_goal(
    pg: &PgStorage,
    owner: &Owner,
    state: GoalState,
    supersedes: Option<GoalId>,
    request_id: &str,
) -> Result<GoalId, Box<dyn std::error::Error>> {
    let outcome = if let Some(prior) = supersedes {
        pg.supersede_goal_atomic(prior, &draft(owner, state, Some(prior), request_id))
            .await?
    } else {
        pg.write_goal_atomic(&draft(owner, state, None, request_id))
            .await?
    };
    Ok(outcome.goal_id)
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
            source_kind: "Goal",
            source_memory_id: None,
            source_goal_id: Some(goal_id.into_inner()),
            target_kind: "Perspective",
            target_memory_id: Some(self_id.into_inner()),
            target_goal_id: None,
            authorship_kind: "ExternalAgent",
            authorship_owner_memory_id: Some(self_id.into_inner()),
            owner,
        },
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[tokio::test]
async fn list_active_goals_follows_inspires_and_goal_supersession()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };

    let result = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let other = other_owner();
        let self_a = insert_self(&pg, &owner).await?;
        let self_b = insert_self(&pg, &other).await?;

        let proposed_a = write_goal(&pg, &owner, GoalState::Proposed, None, "a-proposed").await?;
        link_goal_to_self(&pg, &owner, proposed_a, self_a).await?;
        let active_a =
            write_goal(&pg, &owner, GoalState::Active, Some(proposed_a), "a-active").await?;

        let still_proposed =
            write_goal(&pg, &owner, GoalState::Proposed, None, "a-pending").await?;
        link_goal_to_self(&pg, &owner, still_proposed, self_a).await?;

        let unconnected = write_goal(&pg, &owner, GoalState::Active, None, "a-unlinked").await?;
        let _ = unconnected;

        let paused_base = write_goal(&pg, &owner, GoalState::Active, None, "a-pause-base").await?;
        link_goal_to_self(&pg, &owner, paused_base, self_a).await?;
        let _paused = write_goal(
            &pg,
            &owner,
            GoalState::Paused,
            Some(paused_base),
            "a-paused",
        )
        .await?;

        let active_b = write_goal(&pg, &other, GoalState::Active, None, "b-active").await?;
        link_goal_to_self(&pg, &other, active_b, self_b).await?;

        let goals_a = pg.list_active_goals(&owner, self_a, 100).await?;
        assert_eq!(goals_a.len(), 1);
        assert_eq!(goals_a[0].goal_id, active_a);
        assert_eq!(goals_a[0].title, "a-active");
        // No goal-activated sidecar exists in this fixture (the goal flavor
        // wasn't migrated) — the substrate query degrades to None.
        assert!(goals_a[0].goal_activated_memory_id.is_none());

        let goals_b = pg.list_active_goals(&other, self_b, 100).await?;
        assert_eq!(goals_b.len(), 1);
        assert_eq!(goals_b[0].goal_id, active_b);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

async fn apply_goal_activated_sidecar(pool: &sqlx::PgPool) -> sqlx::Result<()> {
    pool.execute(
        "CREATE SCHEMA IF NOT EXISTS proxima_goal; \
         CREATE TABLE IF NOT EXISTS proxima_goal.goal_activated_v1 ( \
             memory_id      uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id), \
             goal_id        uuid NOT NULL REFERENCES proxima_core.goals(goal_id), \
             schema_id      text NOT NULL, \
             title          text NOT NULL, \
             accepted_at    timestamptz NOT NULL, \
             evidence_count integer NOT NULL \
         );",
    )
    .await
    .map(|_| ())
}

async fn insert_goal_activated_fact(
    pg: &PgStorage,
    owner: &Owner,
    goal_id: GoalId,
    title: &str,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_parts(owner);
    let memory_id = Uuid::now_v7();
    let mut tx = pg.pool().begin().await?;

    // Minimal Fact-shaped memory row (requires event_id + citation_mapping_id
    // per the variant check). Insert dummy events/citation rows just so the
    // FK + CHECK constraints accept the row.
    let event_id = Uuid::now_v7().as_bytes().to_vec();
    let source_id = Uuid::now_v7();
    let source_batch_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.source_batches
            (source_batch_id, owner_principal_kind, owner_principal_id, owner_org_id,
             source_id, source_id_text, f2a_invocation_key)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(source_batch_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(source_id)
    .bind("proxima-test/source")
    .bind(Uuid::now_v7().to_string())
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.events
            (event_id, owner_principal_kind, owner_principal_id, owner_org_id,
             source_batch_id, source_id, source_id_text, payload, schema_id,
             schema_version, wake_chain_depth)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(&event_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(source_batch_id)
    .bind(source_id)
    .bind("proxima-test/source")
    .bind(b"{}".as_slice())
    .bind("proxima-goal/goal-activated-v1")
    .bind(1_i32)
    .bind(0_i32)
    .execute(&mut *tx)
    .await?;
    let citation_mapping_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.citation_mappings
            (citation_mapping_id, owner_principal_kind, owner_principal_id,
             owner_org_id, content_sha256)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(citation_mapping_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(vec![0u8; 32])
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, event_id, citation_mapping_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind("proxima-goal/goal-activated-v1")
    .bind(1_i32)
    .bind(&event_id)
    .bind(citation_mapping_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE proxima_core.citation_mappings
            SET memory_id = $1
          WHERE citation_mapping_id = $2",
    )
    .bind(memory_id)
    .bind(citation_mapping_id)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO proxima_goal.goal_activated_v1
            (memory_id, goal_id, schema_id, title, accepted_at, evidence_count)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(memory_id)
    .bind(goal_id.into_inner())
    .bind("test/goal")
    .bind(title)
    .bind(time::OffsetDateTime::now_utc())
    .bind(0_i32)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(MemoryId::new(memory_id))
}

#[tokio::test]
async fn list_active_goals_surfaces_goal_activated_memory_when_present()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };

    let result = async {
        pg.run_migrations().await?;
        apply_goal_activated_sidecar(pg.pool()).await?;

        let owner = owner_fixture();
        let self_a = insert_self(&pg, &owner).await?;
        let proposed = write_goal(&pg, &owner, GoalState::Proposed, None, "p-proposed").await?;
        link_goal_to_self(&pg, &owner, proposed, self_a).await?;
        let active = write_goal(&pg, &owner, GoalState::Active, Some(proposed), "p-active").await?;
        let activated_memory =
            insert_goal_activated_fact(&pg, &owner, active, "p-active").await?;

        let goals = pg.list_active_goals(&owner, self_a, 100).await?;
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].goal_id, active);
        assert_eq!(goals[0].goal_activated_memory_id, Some(activated_memory));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

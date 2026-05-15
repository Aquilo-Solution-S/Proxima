mod common;

use common::{ctx, drop_db, insert_abstraction, insert_self_perspective, migrated, owner_fixture};
use proxima_core::mcp::McpTool;
use proxima_core::storage::Storage;
use proxima_core::verbs::query::QueryRequest;
use proxima_core::{EntityRef, GoalId, MemoryId};
use proxima_flavor_goal::tools::accept::{AcceptArgs, AcceptTool};
use proxima_flavor_goal::tools::decline::{DeclineArgs, DeclineTool};
use proxima_flavor_goal::tools::mark_achieved::{
    MarkAchievedArgs, MarkAchievedStatus, MarkAchievedTool,
};
use proxima_flavor_goal::tools::modify::{ModifyArgs, ModifyTool};
use proxima_flavor_goal::tools::propose::{ProposeArgs, ProposeTool};
use proxima_flavor_goal::tools::util::{GoalPayloadInput, SimpleTextGoalBody};

async fn propose_with_evidence(
    pg: &proxima_storage_pg::PgStorage,
    ctx: &proxima_core::McpToolCtx,
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let evidence = insert_abstraction(pg, &ctx.owner).await?;
    let evidence_handle = ctx.format_memory(proxima_core::MemoryId::new(evidence));
    let outcome = ProposeTool::call(
        ctx.clone(),
        ProposeArgs {
            payload: GoalPayloadInput::SimpleText(SimpleTextGoalBody {
                title: "proposal".into(),
                text: "proposal".into(),
            }),
            evidence: vec![evidence_handle.as_str().to_string()],
            target_personality: None,
            idempotency_key: Some(format!("proposal-{evidence}")),
        },
    )
    .await?;
    let goal_id = ctx
        .resolve_goal(&outcome.handle)
        .expect("goal handle resolves")
        .into_inner();
    Ok(goal_id)
}

async fn propose_for_self(
    pg: &proxima_storage_pg::PgStorage,
    owner: proxima_core::Owner,
) -> Result<(proxima_core::McpToolCtx, uuid::Uuid, uuid::Uuid), Box<dyn std::error::Error>> {
    let mut ctx = ctx(pg, owner.clone());
    let self_id = insert_self_perspective(pg, &owner).await?;
    ctx.caller_self_perspective = Some(self_id);
    let outcome = ProposeTool::call(
        ctx.clone(),
        ProposeArgs {
            payload: GoalPayloadInput::SimpleText(SimpleTextGoalBody {
                title: "proposal".into(),
                text: "proposal".into(),
            }),
            evidence: Vec::new(),
            target_personality: None,
            idempotency_key: Some(format!("proposal-for-self-{self_id:?}")),
        },
    )
    .await?;
    let goal_id = ctx
        .resolve_goal(&outcome.handle)
        .expect("goal handle resolves")
        .into_inner();
    let inspires_edge_handle = outcome
        .inspires_edge_handle
        .as_deref()
        .expect("personality proposal writes core/inspires");
    let inspires_edge_id = ctx
        .resolve_edge(inspires_edge_handle)
        .expect("inspires edge handle resolves")
        .into_inner();
    Ok((ctx, goal_id, inspires_edge_id))
}

async fn assert_inspires_edge_unchanged(
    pg: &proxima_storage_pg::PgStorage,
    edge_id: uuid::Uuid,
    proposal_id: uuid::Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let rows: Vec<(uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
        "SELECT edge_id, source_goal_id
           FROM proxima_core.edges
          WHERE relation = 'core/inspires'
          ORDER BY created_at ASC",
    )
    .fetch_all(pg.pool())
    .await?;
    assert_eq!(rows, vec![(edge_id, proposal_id)]);
    Ok(())
}

async fn read_proposed_lifecycle_fact(
    pg: &proxima_storage_pg::PgStorage,
    goal_id: uuid::Uuid,
) -> Result<(uuid::Uuid, String, String), Box<dyn std::error::Error>> {
    Ok(sqlx::query_as(
        "SELECT memory_id, schema_id, title
           FROM proxima_goal.goal_proposed_v1
          WHERE goal_id = $1",
    )
    .bind(goal_id)
    .fetch_one(pg.pool())
    .await?)
}

async fn read_activated_lifecycle_fact(
    pg: &proxima_storage_pg::PgStorage,
    goal_id: uuid::Uuid,
) -> Result<(uuid::Uuid, String, String, i32), Box<dyn std::error::Error>> {
    Ok(sqlx::query_as(
        "SELECT memory_id, schema_id, title, evidence_count
           FROM proxima_goal.goal_activated_v1
          WHERE goal_id = $1",
    )
    .bind(goal_id)
    .fetch_one(pg.pool())
    .await?)
}

async fn read_achieved_lifecycle_fact(
    pg: &proxima_storage_pg::PgStorage,
    goal_id: uuid::Uuid,
) -> Result<(uuid::Uuid, String, String, i32), Box<dyn std::error::Error>> {
    Ok(sqlx::query_as(
        "SELECT memory_id, schema_id, title, evidence_count
           FROM proxima_goal.goal_achieved_v1
          WHERE goal_id = $1",
    )
    .bind(goal_id)
    .fetch_one(pg.pool())
    .await?)
}

async fn assert_query_contains_lifecycle_authorship(
    pg: &proxima_storage_pg::PgStorage,
    ctx: &proxima_core::McpToolCtx,
    owner: proxima_core::Owner,
    self_id: MemoryId,
    proposed_fact_id: MemoryId,
    activated_fact_id: MemoryId,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut req = QueryRequest::for_owner(owner);
    req.limit = 100;
    let response = pg
        .query_memories(&req, ctx.registry.list().as_slice())
        .await?;
    let expected_facts = [proposed_fact_id, activated_fact_id]
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    let queried_facts = response
        .memories
        .iter()
        .filter(|row| expected_facts.contains(&row.id))
        .collect::<Vec<_>>();
    assert_eq!(queried_facts.len(), 2);
    assert!(queried_facts.iter().all(|row| !row.payload.is_empty()));

    let queried_authored_edges = response
        .edges
        .iter()
        .filter(|edge| {
            edge.relation == "core/authored"
                && edge.source == EntityRef::Memory(self_id)
                && matches!(edge.target, EntityRef::Memory(id) if expected_facts.contains(&id))
        })
        .count();
    assert_eq!(queried_authored_edges, 2);
    Ok(())
}

#[tokio::test]
async fn accept_supersedes_and_re_emits_motivated_by() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let ctx = ctx(&pg, owner_fixture());
        let proposal = propose_with_evidence(&pg, &ctx).await?;
        let proposal_handle = ctx.format_goal(GoalId::new(proposal));

        let accepted = AcceptTool::call(
            ctx.clone(),
            AcceptArgs {
                proposal: proposal_handle.as_str().to_string(),
                payload: None,
                evidence: None,
                target_personality: None,
                idempotency_key: Some("accept-1".into()),
            },
        )
        .await?;
        let accepted_id = ctx
            .resolve_goal(&accepted.handle)
            .expect("goal handle resolves")
            .into_inner();

        let row: (String, Option<uuid::Uuid>) =
            sqlx::query_as("SELECT state, supersedes FROM proxima_core.goals WHERE goal_id = $1")
                .bind(accepted_id)
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(row.0, "Active");
        assert_eq!(row.1, Some(proposal));

        let proposal_edges: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.edges
             WHERE source_goal_id = $1 AND relation = 'proxima-goal/motivated-by'",
        )
        .bind(proposal)
        .fetch_one(pg.pool())
        .await?;
        let active_edges: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.edges
             WHERE source_goal_id = $1 AND relation = 'proxima-goal/motivated-by'",
        )
        .bind(accepted_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(proposal_edges, 1);
        assert_eq!(active_edges, 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn propose_and_accept_emit_lifecycle_facts_and_authored_edges()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let owner = owner_fixture();
        let mut ctx = ctx(&pg, owner.clone());
        let self_id = insert_self_perspective(&pg, &owner).await?;
        ctx.caller_self_perspective = Some(self_id);
        let evidence = insert_abstraction(&pg, &owner).await?;
        let evidence_handle = ctx.format_memory(proxima_core::MemoryId::new(evidence));

        let proposed = ProposeTool::call(
            ctx.clone(),
            ProposeArgs {
                payload: GoalPayloadInput::SimpleText(SimpleTextGoalBody {
                    title: "lifecycle proposal".into(),
                    text: "lifecycle proposal".into(),
                }),
                evidence: vec![evidence_handle.as_str().to_string()],
                target_personality: None,
                idempotency_key: Some("lifecycle-propose".into()),
            },
        )
        .await?;
        let proposal_id = ctx
            .resolve_goal(&proposed.handle)
            .expect("goal handle resolves")
            .into_inner();
        let proposal_handle = ctx.format_goal(GoalId::new(proposal_id));

        let accepted = AcceptTool::call(
            ctx.clone(),
            AcceptArgs {
                proposal: proposal_handle.as_str().to_string(),
                payload: None,
                evidence: None,
                target_personality: None,
                idempotency_key: Some("lifecycle-accept".into()),
            },
        )
        .await?;
        let accepted_id = ctx
            .resolve_goal(&accepted.handle)
            .expect("accepted goal handle resolves")
            .into_inner();

        let proposed_fact = read_proposed_lifecycle_fact(&pg, proposal_id).await?;
        assert_eq!(proposed_fact.1, "proxima-goal/simple-text-v1");
        assert_eq!(proposed_fact.2, "lifecycle proposal");

        let activated_fact = read_activated_lifecycle_fact(&pg, accepted_id).await?;
        assert_eq!(activated_fact.1, "proxima-goal/simple-text-v1");
        assert_eq!(activated_fact.2, "lifecycle proposal");
        assert_eq!(activated_fact.3, 1);

        let authored_targets: Vec<uuid::Uuid> = sqlx::query_scalar(
            "SELECT target_memory_id
               FROM proxima_core.edges
              WHERE relation = 'core/authored'
                AND source_memory_id = $1
              ORDER BY created_at ASC",
        )
        .bind(self_id.into_inner())
        .fetch_all(pg.pool())
        .await?;
        assert_eq!(authored_targets.len(), 2);
        assert!(authored_targets.contains(&proposed_fact.0));
        assert!(authored_targets.contains(&activated_fact.0));

        assert_query_contains_lifecycle_authorship(
            &pg,
            &ctx,
            owner,
            self_id,
            MemoryId::new(proposed_fact.0),
            MemoryId::new(activated_fact.0),
        )
        .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mark_achieved_supersedes_active_goal_with_lifecycle_and_edges()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let owner = owner_fixture();
        let ctx = ctx(&pg, owner);
        let proposal = propose_with_evidence(&pg, &ctx).await?;
        let proposal_handle = ctx.format_goal(GoalId::new(proposal));
        let active = AcceptTool::call(
            ctx.clone(),
            AcceptArgs {
                proposal: proposal_handle.as_str().to_string(),
                payload: None,
                evidence: None,
                target_personality: None,
                idempotency_key: Some("achieve-accept".into()),
            },
        )
        .await?;
        let active_id = ctx
            .resolve_goal(&active.handle)
            .expect("active goal handle resolves")
            .into_inner();
        let evidence = insert_abstraction(&pg, &ctx.owner).await?;

        let achieved = MarkAchievedTool::call(
            ctx.clone(),
            MarkAchievedArgs {
                goal: active_id.to_string(),
                evidence: vec![evidence.to_string()],
                idempotency_key: Some("mark-achieved-1".into()),
            },
        )
        .await?;
        assert!(matches!(achieved.status, MarkAchievedStatus::Achieved));
        let achieved_id = ctx
            .resolve_goal(achieved.handle.as_deref().expect("achieved handle"))
            .expect("achieved goal handle resolves")
            .into_inner();

        let row: (
            String,
            Option<uuid::Uuid>,
            String,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT state, supersedes, authorship_kind, authorship_origin, authorship_tool_id
                   FROM proxima_core.goals
                  WHERE goal_id = $1",
        )
        .bind(achieved_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(row.0, "Achieved");
        assert_eq!(row.1, Some(active_id));
        assert_eq!(row.2, "System");
        assert_eq!(row.3.as_deref(), Some("Tool"));
        assert_eq!(row.4.as_deref(), Some("proxima-goal/goal_mark_achieved"));

        let lifecycle = read_achieved_lifecycle_fact(&pg, achieved_id).await?;
        assert_eq!(lifecycle.1, "proxima-goal/simple-text-v1");
        assert_eq!(lifecycle.2, "proposal");
        assert_eq!(lifecycle.3, 1);

        let motivated_edges: i64 = sqlx::query_scalar(
            "SELECT count(*)
               FROM proxima_core.edges
              WHERE relation = 'proxima-goal/motivated-by'
                AND source_goal_id = $1
                AND target_memory_id = $2",
        )
        .bind(achieved_id)
        .bind(evidence)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(motivated_edges, 1);

        let derived_edges: i64 = sqlx::query_scalar(
            "SELECT count(*)
               FROM proxima_core.edges
              WHERE relation = 'core/derived-from'
                AND source_memory_id = $1
                AND target_memory_id = $2",
        )
        .bind(lifecycle.0)
        .bind(evidence)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(derived_edges, 0);
        assert!(achieved.derived_edge_handles.is_empty());

        let replay = MarkAchievedTool::call(
            ctx.clone(),
            MarkAchievedArgs {
                goal: active_id.to_string(),
                evidence: vec![evidence.to_string()],
                idempotency_key: Some("mark-achieved-1".into()),
            },
        )
        .await?;
        assert!(matches!(
            replay.status,
            MarkAchievedStatus::IdempotentReplay
        ));
        assert_eq!(replay.handle, achieved.handle);

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn mark_achieved_skips_stale_or_terminal_goal_head() -> Result<(), Box<dyn std::error::Error>>
{
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let ctx = ctx(&pg, owner_fixture());
        let proposal = propose_with_evidence(&pg, &ctx).await?;
        let proposal_handle = ctx.format_goal(GoalId::new(proposal));
        let active = AcceptTool::call(
            ctx.clone(),
            AcceptArgs {
                proposal: proposal_handle.as_str().to_string(),
                payload: None,
                evidence: None,
                target_personality: None,
                idempotency_key: Some("stale-achieve-accept".into()),
            },
        )
        .await?;
        let active_id = ctx
            .resolve_goal(&active.handle)
            .expect("active goal handle resolves")
            .into_inner();
        let evidence = insert_abstraction(&pg, &ctx.owner).await?;
        let achieved = MarkAchievedTool::call(
            ctx.clone(),
            MarkAchievedArgs {
                goal: active.handle.clone(),
                evidence: vec![evidence.to_string()],
                idempotency_key: Some("stale-mark-achieved".into()),
            },
        )
        .await?;
        assert!(matches!(achieved.status, MarkAchievedStatus::Achieved));

        let stale = MarkAchievedTool::call(
            ctx.clone(),
            MarkAchievedArgs {
                goal: active_id.to_string(),
                evidence: vec![evidence.to_string()],
                idempotency_key: Some("stale-mark-achieved-2".into()),
            },
        )
        .await?;
        assert!(matches!(stale.status, MarkAchievedStatus::Skipped));
        assert!(
            stale
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("not the current lineage head")
        );

        let terminal = MarkAchievedTool::call(
            ctx,
            MarkAchievedArgs {
                goal: achieved.handle.expect("achieved handle"),
                evidence: vec![evidence.to_string()],
                idempotency_key: Some("terminal-mark-achieved".into()),
            },
        )
        .await?;
        assert!(matches!(terminal.status, MarkAchievedStatus::Skipped));
        assert!(
            terminal
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("not Active")
        );

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn modify_uses_supplied_payload() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let ctx = ctx(&pg, owner_fixture());
        let proposal = propose_with_evidence(&pg, &ctx).await?;
        let proposal_handle = ctx.format_goal(GoalId::new(proposal));

        let modified = ModifyTool::call(
            ctx.clone(),
            ModifyArgs {
                proposal: proposal_handle.as_str().to_string(),
                payload: GoalPayloadInput::SimpleText(SimpleTextGoalBody {
                    title: "rewritten".into(),
                    text: "rewritten".into(),
                }),
                evidence: None,
                idempotency_key: Some("modify-1".into()),
            },
        )
        .await?;
        let modified_id = ctx
            .resolve_goal(&modified.handle)
            .expect("goal handle resolves")
            .into_inner();

        let row: (String, String, Vec<u8>) = sqlx::query_as(
            "SELECT title, text, payload FROM proxima_core.goals WHERE goal_id = $1",
        )
        .bind(modified_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(row.0, "rewritten");
        assert_eq!(row.1, "rewritten");
        let _: proxima_flavor_goal::SimpleTextGoalV1 = ciborium::de::from_reader(&row.2[..])?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn decline_makes_goal_terminal() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let ctx = ctx(&pg, owner_fixture());
        let proposal = propose_with_evidence(&pg, &ctx).await?;
        let proposal_handle = ctx.format_goal(GoalId::new(proposal));

        let declined = DeclineTool::call(
            ctx.clone(),
            DeclineArgs {
                proposal: proposal_handle.as_str().to_string(),
                idempotency_key: Some("decline-1".into()),
            },
        )
        .await?;
        let declined_id = ctx
            .resolve_goal(&declined.handle)
            .expect("goal handle resolves")
            .into_inner();

        let state: String =
            sqlx::query_scalar("SELECT state FROM proxima_core.goals WHERE goal_id = $1")
                .bind(declined_id)
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(state, "Rejected");

        let err = ModifyTool::call(
            ctx,
            ModifyArgs {
                proposal: declined.handle.clone(),
                payload: GoalPayloadInput::SimpleText(SimpleTextGoalBody {
                    title: "retry".into(),
                    text: "retry".into(),
                }),
                evidence: None,
                idempotency_key: Some("modify-rejected".into()),
            },
        )
        .await
        .expect_err("rejected goal is terminal");
        assert!(err.to_string().contains("goal: state=Rejected is terminal"));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn accept_and_decline_preserve_inspires_edge() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let owner = owner_fixture();
        let (ctx, proposal, edge_id) = propose_for_self(&pg, owner.clone()).await?;
        let proposal_handle = ctx.format_goal(GoalId::new(proposal));

        let accepted = AcceptTool::call(
            ctx.clone(),
            AcceptArgs {
                proposal: proposal_handle.as_str().to_string(),
                payload: None,
                evidence: None,
                target_personality: None,
                idempotency_key: Some("accept-preserve-inspires".into()),
            },
        )
        .await?;
        let accepted_id = ctx
            .resolve_goal(&accepted.handle)
            .expect("goal handle resolves")
            .into_inner();
        assert_ne!(accepted_id, proposal);
        assert_inspires_edge_unchanged(&pg, edge_id, proposal).await?;

        let (ctx, declined_proposal, declined_edge_id) = propose_for_self(&pg, owner).await?;
        let declined_handle = ctx.format_goal(GoalId::new(declined_proposal));
        let declined = DeclineTool::call(
            ctx.clone(),
            DeclineArgs {
                proposal: declined_handle.as_str().to_string(),
                idempotency_key: Some("decline-preserve-inspires".into()),
            },
        )
        .await?;
        let declined_id = ctx
            .resolve_goal(&declined.handle)
            .expect("goal handle resolves")
            .into_inner();
        assert_ne!(declined_id, declined_proposal);

        let rows: Vec<(uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
            "SELECT edge_id, source_goal_id
               FROM proxima_core.edges
              WHERE relation = 'core/inspires'
              ORDER BY created_at ASC",
        )
        .fetch_all(pg.pool())
        .await?;
        assert_eq!(
            rows,
            vec![(edge_id, proposal), (declined_edge_id, declined_proposal)]
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

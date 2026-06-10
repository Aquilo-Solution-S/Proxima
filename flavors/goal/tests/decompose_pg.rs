mod common;

use common::{ctx, drop_db, insert_abstraction, insert_self_perspective, migrated, owner_fixture};
use proxima_core::mcp::McpTool;
use proxima_core::verbs::goal_write::{GoalAuthorshipKind, GoalAuthorshipOrigin, GoalState};
use proxima_core::{EdgeAuthorshipKind, GoalId, MemoryId};
use proxima_flavor_goal::tools::accept::{AcceptArgs, AcceptTool};
use proxima_flavor_goal::tools::decompose::{ChildGoalInput, DecomposeArgs, DecomposeTool};
use proxima_flavor_goal::tools::propose::{ProposeArgs, ProposeTool};
use proxima_flavor_goal::tools::util::{GoalPayloadInput, SimpleTextGoalBody};

async fn active_parent(
    pg: &proxima_storage_pg::PgStorage,
    ctx: &proxima_core::McpToolCtx,
) -> Result<GoalId, Box<dyn std::error::Error>> {
    let proposal = ProposeTool::call(
        ctx.clone(),
        ProposeArgs {
            payload: simple_goal("parent"),
            evidence: Vec::new(),
            target_personality: None,
            idempotency_key: Some(format!("parent-{}", uuid::Uuid::now_v7())),
        },
    )
    .await?;
    let accepted = AcceptTool::call(
        ctx.clone(),
        AcceptArgs {
            proposal: proposal.handle,
            payload: None,
            evidence: None,
            target_personality: None,
            idempotency_key: Some(format!("accept-parent-{}", uuid::Uuid::now_v7())),
        },
    )
    .await?;
    let goal_id = ctx.resolve_goal(&accepted.handle)?;
    assert_goal_state(pg, goal_id, GoalState::Active).await?;
    Ok(goal_id)
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "linear decompose e2e: children, parent links, assignments"
)]
async fn goal_decompose_writes_active_children_parent_links_and_assignments()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let owner = owner_fixture();
        let mut ctx = ctx(&pg, owner.clone());
        let self_id = insert_self_perspective(&pg, &owner).await?;
        ctx.caller_self_perspective = Some(self_id);
        let parent = active_parent(&pg, &ctx).await?;
        let evidence = insert_abstraction(&pg, &owner).await?;
        let evidence_handle = ctx.format_abstraction_memory(MemoryId::new(evidence));

        let output = DecomposeTool::call(
            ctx.clone(),
            DecomposeArgs {
                parent_goal: ctx.format_goal(parent),
                children: vec![
                    ChildGoalInput {
                        payload: simple_goal("child one"),
                        evidence: vec![evidence_handle],
                    },
                    ChildGoalInput {
                        payload: simple_goal("child two"),
                        evidence: Vec::new(),
                    },
                ],
                target_personality: None,
                activate_children: true,
                idempotency_key: "decompose-active".into(),
            },
        )
        .await?;

        assert_eq!(output.children.len(), 2);
        assert!(!output.idempotent_replay);
        let child_ids = output
            .children
            .iter()
            .map(|child| {
                assert!(child.lifecycle_memory.is_some());
                assert!(child.inspires_edge_handle.is_some());
                ctx.resolve_goal(&child.handle).map(GoalId::into_inner)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let parent_links: Vec<(uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
            "SELECT goal_id, parent_goal_id
               FROM proxima_core.goal_parents
              WHERE parent_goal_id = $1
              ORDER BY goal_id",
        )
        .bind(parent.into_inner())
        .fetch_all(pg.pool())
        .await?;
        assert_eq!(parent_links.len(), 2);
        assert!(
            parent_links
                .iter()
                .all(|(_, parent_id)| *parent_id == parent.into_inner())
        );

        for child_id in &child_ids {
            let row: (
                GoalState,
                GoalAuthorshipKind,
                Option<GoalAuthorshipOrigin>,
                Option<String>,
            ) = sqlx::query_as(
                "SELECT state, authorship_kind, authorship_origin, authorship_tool_id
                   FROM proxima_core.goals
                  WHERE goal_id = $1",
            )
            .bind(child_id)
            .fetch_one(pg.pool())
            .await?;
            assert_eq!(row.0, GoalState::Active);
            assert_eq!(row.1, GoalAuthorshipKind::System);
            assert_eq!(row.2, Some(GoalAuthorshipOrigin::Tool));
            assert_eq!(row.3.as_deref(), Some("proxima-goal/goal_decompose"));

            let activated_count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM proxima_goal.goal_activated_v1 WHERE goal_id = $1",
            )
            .bind(child_id)
            .fetch_one(pg.pool())
            .await?;
            assert_eq!(activated_count, 1);

            let inspires: (String, uuid::Uuid, uuid::Uuid, EdgeAuthorshipKind) = sqlx::query_as(
                "SELECT relation, source_goal_id, target_memory_id, authorship_kind
                   FROM proxima_core.edges
                  WHERE source_goal_id = $1
                    AND relation = 'core/inspires'",
            )
            .bind(child_id)
            .fetch_one(pg.pool())
            .await?;
            assert_eq!(inspires.0, "core/inspires");
            assert_eq!(inspires.1, *child_id);
            assert_eq!(inspires.2, self_id.into_inner());
            assert_eq!(inspires.3, EdgeAuthorshipKind::ExternalAgent);
        }

        let motivated_count: i64 = sqlx::query_scalar(
            "SELECT count(*)
               FROM proxima_core.edges
              WHERE source_goal_id = $1
                AND relation = 'proxima-goal/motivated-by'",
        )
        .bind(child_ids[0])
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(motivated_count, 1);

        let replay = DecomposeTool::call(
            ctx.clone(),
            DecomposeArgs {
                parent_goal: ctx.format_goal(parent),
                children: vec![
                    ChildGoalInput {
                        payload: simple_goal("child one"),
                        evidence: Vec::new(),
                    },
                    ChildGoalInput {
                        payload: simple_goal("child two"),
                        evidence: Vec::new(),
                    },
                ],
                target_personality: None,
                activate_children: true,
                idempotency_key: "decompose-active".into(),
            },
        )
        .await?;
        assert!(replay.idempotent_replay);
        let total_children: i64 = sqlx::query_scalar(
            "SELECT count(*)
               FROM proxima_core.goal_parents
              WHERE parent_goal_id = $1",
        )
        .bind(parent.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(total_children, 2);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn goal_decompose_defaults_to_proposed_children_without_assignment()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let owner = owner_fixture();
        let ctx = ctx(&pg, owner);
        let parent = active_parent(&pg, &ctx).await?;

        let output = DecomposeTool::call(
            ctx.clone(),
            DecomposeArgs {
                parent_goal: ctx.format_goal(parent),
                children: vec![ChildGoalInput {
                    payload: simple_goal("proposed child"),
                    evidence: Vec::new(),
                }],
                target_personality: None,
                activate_children: false,
                idempotency_key: "decompose-proposed".into(),
            },
        )
        .await?;
        let child_id = ctx.resolve_goal(&output.children[0].handle)?;
        assert_goal_state(&pg, child_id, GoalState::Proposed).await?;
        assert!(output.children[0].inspires_edge_handle.is_none());

        let proposed_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_goal.goal_proposed_v1 WHERE goal_id = $1",
        )
        .bind(child_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(proposed_count, 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn goal_decompose_accepts_parent_activation_memory_handle()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let owner = owner_fixture();
        let ctx = ctx(&pg, owner);
        let parent = active_parent(&pg, &ctx).await?;
        let activation_memory: uuid::Uuid = sqlx::query_scalar(
            "SELECT memory_id
               FROM proxima_goal.goal_activated_v1
              WHERE goal_id = $1",
        )
        .bind(parent.into_inner())
        .fetch_one(pg.pool())
        .await?;

        let output = DecomposeTool::call(
            ctx.clone(),
            DecomposeArgs {
                parent_goal: ctx.format_fact_memory(MemoryId::new(activation_memory)),
                children: vec![ChildGoalInput {
                    payload: simple_goal("child from activation"),
                    evidence: Vec::new(),
                }],
                target_personality: None,
                activate_children: false,
                idempotency_key: "decompose-activation-handle".into(),
            },
        )
        .await?;

        assert_eq!(output.parent_goal, ctx.format_goal(parent));
        assert_eq!(output.children.len(), 1);
        let child_id = ctx.resolve_goal(&output.children[0].handle)?;
        let parent_link_count: i64 = sqlx::query_scalar(
            "SELECT count(*)
               FROM proxima_core.goal_parents
              WHERE goal_id = $1
                AND parent_goal_id = $2",
        )
        .bind(child_id.into_inner())
        .bind(parent.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(parent_link_count, 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn goal_decompose_rejects_non_active_parent() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let owner = owner_fixture();
        let ctx = ctx(&pg, owner);
        let proposal = ProposeTool::call(
            ctx.clone(),
            ProposeArgs {
                payload: simple_goal("parent"),
                evidence: Vec::new(),
                target_personality: None,
                idempotency_key: Some("inactive-parent".into()),
            },
        )
        .await?;

        let err = DecomposeTool::call(
            ctx,
            DecomposeArgs {
                parent_goal: proposal.handle,
                children: vec![ChildGoalInput {
                    payload: simple_goal("child"),
                    evidence: Vec::new(),
                }],
                target_personality: None,
                activate_children: false,
                idempotency_key: "decompose-inactive-parent".into(),
            },
        )
        .await
        .expect_err("non-active parent must fail");
        assert!(err.to_string().contains("parent_goal must be Active"));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

async fn assert_goal_state(
    pg: &proxima_storage_pg::PgStorage,
    goal_id: GoalId,
    expected: GoalState,
) -> Result<(), Box<dyn std::error::Error>> {
    let state: GoalState = sqlx::query_scalar(
        "SELECT state
           FROM proxima_core.goals
          WHERE goal_id = $1",
    )
    .bind(goal_id.into_inner())
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(state, expected);
    Ok(())
}

fn simple_goal(title: &str) -> GoalPayloadInput {
    GoalPayloadInput::SimpleText(SimpleTextGoalBody {
        title: title.into(),
        text: format!("{title} text"),
    })
}

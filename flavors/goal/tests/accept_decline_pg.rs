mod common;

use common::{ctx, drop_db, insert_abstraction, migrated, owner_fixture};
use proxima_core::GoalId;
use proxima_core::mcp::McpTool;
use proxima_flavor_goal::tools::accept::{AcceptArgs, AcceptTool};
use proxima_flavor_goal::tools::decline::{DeclineArgs, DeclineTool};
use proxima_flavor_goal::tools::modify::{ModifyArgs, ModifyTool};
use proxima_flavor_goal::tools::propose::{ProposeArgs, ProposeTool};
use proxima_flavor_goal::tools::util::{GoalPayloadInput, SimpleTextGoalBody};

async fn propose_with_evidence(
    pg: &proxima_storage_pg::PgStorage,
    ctx: &proxima_core::McpToolCtx,
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let evidence = insert_abstraction(pg, &ctx.owner).await?;
    let evidence_handle = ctx
        .handles
        .assign_memory(proxima_core::MemoryId::new(evidence));
    let outcome = ProposeTool::call(
        ctx.clone(),
        ProposeArgs {
            payload: GoalPayloadInput::SimpleText(SimpleTextGoalBody {
                title: "proposal".into(),
                text: "proposal".into(),
            }),
            evidence: vec![evidence_handle.as_str().to_string()],
            idempotency_key: Some(format!("proposal-{evidence}")),
        },
    )
    .await?;
    Ok(outcome.uuid)
}

#[tokio::test]
async fn accept_supersedes_and_re_emits_motivated_by() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let ctx = ctx(&pg, owner_fixture());
        let proposal = propose_with_evidence(&pg, &ctx).await?;
        let proposal_handle = ctx.handles.assign_goal(GoalId::new(proposal));

        let accepted = AcceptTool::call(
            ctx.clone(),
            AcceptArgs {
                proposal: proposal_handle.as_str().to_string(),
                payload: None,
                evidence: None,
                idempotency_key: Some("accept-1".into()),
            },
        )
        .await?;

        let row: (String, Option<uuid::Uuid>) =
            sqlx::query_as("SELECT state, supersedes FROM proxima_core.goals WHERE goal_id = $1")
                .bind(accepted.uuid)
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
        .bind(accepted.uuid)
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
async fn modify_uses_supplied_payload() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let ctx = ctx(&pg, owner_fixture());
        let proposal = propose_with_evidence(&pg, &ctx).await?;
        let proposal_handle = ctx.handles.assign_goal(GoalId::new(proposal));

        let modified = ModifyTool::call(
            ctx,
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

        let row: (String, String, Vec<u8>) = sqlx::query_as(
            "SELECT title, text, payload FROM proxima_core.goals WHERE goal_id = $1",
        )
        .bind(modified.uuid)
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
        let proposal_handle = ctx.handles.assign_goal(GoalId::new(proposal));

        let declined = DeclineTool::call(
            ctx.clone(),
            DeclineArgs {
                proposal: proposal_handle.as_str().to_string(),
                idempotency_key: Some("decline-1".into()),
            },
        )
        .await?;

        let state: String =
            sqlx::query_scalar("SELECT state FROM proxima_core.goals WHERE goal_id = $1")
                .bind(declined.uuid)
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(state, "Rejected");

        let declined_handle = ctx.handles.assign_goal(GoalId::new(declined.uuid));
        let err = ModifyTool::call(
            ctx,
            ModifyArgs {
                proposal: declined_handle.as_str().to_string(),
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

mod common;

use common::{
    ctx, drop_db, insert_abstraction, insert_self_perspective, migrated, other_owner_fixture,
    owner_fixture,
};
use proxima_core::GoalId;
use proxima_core::mcp::McpTool;
use proxima_core::verbs::goal_write::GoalState;
use proxima_flavor_goal::tools::propose::{ProposeArgs, ProposeTool};
use proxima_flavor_goal::tools::util::{GoalPayloadInput, SimpleTextGoalBody};

#[tokio::test]
async fn propose_writes_goal_and_motivated_by_atomically() -> Result<(), Box<dyn std::error::Error>>
{
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let owner = owner_fixture();
        let ctx = ctx(&pg, owner.clone());
        let evidence = insert_abstraction(&pg, &owner).await?;
        let evidence_handle = ctx
            .handles
            .assign_memory(proxima_core::MemoryId::new(evidence));

        let outcome = ProposeTool::call(
            ctx.clone(),
            ProposeArgs {
                payload: GoalPayloadInput::SimpleText(SimpleTextGoalBody {
                    title: "ship goal flavor".into(),
                    text: "ship goal flavor".into(),
                }),
                evidence: vec![evidence_handle.as_str().to_string()],
                idempotency_key: Some("proposal-1".into()),
            },
        )
        .await?;

        let goal: (String, String, String, String, Vec<u8>) = sqlx::query_as(
            "SELECT state, authorship_kind, title, text, payload
             FROM proxima_core.goals
             WHERE goal_id = $1",
        )
        .bind(outcome.uuid)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(goal.0, "Proposed");
        assert_eq!(goal.1, "External");
        assert_eq!(goal.2, "ship goal flavor");
        assert_eq!(goal.3, "ship goal flavor");
        let _: proxima_flavor_goal::SimpleTextGoalV1 = ciborium::de::from_reader(&goal.4[..])?;

        let edge_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.edges
             WHERE source_goal_id = $1 AND relation = 'proxima-goal/motivated-by'",
        )
        .bind(outcome.uuid)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(edge_count, 1);
        assert_eq!(outcome.inspires_edge_id, None);

        let handle = ctx.handles.assign_goal(GoalId::new(outcome.uuid));
        assert_eq!(handle.as_str(), outcome.handle);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn propose_writes_inspires_edge_for_personality_caller()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let owner = owner_fixture();
        let mut ctx = ctx(&pg, owner.clone());
        let self_id = insert_self_perspective(&pg, &owner).await?;
        ctx.caller_self_perspective = Some(self_id);

        let outcome = ProposeTool::call(
            ctx,
            ProposeArgs {
                payload: GoalPayloadInput::SimpleText(SimpleTextGoalBody {
                    title: "connect goal".into(),
                    text: "connect goal".into(),
                }),
                evidence: Vec::new(),
                idempotency_key: Some("proposal-with-self".into()),
            },
        )
        .await?;

        let inspires_edge_id = outcome
            .inspires_edge_id
            .expect("personality caller writes core/inspires edge");
        let row: (String, uuid::Uuid, uuid::Uuid, String, Option<uuid::Uuid>) = sqlx::query_as(
            "SELECT relation, source_goal_id, target_memory_id, authorship_kind,
                    authorship_owner_memory_id
               FROM proxima_core.edges
              WHERE edge_id = $1",
        )
        .bind(inspires_edge_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(row.0, "core/inspires");
        assert_eq!(row.1, outcome.uuid);
        assert_eq!(row.2, self_id.into_inner());
        assert_eq!(row.3, "ExternalAgent");
        assert_eq!(row.4, Some(self_id.into_inner()));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn propose_rejects_evidence_in_other_owner() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let owner = owner_fixture();
        let other = other_owner_fixture();
        let ctx = ctx(&pg, owner);
        let evidence = insert_abstraction(&pg, &other).await?;
        let evidence_handle = ctx
            .handles
            .assign_memory(proxima_core::MemoryId::new(evidence));

        let err = ProposeTool::call(
            ctx,
            ProposeArgs {
                payload: GoalPayloadInput::SimpleText(SimpleTextGoalBody {
                    title: "x".into(),
                    text: "x".into(),
                }),
                evidence: vec![evidence_handle.as_str().to_string()],
                idempotency_key: None,
            },
        )
        .await
        .expect_err("cross-owner evidence must fail");
        assert!(err.to_string().contains("crosses Owner boundary"));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[test]
fn proposed_state_is_exposed_for_tool_tests() {
    assert_eq!(GoalState::Proposed, GoalState::Proposed);
}

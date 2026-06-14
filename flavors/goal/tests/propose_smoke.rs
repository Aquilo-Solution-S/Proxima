mod common;

use common::{
    ctx, drop_db, insert_abstraction, insert_self_perspective, migrated, other_owner_fixture,
    owner_fixture,
};
use proxima_core::mcp::McpTool;
use proxima_core::verbs::goal_write::{
    GoalAuthorshipKind, GoalAuthorshipOrigin, GoalState, OperatorKind,
};
use proxima_core::{EdgeAuthorshipKind, PersonalityInstanceId};
use proxima_flavor_goal::tools::propose::{ProposeArgs, ProposeTool};
use proxima_flavor_goal::tools::util::{GoalPayloadInput, SimpleTextGoalBody};

#[derive(Debug, sqlx::FromRow)]
struct GoalAuthorshipRow {
    authorship_kind: GoalAuthorshipKind,
    authorship_origin: Option<GoalAuthorshipOrigin>,
    authorship_operator_id: Option<uuid::Uuid>,
    operator_kind: Option<OperatorKind>,
    model_id: Option<String>,
    prompt_version: Option<String>,
    personality_instance_id: Option<uuid::Uuid>,
    authorship_tool_id: Option<String>,
}

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
        let evidence_handle = ctx.format_abstraction_memory(proxima_core::MemoryId::new(evidence));

        let outcome = ProposeTool::call(
            ctx.clone(),
            ProposeArgs {
                payload: GoalPayloadInput::SimpleText(SimpleTextGoalBody {
                    title: "ship goal flavor".into(),
                    text: "ship goal flavor".into(),
                }),
                evidence: vec![evidence_handle],
                target_personality: None,
                idempotency_key: Some("proposal-1".into()),
            },
        )
        .await?;
        let goal_id = ctx
            .resolve_goal(&outcome.handle)
            .expect("goal handle resolves")
            .into_inner();

        let goal: (GoalState, GoalAuthorshipKind, String, String, Vec<u8>) = sqlx::query_as(
            "SELECT state, authorship_kind, title, text, payload
             FROM proxima_core.goals
             WHERE goal_id = $1",
        )
        .bind(goal_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(goal.0, GoalState::Proposed);
        assert_eq!(goal.1, GoalAuthorshipKind::External);
        assert_eq!(goal.2, "ship goal flavor");
        assert_eq!(goal.3, "ship goal flavor");
        let _: proxima_flavor_goal::SimpleTextGoalV1 = serde_json::from_slice(&goal.4)?;

        let edge_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.edges
             WHERE source_goal_id = $1 AND relation = 'proxima-goal/motivated-by'",
        )
        .bind(goal_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(edge_count, 1);
        assert_eq!(outcome.inspires_edge_handle, None);

        let proposed_fact_count: i64 = sqlx::query_scalar(
            "SELECT count(*)
               FROM proxima_goal.goal_proposed_v1
              WHERE goal_id = $1",
        )
        .bind(goal_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(proposed_fact_count, 1);

        let authored_count: i64 = sqlx::query_scalar(
            "SELECT count(*)
               FROM proxima_core.edges
              WHERE relation = 'core/authored'",
        )
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(authored_count, 0);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn propose_with_author_personality_persists_system_operator_authorship()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let owner = owner_fixture();
        let mut ctx = ctx(&pg, owner);
        let personality_id = PersonalityInstanceId::new(uuid::Uuid::now_v7());
        ctx.author.personality_instance_id = Some(personality_id);

        let outcome = ProposeTool::call(
            ctx.clone(),
            ProposeArgs {
                payload: GoalPayloadInput::SimpleText(SimpleTextGoalBody {
                    title: "attribute proposed goal".into(),
                    text: "attribute proposed goal".into(),
                }),
                evidence: Vec::new(),
                target_personality: None,
                idempotency_key: Some("proposal-with-author-personality".into()),
            },
        )
        .await?;
        let goal_id = ctx
            .resolve_goal(&outcome.handle)
            .expect("goal handle resolves")
            .into_inner();

        let row: GoalAuthorshipRow = sqlx::query_as(
            "SELECT authorship_kind, authorship_origin, authorship_operator_id,
                    operator_kind, model_id, prompt_version, personality_instance_id,
                    authorship_tool_id
               FROM proxima_core.goals
              WHERE goal_id = $1",
        )
        .bind(goal_id)
        .fetch_one(pg.pool())
        .await?;

        assert_eq!(row.authorship_kind, GoalAuthorshipKind::System);
        assert_eq!(row.authorship_origin, Some(GoalAuthorshipOrigin::Operator));
        assert!(row.authorship_operator_id.is_some());
        assert_eq!(row.operator_kind, Some(OperatorKind::AtoGoal));
        assert_eq!(row.model_id.as_deref(), Some("test-model"));
        assert_eq!(row.prompt_version.as_deref(), Some("external-agent"));
        assert_eq!(
            row.personality_instance_id,
            Some(personality_id.into_inner())
        );
        assert_eq!(row.authorship_tool_id, None);
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
            ctx.clone(),
            ProposeArgs {
                payload: GoalPayloadInput::SimpleText(SimpleTextGoalBody {
                    title: "connect goal".into(),
                    text: "connect goal".into(),
                }),
                evidence: Vec::new(),
                target_personality: None,
                idempotency_key: Some("proposal-with-self".into()),
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
            .expect("personality caller writes core/inspires edge");
        let inspires_edge_id = ctx
            .resolve_edge(inspires_edge_handle)
            .expect("inspires edge handle resolves")
            .into_inner();
        let row: (
            String,
            uuid::Uuid,
            uuid::Uuid,
            EdgeAuthorshipKind,
            Option<uuid::Uuid>,
        ) = sqlx::query_as(
            "SELECT relation, source_goal_id, target_memory_id, authorship_kind,
                    authorship_owner_memory_id
               FROM proxima_core.edges
              WHERE edge_id = $1",
        )
        .bind(inspires_edge_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(row.0, "core/inspires");
        assert_eq!(row.1, goal_id);
        assert_eq!(row.2, self_id.into_inner());
        assert_eq!(row.3, EdgeAuthorshipKind::ExternalAgent);
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
        let evidence_handle = ctx.format_abstraction_memory(proxima_core::MemoryId::new(evidence));

        let err = ProposeTool::call(
            ctx,
            ProposeArgs {
                payload: GoalPayloadInput::SimpleText(SimpleTextGoalBody {
                    title: "x".into(),
                    text: "x".into(),
                }),
                evidence: vec![evidence_handle],
                target_personality: None,
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

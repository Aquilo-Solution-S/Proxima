mod common;

use std::sync::Arc;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::auth::NoAuth;
use proxima_core::mcp::McpAuthorContext;
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{
    ApprovalDecision, ApprovalEligibleVoter, ApprovalRequirement, ApprovalRequirementKind,
    ApprovalTargetKind, ApprovalVoteVerdict, ApprovalVoterKind, DIRECTED_QUESTION_SCHEMA_ID,
    EmitApprovalPolicyTool, EmitApprovalVoteTool, EmitDirectedAnswerTool, EmitDirectedQuestionTool,
    Engine, FlavorRegistry, GetInquiryThreadTool, ListInquiryTargetsTool, McpTool, McpToolCtx,
    MemoryId, ModelTier, OutputMode, SetWakeEntriesRequest, Storage, TryEmitApprovalDecisionOutput,
    TryEmitApprovalDecisionTool, WakeEntryAuthoredBy, WakeEntryDraft, WakeEntryExecutionMode,
    WakeEntryRow, WakeEntryTriggerKind, build_wake_coordination_context,
};
use uuid::Uuid;

#[tokio::test]
async fn directed_inquiry_round_trip_and_coordination_context()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let planner = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Planner".into(),
            purpose: "Ask implementation questions".into(),
        })
        .await?;
    let answerer = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Testing Engineer".into(),
            purpose: "Answer test-design questions".into(),
        })
        .await?;
    let rows = pg.list_personality_instances(&owner, false).await?;
    let planner_self = rows
        .iter()
        .find(|row| row.personality_instance_id == planner.instance_id)
        .expect("planner row")
        .current_root_perspective_memory_id;
    let answerer_self = rows
        .iter()
        .find(|row| row.personality_instance_id == answerer.instance_id)
        .expect("answerer row")
        .current_root_perspective_memory_id;

    let planner_wake = wake(
        planner.instance_id,
        "core/goal-activated-v1",
        "planner",
        vec!["core/emit_directed_question".into()],
    );
    let answerer_wake = wake(
        answerer.instance_id,
        DIRECTED_QUESTION_SCHEMA_ID,
        "directed-question",
        vec!["core/emit_directed_answer".into()],
    );
    pg.set_wake_entries(&SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id: planner.instance_id,
        entries: vec![planner_wake.clone()],
    })
    .await?;
    pg.set_wake_entries(&SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id: answerer.instance_id,
        entries: vec![answerer_wake],
    })
    .await?;

    let planner_ctx = ctx(&pg, owner.clone(), planner_self);
    let targets = ListInquiryTargetsTool::call(
        planner_ctx.clone(),
        proxima_core::ListInquiryTargetsArgs {
            include_self: false,
        },
    )
    .await?;
    assert_eq!(targets.targets.len(), 1);
    assert_eq!(targets.targets[0].display_name, "Testing Engineer");

    let question = EmitDirectedQuestionTool::call(
        planner_ctx.clone(),
        proxima_core::EmitDirectedQuestionArgs {
            target_personality: answerer.instance_id.into_inner().to_string(),
            thread_key: "planning-discussion".into(),
            question: "Which tests do you require?".into(),
            parent_question: None,
            context_memories: Vec::new(),
            context_goals: Vec::new(),
            idempotency_key: "question-1".into(),
        },
    )
    .await?;
    assert!(question.target_edge_handle.is_some());

    let wrong_answer = EmitDirectedAnswerTool::call(
        planner_ctx,
        proxima_core::EmitDirectedAnswerArgs {
            question: question.handle.clone(),
            answer: "wrong caller".into(),
            context_memories_used: Vec::new(),
            idempotency_key: "answer-wrong".into(),
        },
    )
    .await
    .expect_err("planner must not answer answerer's question");
    assert!(wrong_answer.to_string().contains("addressed target"));

    let answer = EmitDirectedAnswerTool::call(
        ctx(&pg, owner.clone(), answerer_self),
        proxima_core::EmitDirectedAnswerArgs {
            question: question.handle,
            answer: "I require a target-discovery test and a wrong-self rejection test.".into(),
            context_memories_used: Vec::new(),
            idempotency_key: "answer-1".into(),
        },
    )
    .await?;
    assert!(answer.answer_edge_handle.is_some());

    let question_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.directed_question_v1")
            .fetch_one(pg.pool())
            .await?;
    let answer_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.directed_answer_v1")
            .fetch_one(pg.pool())
            .await?;
    assert_eq!(question_rows, 1);
    assert_eq!(answer_rows, 1);

    let relation_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.edges
          WHERE relation IN ('core/receives-directed-question', 'core/answers-question')",
    )
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(relation_rows, 2);

    let engine = engine(&pg, owner.clone());
    let coordination = build_wake_coordination_context(
        &engine,
        &owner,
        planner.instance_id,
        &wake_row(&planner_wake),
    )
    .await?;
    assert_eq!(coordination.askable_personalities.len(), 1);
    assert_eq!(
        coordination.askable_personalities[0].personality_instance_id,
        answerer.instance_id.into_inner()
    );
    assert!(
        coordination
            .wake_path
            .downstream
            .iter()
            .any(|node| node.personality_instance_id == answerer.instance_id.into_inner())
    );

    drop(engine);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn directed_question_requires_target_wake_entry() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let planner = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Planner".into(),
            purpose: "Ask questions".into(),
        })
        .await?;
    let silent = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Silent".into(),
            purpose: "No inquiry wake".into(),
        })
        .await?;
    let planner_self = pg
        .list_personality_instances(&owner, false)
        .await?
        .into_iter()
        .find(|row| row.personality_instance_id == planner.instance_id)
        .expect("planner row")
        .current_root_perspective_memory_id;
    let err = EmitDirectedQuestionTool::call(
        ctx(&pg, owner, planner_self),
        proxima_core::EmitDirectedQuestionArgs {
            target_personality: silent.instance_id.into_inner().to_string(),
            thread_key: "missing-wake".into(),
            question: "Can you answer?".into(),
            parent_question: None,
            context_memories: Vec::new(),
            context_goals: Vec::new(),
            idempotency_key: "missing-wake-question".into(),
        },
    )
    .await
    .expect_err("target without directed-question wake must be rejected");
    assert!(err.to_string().contains("directed-question wake entry"));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn planning_round_is_observable_as_graph() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let planner = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Master Planner".into(),
            purpose: "Coordinate planning rounds".into(),
        })
        .await?;
    let tester = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Testing Engineer".into(),
            purpose: "Define acceptance tests".into(),
        })
        .await?;
    let decider = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Decider".into(),
            purpose: "Approve complete plans".into(),
        })
        .await?;
    let rows = pg.list_personality_instances(&owner, false).await?;
    let planner_self = self_perspective(&rows, planner.instance_id);
    let tester_self = self_perspective(&rows, tester.instance_id);
    let decider_self = self_perspective(&rows, decider.instance_id);

    pg.set_wake_entries(&SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id: planner.instance_id,
        entries: vec![wake(
            planner.instance_id,
            "core/goal-activated-v1",
            "planning",
            vec![
                "core/emit_directed_question".into(),
                "core/emit_approval_policy".into(),
            ],
        )],
    })
    .await?;
    pg.set_wake_entries(&SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id: tester.instance_id,
        entries: vec![wake(
            tester.instance_id,
            DIRECTED_QUESTION_SCHEMA_ID,
            "answer-planning-question",
            vec!["core/emit_directed_answer".into()],
        )],
    })
    .await?;

    let question = EmitDirectedQuestionTool::call(
        ctx(&pg, owner.clone(), planner_self),
        proxima_core::EmitDirectedQuestionArgs {
            target_personality: tester.instance_id.into_inner().to_string(),
            thread_key: "meaningful-planning-round".into(),
            question: "Propose the required tests T1..Tn before implementation.".into(),
            parent_question: None,
            context_memories: Vec::new(),
            context_goals: Vec::new(),
            idempotency_key: "planning-round-question".into(),
        },
    )
    .await?;
    let question_handle = question.handle.clone();
    let answer = EmitDirectedAnswerTool::call(
        ctx(&pg, owner.clone(), tester_self),
        proxima_core::EmitDirectedAnswerArgs {
            question: question.handle.clone(),
            answer: "T1: target discovery lists the tester. T2: wrong self cannot answer. T3: the final plan answer is approved by the decider.".into(),
            context_memories_used: Vec::new(),
            idempotency_key: "planning-round-answer".into(),
        },
    )
    .await?;
    let policy = EmitApprovalPolicyTool::call(
        ctx(&pg, owner.clone(), planner_self),
        proxima_core::EmitApprovalPolicyArgs {
            target: answer.handle.clone(),
            target_kind: ApprovalTargetKind::Fact,
            title: "Approve planning answer".into(),
            summary: "Decider approval closes the planning round.".into(),
            eligible_voters: vec![ApprovalEligibleVoter {
                voter_key: "decider".into(),
                kind: ApprovalVoterKind::Personality,
                role: Some("decider".into()),
                personality_instance_id: Some(decider.instance_id.into_inner()),
                self_perspective_memory_id: Some(decider_self.into_inner()),
            }],
            requirements: vec![ApprovalRequirement {
                kind: ApprovalRequirementKind::AllOfVoters,
                voter_keys: vec!["decider".into()],
                role: None,
                min_approvals: None,
            }],
            idempotency_key: "planning-round-policy".into(),
        },
    )
    .await?;
    let policy_handle = policy.handle.clone();
    let vote = EmitApprovalVoteTool::call(
        ctx(&pg, owner.clone(), decider_self),
        proxima_core::EmitApprovalVoteArgs {
            policy: policy.handle.clone(),
            voter_key: "decider".into(),
            verdict: ApprovalVoteVerdict::Approved,
            rationale: "The test engineer separated acceptance tests before implementation.".into(),
            idempotency_key: "planning-round-vote".into(),
        },
    )
    .await?;
    let decision = TryEmitApprovalDecisionTool::call(
        ctx(&pg, owner.clone(), decider_self),
        proxima_core::TryEmitApprovalDecisionArgs {
            policy: policy.handle.clone(),
            idempotency_key: "planning-round-decision".into(),
        },
    )
    .await?;
    let TryEmitApprovalDecisionOutput::Written {
        handle: decision_handle,
        decision,
        ..
    } = decision
    else {
        panic!("expected planning approval decision");
    };
    assert_eq!(decision, ApprovalDecision::Approved);

    let thread = GetInquiryThreadTool::call(
        ctx(&pg, owner.clone(), planner_self),
        proxima_core::GetInquiryThreadArgs {
            thread_key: Some("meaningful-planning-round".into()),
            anchor: None,
            limit: None,
        },
    )
    .await?;
    assert_eq!(thread.thread_key, "meaningful-planning-round");
    assert_eq!(thread.questions.len(), 1);
    assert_eq!(thread.answers.len(), 1);
    assert_eq!(thread.approval_policies.len(), 1);
    assert_eq!(thread.approval_votes.len(), 1);
    assert_eq!(thread.approval_decisions.len(), 1);
    assert!(thread.questions[0].question.contains("required tests"));
    assert!(thread.answers[0].answer.contains("T1"));
    assert_eq!(thread.approval_policies[0].handle, policy_handle);
    assert_eq!(thread.approval_votes[0].handle, vote.handle);
    assert_eq!(
        thread.approval_decisions[0].decision,
        ApprovalDecision::Approved
    );
    assert_eq!(thread.open_items.unanswered_questions.len(), 0);
    assert_eq!(thread.open_items.undecided_policies.len(), 0);

    for relation in [
        "core/receives-directed-question",
        "core/answers-question",
        "core/has-approval-policy",
        "core/votes-on",
        "core/has-approval-decision",
        "core/derived-from",
    ] {
        assert!(
            thread.edges.iter().any(|edge| edge.relation == relation),
            "missing relation {relation}"
        );
    }

    let by_answer_anchor = GetInquiryThreadTool::call(
        ctx(&pg, owner.clone(), planner_self),
        proxima_core::GetInquiryThreadArgs {
            thread_key: None,
            anchor: Some(answer.handle.clone()),
            limit: None,
        },
    )
    .await?;
    assert_eq!(by_answer_anchor.thread_key, thread.thread_key);
    let by_decision_anchor = GetInquiryThreadTool::call(
        ctx(&pg, owner.clone(), planner_self),
        proxima_core::GetInquiryThreadArgs {
            thread_key: None,
            anchor: Some(decision_handle),
            limit: None,
        },
    )
    .await?;
    assert_eq!(by_decision_anchor.thread_key, thread.thread_key);

    let open_question = EmitDirectedQuestionTool::call(
        ctx(&pg, owner.clone(), planner_self),
        proxima_core::EmitDirectedQuestionArgs {
            target_personality: tester.instance_id.into_inner().to_string(),
            thread_key: "open-planning-round".into(),
            question: "Which acceptance item is still open?".into(),
            parent_question: None,
            context_memories: Vec::new(),
            context_goals: Vec::new(),
            idempotency_key: "open-planning-question".into(),
        },
    )
    .await?;
    let open_policy = EmitApprovalPolicyTool::call(
        ctx(&pg, owner.clone(), planner_self),
        proxima_core::EmitApprovalPolicyArgs {
            target: open_question.handle.clone(),
            target_kind: ApprovalTargetKind::Fact,
            title: "Approve open planning question".into(),
            summary: "This policy intentionally has no decision yet.".into(),
            eligible_voters: vec![ApprovalEligibleVoter {
                voter_key: "decider".into(),
                kind: ApprovalVoterKind::Personality,
                role: Some("decider".into()),
                personality_instance_id: Some(decider.instance_id.into_inner()),
                self_perspective_memory_id: Some(decider_self.into_inner()),
            }],
            requirements: vec![ApprovalRequirement {
                kind: ApprovalRequirementKind::AllOfVoters,
                voter_keys: vec!["decider".into()],
                role: None,
                min_approvals: None,
            }],
            idempotency_key: "open-planning-policy".into(),
        },
    )
    .await?;
    let open_thread = GetInquiryThreadTool::call(
        ctx(&pg, owner.clone(), planner_self),
        proxima_core::GetInquiryThreadArgs {
            thread_key: Some("open-planning-round".into()),
            anchor: None,
            limit: None,
        },
    )
    .await?;
    assert_eq!(
        open_thread.open_items.unanswered_questions,
        vec![open_question.handle]
    );
    assert_eq!(
        open_thread.open_items.undecided_policies,
        vec![open_policy.handle]
    );
    assert!(
        open_thread
            .edges
            .iter()
            .any(|edge| edge.relation == "core/has-approval-policy")
    );
    assert_eq!(question_handle, thread.questions[0].handle);

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

fn wake(
    personality_instance_id: proxima_core::PersonalityInstanceId,
    trigger_id: &str,
    label: &str,
    substrate_tool_palette: Vec<String>,
) -> WakeEntryDraft {
    WakeEntryDraft::new(
        Uuid::now_v7(),
        personality_instance_id,
        WakeEntryTriggerKind::OnMemory,
        trigger_id,
        label,
        WakeEntryAuthoredBy::Any,
        1000,
        ModelTier::Standard,
        None,
        substrate_tool_palette,
        4,
    )
    .expect("wake draft")
}

fn wake_row(draft: &WakeEntryDraft) -> WakeEntryRow {
    WakeEntryRow {
        wake_entry_id: draft.wake_entry_id,
        trigger_kind: draft.trigger_kind,
        trigger_id: draft.trigger_id.clone(),
        label: draft.label.clone(),
        enabled: draft.enabled,
        execution_mode: WakeEntryExecutionMode::SubstrateOnly,
        authored_by: draft.authored_by,
        probability_promille: draft.probability_promille,
        goal_scope: draft.goal_scope,
        instructions: draft.instructions.clone(),
        model_tier: draft.model_tier,
        inference_target_ref: draft.inference_target_ref.clone(),
        substrate_tool_palette: draft.substrate_tool_palette.clone(),
        workspace_tool_palette: draft.workspace_tool_palette.clone(),
        max_rounds: draft.max_rounds,
        intervention_policy: draft.intervention_policy.clone(),
        disabled_reason: None,
    }
}

fn self_perspective(
    rows: &[proxima_core::PersonalityInstanceRow],
    instance_id: proxima_core::PersonalityInstanceId,
) -> MemoryId {
    rows.iter()
        .find(|row| row.personality_instance_id == instance_id)
        .expect("personality row")
        .current_root_perspective_memory_id
}

fn ctx(
    pg: &proxima_storage_pg::PgStorage,
    owner: proxima_core::Owner,
    caller_self_perspective: MemoryId,
) -> McpToolCtx {
    let registry = Arc::new(FlavorRegistry::new().freeze());
    let engine = engine(pg, owner.clone());
    McpToolCtx {
        pool: pg.pool().clone(),
        owner,
        handles: None,
        mode: OutputMode::RawIds,
        registry,
        author: McpAuthorContext {
            model_id: "test/model".into(),
            client_name: "test".into(),
            client_version: "1".into(),
            caller_self_perspective: Some(caller_self_perspective),
        },
        caller_self_perspective: Some(caller_self_perspective),
        master_token_id: None,
        engine: Some(Arc::new(engine)),
    }
}

fn engine(pg: &proxima_storage_pg::PgStorage, owner: proxima_core::Owner) -> Engine {
    Engine::new(
        FlavorRegistry::new().freeze(),
        MemoryStore::new(),
        Box::new(NoAuth::new(owner.principal.clone(), owner)),
    )
    .with_storage(pg.clone().into_handle())
}

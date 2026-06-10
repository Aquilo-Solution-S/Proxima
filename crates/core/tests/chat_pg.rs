mod chat_support;
mod common;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::{
    ApprovalDecision, ApprovalEligibleVoter, ApprovalRequirement, ApprovalRequirementKind,
    ApprovalTargetKind, ApprovalVoteVerdict, ApprovalVoterKind, CHAT_MESSAGE_SCHEMA_ID,
    EmitApprovalPolicyTool, EmitApprovalVoteTool, EmitChatMessageTool, EmitChatReplyTool,
    GetChatThreadTool, ListChatTargetsTool, McpTool, RelationClass, SetWakeEntriesRequest,
    StartChatTool, Storage, TryEmitApprovalDecisionOutput, TryEmitApprovalDecisionTool,
    build_wake_coordination_context,
};
use uuid::Uuid;

use chat_support::*;
#[tokio::test]
async fn start_chat_emits_started_fact_and_first_message() -> Result<(), Box<dyn std::error::Error>>
{
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let shell = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Shell".into(),
            purpose: "Start chats".into(),
        })
        .await?;
    let mira = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Mira".into(),
            purpose: "Receive chat messages".into(),
        })
        .await?;
    let rows = pg.list_personality_instances(&owner, false).await?;
    let shell_self = self_perspective(&rows, shell.instance_id);

    pg.set_wake_entries(&SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id: mira.instance_id,
        entries: vec![wake(
            mira.instance_id,
            CHAT_MESSAGE_SCHEMA_ID,
            "receive-chat-message",
            vec!["core/emit_chat_reply".into()],
        )],
    })
    .await?;

    let started = StartChatTool::call(
        ctx(&pg, owner.clone(), shell_self),
        proxima_core::StartChatArgs {
            target_personality: mira.instance_id.into_inner().to_string(),
            thread_key: Some("start-chat-first-step".into()),
            title: Some("First chat".into()),
            message: "Start with the smallest real chat surface.".into(),
            context_memories: Vec::new(),
            context_goals: Vec::new(),
            idempotency_key: "start-chat-first-step".into(),
        },
    )
    .await?;
    assert_eq!(started.thread_key, "start-chat-first-step");
    assert!(started.target_edge_handle.is_some());

    let started_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.chat_started_v1")
        .fetch_one(pg.pool())
        .await?;
    let message_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.chat_message_v1")
        .fetch_one(pg.pool())
        .await?;
    assert_eq!(started_rows, 1);
    assert_eq!(message_rows, 1);

    let thread = GetChatThreadTool::call(
        ctx(&pg, owner.clone(), shell_self),
        proxima_core::GetChatThreadArgs {
            thread_key: Some("start-chat-first-step".into()),
            anchor: None,
            limit: None,
        },
    )
    .await?;
    let thread_started = thread.started.expect("started projection");
    assert_eq!(thread_started.handle, started.started);
    assert_eq!(thread_started.title.as_deref(), Some("First chat"));
    assert_eq!(thread.messages.len(), 1);
    assert_eq!(thread.messages[0].handle, started.message);
    assert_eq!(thread.replies.len(), 0);
    assert_eq!(
        thread.open_items.unreplied_messages,
        vec![started.message.clone()]
    );

    let thread_from_matching_anchor = GetChatThreadTool::call(
        ctx(&pg, owner.clone(), shell_self),
        proxima_core::GetChatThreadArgs {
            thread_key: Some("start-chat-first-step".into()),
            anchor: Some(started.message.clone()),
            limit: None,
        },
    )
    .await?;
    assert_eq!(
        thread_from_matching_anchor
            .started
            .expect("started projection from matching anchor")
            .handle,
        started.started
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "linear chat e2e fixture")]
async fn chat_round_trip_and_coordination_context() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let planner = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Planner".into(),
            purpose: "Ask implementation messages".into(),
        })
        .await?;
    let responder = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Testing Engineer".into(),
            purpose: "Reply test-design messages".into(),
        })
        .await?;
    let rows = pg.list_personality_instances(&owner, false).await?;
    let planner_self = rows
        .iter()
        .find(|row| row.personality_instance_id == planner.instance_id)
        .expect("planner row")
        .current_root_perspective_memory_id;
    let responder_self = rows
        .iter()
        .find(|row| row.personality_instance_id == responder.instance_id)
        .expect("responder row")
        .current_root_perspective_memory_id;

    let planner_wake = wake(
        planner.instance_id,
        "core/goal-activated-v1",
        "planner",
        vec!["core/emit_chat_message".into()],
    );
    let responder_wake = wake(
        responder.instance_id,
        CHAT_MESSAGE_SCHEMA_ID,
        "chat-message",
        vec!["core/emit_chat_reply".into()],
    );
    pg.set_wake_entries(&SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id: planner.instance_id,
        entries: vec![planner_wake.clone()],
    })
    .await?;
    pg.set_wake_entries(&SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id: responder.instance_id,
        entries: vec![responder_wake],
    })
    .await?;

    let planner_ctx = ctx(&pg, owner.clone(), planner_self);
    let targets = ListChatTargetsTool::call(
        planner_ctx.clone(),
        proxima_core::ListChatTargetsArgs {
            include_self: false,
        },
    )
    .await?;
    assert_eq!(targets.targets.len(), 1);
    assert_eq!(targets.targets[0].display_name, "Testing Engineer");

    let message = EmitChatMessageTool::call(
        planner_ctx.clone(),
        proxima_core::EmitChatMessageArgs {
            target_personality: responder.instance_id.into_inner().to_string(),
            thread_key: "planning-discussion".into(),
            message: "Which tests do you require?".into(),
            parent: None,
            context_memories: Vec::new(),
            context_goals: Vec::new(),
            idempotency_key: "message-1".into(),
        },
    )
    .await?;
    assert!(message.target_edge_handle.is_some());

    let wrong_reply = EmitChatReplyTool::call(
        planner_ctx,
        proxima_core::EmitChatReplyArgs {
            reply_to: message.handle.clone(),
            reply: "wrong caller".into(),
            context_memories_used: Vec::new(),
            idempotency_key: "reply-wrong".into(),
        },
    )
    .await
    .expect_err("planner must not reply responder's message");
    assert!(wrong_reply.to_string().contains("addressed target"));

    let reply = EmitChatReplyTool::call(
        ctx(&pg, owner.clone(), responder_self),
        proxima_core::EmitChatReplyArgs {
            reply_to: message.handle,
            reply: "I require a target-discovery test and a wrong-self rejection test.".into(),
            context_memories_used: Vec::new(),
            idempotency_key: "reply-1".into(),
        },
    )
    .await?;
    assert!(reply.reply_edge_handle.is_some());

    let message_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.chat_message_v1")
        .fetch_one(pg.pool())
        .await?;
    let reply_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.chat_reply_v1")
        .fetch_one(pg.pool())
        .await?;
    assert_eq!(message_rows, 1);
    assert_eq!(reply_rows, 1);

    let relation_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.edges
          WHERE relation IN ('core/receives-chat-message', 'core/replies-to-message')",
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
    assert_eq!(coordination.chat_targets.len(), 1);
    assert_eq!(
        coordination.chat_targets[0].personality_instance_id,
        responder.instance_id.into_inner()
    );
    assert!(
        coordination
            .wake_path
            .downstream
            .iter()
            .any(|node| node.personality_instance_id == responder.instance_id.into_inner())
    );

    drop(engine);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn chat_message_requires_target_wake_entry() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let planner = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Planner".into(),
            purpose: "Ask messages".into(),
        })
        .await?;
    let silent = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Silent".into(),
            purpose: "No chat wake".into(),
        })
        .await?;
    let planner_self = pg
        .list_personality_instances(&owner, false)
        .await?
        .into_iter()
        .find(|row| row.personality_instance_id == planner.instance_id)
        .expect("planner row")
        .current_root_perspective_memory_id;
    let err = EmitChatMessageTool::call(
        ctx(&pg, owner, planner_self),
        proxima_core::EmitChatMessageArgs {
            target_personality: silent.instance_id.into_inner().to_string(),
            thread_key: "missing-wake".into(),
            message: "Can you reply?".into(),
            parent: None,
            context_memories: Vec::new(),
            context_goals: Vec::new(),
            idempotency_key: "missing-wake-message".into(),
        },
    )
    .await
    .expect_err("target without chat-message wake must be rejected");
    assert!(err.to_string().contains("chat-message wake entry"));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "linear chat e2e fixture")]
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
                "core/emit_chat_message".into(),
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
            CHAT_MESSAGE_SCHEMA_ID,
            "reply-planning-message",
            vec!["core/emit_chat_reply".into()],
        )],
    })
    .await?;

    let message = EmitChatMessageTool::call(
        ctx(&pg, owner.clone(), planner_self),
        proxima_core::EmitChatMessageArgs {
            target_personality: tester.instance_id.into_inner().to_string(),
            thread_key: "meaningful-planning-round".into(),
            message: "Propose the required tests T1..Tn before implementation.".into(),
            parent: None,
            context_memories: Vec::new(),
            context_goals: Vec::new(),
            idempotency_key: "planning-round-message".into(),
        },
    )
    .await?;
    let message_handle = message.handle.clone();
    let reply = EmitChatReplyTool::call(
        ctx(&pg, owner.clone(), tester_self),
        proxima_core::EmitChatReplyArgs {
            reply_to: message.handle.clone(),
            reply: "T1: target discovery lists the tester. T2: wrong self cannot reply. T3: the final plan reply is approved by the decider.".into(),
            context_memories_used: Vec::new(),
            idempotency_key: "planning-round-reply".into(),
        },
    )
    .await?;
    let policy = EmitApprovalPolicyTool::call(
        ctx(&pg, owner.clone(), planner_self),
        proxima_core::EmitApprovalPolicyArgs {
            target: reply.handle.clone(),
            target_kind: ApprovalTargetKind::Fact,
            title: "Approve planning reply".into(),
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

    let thread = GetChatThreadTool::call(
        ctx(&pg, owner.clone(), planner_self),
        proxima_core::GetChatThreadArgs {
            thread_key: Some("meaningful-planning-round".into()),
            anchor: None,
            limit: None,
        },
    )
    .await?;
    assert_eq!(thread.thread_key, "meaningful-planning-round");
    assert_eq!(thread.messages.len(), 1);
    assert_eq!(thread.replies.len(), 1);
    assert_eq!(thread.approval_policies.len(), 1);
    assert_eq!(thread.approval_votes.len(), 1);
    assert_eq!(thread.approval_decisions.len(), 1);
    assert!(thread.messages[0].message.contains("required tests"));
    assert!(thread.replies[0].reply.contains("T1"));
    assert_eq!(thread.approval_policies[0].handle, policy_handle);
    assert_eq!(thread.approval_votes[0].handle, vote.handle);
    assert_eq!(
        thread.approval_decisions[0].decision,
        ApprovalDecision::Approved
    );
    assert_eq!(thread.open_items.unreplied_messages.len(), 0);
    assert_eq!(thread.open_items.undecided_policies.len(), 0);

    for relation in [
        "core/receives-chat-message",
        "core/replies-to-message",
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

    let by_reply_anchor = GetChatThreadTool::call(
        ctx(&pg, owner.clone(), planner_self),
        proxima_core::GetChatThreadArgs {
            thread_key: None,
            anchor: Some(reply.handle.clone()),
            limit: None,
        },
    )
    .await?;
    assert_eq!(by_reply_anchor.thread_key, thread.thread_key);
    let by_decision_anchor = GetChatThreadTool::call(
        ctx(&pg, owner.clone(), planner_self),
        proxima_core::GetChatThreadArgs {
            thread_key: None,
            anchor: Some(decision_handle),
            limit: None,
        },
    )
    .await?;
    assert_eq!(by_decision_anchor.thread_key, thread.thread_key);

    let open_message = EmitChatMessageTool::call(
        ctx(&pg, owner.clone(), planner_self),
        proxima_core::EmitChatMessageArgs {
            target_personality: tester.instance_id.into_inner().to_string(),
            thread_key: "open-planning-round".into(),
            message: "Which acceptance item is still open?".into(),
            parent: None,
            context_memories: Vec::new(),
            context_goals: Vec::new(),
            idempotency_key: "open-planning-message".into(),
        },
    )
    .await?;
    let open_policy = EmitApprovalPolicyTool::call(
        ctx(&pg, owner.clone(), planner_self),
        proxima_core::EmitApprovalPolicyArgs {
            target: open_message.handle.clone(),
            target_kind: ApprovalTargetKind::Fact,
            title: "Approve open planning message".into(),
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
    let open_thread = GetChatThreadTool::call(
        ctx(&pg, owner.clone(), planner_self),
        proxima_core::GetChatThreadArgs {
            thread_key: Some("open-planning-round".into()),
            anchor: None,
            limit: None,
        },
    )
    .await?;
    assert_eq!(
        open_thread.open_items.unreplied_messages,
        vec![open_message.handle]
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
    assert_eq!(message_handle, thread.messages[0].handle);

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "linear chat e2e fixture")]
async fn chat_lifecycle_compacts_continues_and_summarizes_without_llm()
-> Result<(), Box<dyn std::error::Error>> {
    // This is deliberately no-LLM: chat turns use the real
    // chat MCP tools, while compaction/final summary are inserted as
    // test Abstractions until a production chat-compaction tool exists.
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let shell = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Shell".into(),
            purpose: "Open and drive graph-native chat".into(),
        })
        .await?;
    let mira = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Mira".into(),
            purpose: "Reply chat messages without autonomous action".into(),
        })
        .await?;
    let rows = pg.list_personality_instances(&owner, false).await?;
    let shell_self = self_perspective(&rows, shell.instance_id);
    let mira_self = self_perspective(&rows, mira.instance_id);

    pg.set_wake_entries(&SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id: mira.instance_id,
        entries: vec![wake(
            mira.instance_id,
            CHAT_MESSAGE_SCHEMA_ID,
            "reply-chat-message",
            vec!["core/emit_chat_reply".into()],
        )],
    })
    .await?;

    let m1 = EmitChatMessageTool::call(
        ctx(&pg, owner.clone(), shell_self),
        proxima_core::EmitChatMessageArgs {
            target_personality: mira.instance_id.into_inner().to_string(),
            thread_key: "mock-chat-lifecycle".into(),
            message: "Open chat: prove projection before wake wiring.".into(),
            parent: None,
            context_memories: Vec::new(),
            context_goals: Vec::new(),
            idempotency_key: "chat-open-m1".into(),
        },
    )
    .await?;
    let r1 = EmitChatReplyTool::call(
        ctx(&pg, owner.clone(), mira_self),
        proxima_core::EmitChatReplyArgs {
            reply_to: m1.handle.clone(),
            reply: "Projection and compaction should be proven with mocked behavior first.".into(),
            context_memories_used: Vec::new(),
            idempotency_key: "chat-open-r1".into(),
        },
    )
    .await?;

    let compaction = insert_test_abstraction(
        &pg,
        &owner,
        mira.instance_id,
        "test/chat-thread-compaction-v1",
        "Summary: chat opened; projection before wake wiring. Decision: compaction is an Abstraction.",
    )
    .await?;
    insert_memory_edge(
        &pg,
        &owner,
        compaction,
        Uuid::parse_str(&m1.handle)?,
        "core/derived-from",
        RelationClass::Provenance,
        "Abstraction",
        "Fact",
    )
    .await?;
    insert_memory_edge(
        &pg,
        &owner,
        compaction,
        Uuid::parse_str(&r1.handle)?,
        "core/derived-from",
        RelationClass::Provenance,
        "Abstraction",
        "Fact",
    )
    .await?;

    let m2 = EmitChatMessageTool::call(
        ctx(&pg, owner.clone(), shell_self),
        proxima_core::EmitChatMessageArgs {
            target_personality: mira.instance_id.into_inner().to_string(),
            thread_key: "mock-chat-lifecycle".into(),
            message: "Continue chat after compaction using exact new turns.".into(),
            parent: Some(r1.handle.clone()),
            context_memories: vec![compaction.to_string()],
            context_goals: Vec::new(),
            idempotency_key: "chat-continue-m2".into(),
        },
    )
    .await?;
    let r2 = EmitChatReplyTool::call(
        ctx(&pg, owner.clone(), mira_self),
        proxima_core::EmitChatReplyArgs {
            reply_to: m2.handle.clone(),
            reply: "Continuation sees the compaction handle and the new exact message.".into(),
            context_memories_used: vec![compaction.to_string()],
            idempotency_key: "chat-continue-r2".into(),
        },
    )
    .await?;

    let close = EmitChatMessageTool::call(
        ctx(&pg, owner.clone(), shell_self),
        proxima_core::EmitChatMessageArgs {
            target_personality: mira.instance_id.into_inner().to_string(),
            thread_key: "mock-chat-lifecycle".into(),
            message: "Close chat and produce a durable final summary.".into(),
            parent: Some(r2.handle.clone()),
            context_memories: vec![compaction.to_string()],
            context_goals: Vec::new(),
            idempotency_key: "chat-close-m3".into(),
        },
    )
    .await?;
    let close_reply = EmitChatReplyTool::call(
        ctx(&pg, owner.clone(), mira_self),
        proxima_core::EmitChatReplyArgs {
            reply_to: close.handle.clone(),
            reply: "Closing chat. Final summary should cover compaction and post-compaction facts."
                .into(),
            context_memories_used: vec![compaction.to_string()],
            idempotency_key: "chat-close-r3".into(),
        },
    )
    .await?;

    let final_summary = insert_test_abstraction(
        &pg,
        &owner,
        mira.instance_id,
        "test/chat-thread-final-summary-v1",
        "Final summary: thread opened, compacted, continued, and closed without LLM execution.",
    )
    .await?;
    for target in [
        compaction,
        Uuid::parse_str(&m2.handle)?,
        Uuid::parse_str(&r2.handle)?,
        Uuid::parse_str(&close.handle)?,
        Uuid::parse_str(&close_reply.handle)?,
    ] {
        insert_memory_edge(
            &pg,
            &owner,
            final_summary,
            target,
            "core/derived-from",
            RelationClass::Provenance,
            "Abstraction",
            if target == compaction {
                "Abstraction"
            } else {
                "Fact"
            },
        )
        .await?;
    }

    let thread = GetChatThreadTool::call(
        ctx(&pg, owner.clone(), shell_self),
        proxima_core::GetChatThreadArgs {
            thread_key: Some("mock-chat-lifecycle".into()),
            anchor: None,
            limit: None,
        },
    )
    .await?;
    assert_eq!(thread.messages.len(), 3);
    assert_eq!(thread.replies.len(), 3);
    assert_eq!(thread.open_items.unreplied_messages.len(), 0);
    assert_eq!(
        thread.messages[1].context_memories,
        vec![compaction.to_string()]
    );
    assert_eq!(
        thread.messages[1].parent.as_deref(),
        Some(r1.handle.as_str())
    );
    assert_eq!(
        thread.messages[2].parent.as_deref(),
        Some(r2.handle.as_str())
    );
    assert_eq!(
        thread.replies[1].context_memories_used,
        vec![compaction.to_string()]
    );

    let derived_edges: i64 = sqlx::query_scalar(
        "SELECT count(*) \
           FROM proxima_core.edges \
          WHERE relation = 'core/derived-from' \
            AND source_memory_id IN ($1, $2)",
    )
    .bind(compaction)
    .bind(final_summary)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(
        derived_edges, 7,
        "compaction and final summary must cover explicit source handles"
    );

    let summary_text: String =
        sqlx::query_scalar("SELECT text FROM proxima_core.memories WHERE memory_id = $1")
            .bind(final_summary)
            .fetch_one(pg.pool())
            .await?;
    assert!(summary_text.contains("thread opened"));
    assert!(summary_text.contains("continued"));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

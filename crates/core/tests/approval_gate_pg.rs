mod common;

use std::sync::Arc;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::mcp::McpAuthorContext;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, GoalState};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{
    ApprovalDecision, ApprovalEligibleVoter, ApprovalRequirement, ApprovalRequirementKind,
    ApprovalTargetKind, ApprovalVoteVerdict, ApprovalVoterKind, AuthPath, AuthzContext,
    EmitApprovalPolicyTool, EmitApprovalVoteTool, Engine, FlavorRegistry, McpTool, McpToolCtx,
    MemoryId, OrgId, OutputMode, Owner, OwnerPrincipalKind, Principal, SchemaId, SchemaVersion,
    SourceBatchId, SourceId, Storage, TryEmitApprovalDecisionOutput, TryEmitApprovalDecisionTool,
    UserId,
};
use uuid::Uuid;

#[tokio::test]
async fn approval_gate_shell_author_approves_fact() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let self_memory = insert_abstraction(&pg, &owner).await?;
    let fact = insert_fact(&pg, &owner, "gated fact").await?;
    let ctx = ctx(
        &pg,
        owner.clone(),
        Some(MemoryId::new(self_memory)),
        Some(Uuid::now_v7()),
    );

    let policy = EmitApprovalPolicyTool::call(
        ctx.clone(),
        proxima_core::EmitApprovalPolicyArgs {
            target: fact.to_string(),
            target_kind: ApprovalTargetKind::Fact,
            title: "Fact gate".into(),
            summary: "Require user approval".into(),
            eligible_voters: vec![shell_voter("user", "owner")],
            requirements: vec![ApprovalRequirement {
                kind: ApprovalRequirementKind::AllOfVoters,
                voter_keys: vec!["user".into()],
                role: None,
                min_approvals: None,
            }],
            idempotency_key: "policy-fact".into(),
        },
    )
    .await?;
    assert!(!policy.handle.is_empty());

    let vote = EmitApprovalVoteTool::call(
        ctx.clone(),
        proxima_core::EmitApprovalVoteArgs {
            policy: policy.handle.clone(),
            voter_key: "user".into(),
            verdict: ApprovalVoteVerdict::Approved,
            rationale: "looks good".into(),
            idempotency_key: "vote-user".into(),
        },
    )
    .await?;
    assert!(!vote.handle.is_empty());

    let decision = TryEmitApprovalDecisionTool::call(
        ctx.clone(),
        proxima_core::TryEmitApprovalDecisionArgs {
            policy: policy.handle,
            idempotency_key: "decision-user".into(),
        },
    )
    .await?;
    let TryEmitApprovalDecisionOutput::Written {
        decision,
        edge_handles,
        ..
    } = decision
    else {
        panic!("expected written decision");
    };
    assert_eq!(decision, ApprovalDecision::Approved);
    assert_eq!(edge_handles.len(), 3);
    let sidecars: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.approval_decision_v1")
            .fetch_one(pg.pool())
            .await?;
    assert_eq!(sidecars, 1);

    drop(ctx);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn approval_gate_supports_abstraction_and_goal_targets()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let self_memory = insert_abstraction(&pg, &owner).await?;
    let ctx = ctx(
        &pg,
        owner.clone(),
        Some(MemoryId::new(self_memory)),
        Some(Uuid::now_v7()),
    );
    let abstraction = insert_abstraction(&pg, &owner).await?;
    let goal = pg
        .write_goal_atomic(&GoalDraft {
            owner: owner.clone(),
            schema_id: SchemaId::new("test/approval-goal".into()),
            schema_version: SchemaVersion::new(1),
            title: "Goal".into(),
            text: "Goal text".into(),
            payload: Vec::new(),
            state: GoalState::Active,
            parent_goal_ids: Vec::new(),
            supersedes_goal_id: None,
            authorship: GoalAuthorship::User,
            request_id: "approval-goal".into(),
        })
        .await?
        .goal_id
        .into_inner();

    for (target, target_kind) in [
        (abstraction.to_string(), ApprovalTargetKind::Abstraction),
        (goal.to_string(), ApprovalTargetKind::Goal),
    ] {
        let policy = EmitApprovalPolicyTool::call(
            ctx.clone(),
            proxima_core::EmitApprovalPolicyArgs {
                target,
                target_kind,
                title: "Gate".into(),
                summary: "Require review".into(),
                eligible_voters: vec![shell_voter("user", "owner")],
                requirements: vec![ApprovalRequirement {
                    kind: ApprovalRequirementKind::AllOfVoters,
                    voter_keys: vec!["user".into()],
                    role: None,
                    min_approvals: None,
                }],
                idempotency_key: format!("policy-{}", target_kind.as_str()),
            },
        )
        .await?;
        assert!(policy.target_edge_handle.is_some());
    }

    let edges: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.edges
          WHERE relation = 'core/has-approval-policy'",
    )
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(edges, 2);

    drop(ctx);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn approval_decision_waits_for_quorum_and_blocks_on_changes()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let self_memory = insert_abstraction(&pg, &owner).await?;
    let fact = insert_fact(&pg, &owner, "role quorum").await?;
    let ctx = ctx(
        &pg,
        owner.clone(),
        Some(MemoryId::new(self_memory)),
        Some(Uuid::now_v7()),
    );
    let policy = EmitApprovalPolicyTool::call(
        ctx.clone(),
        proxima_core::EmitApprovalPolicyArgs {
            target: fact.to_string(),
            target_kind: ApprovalTargetKind::Fact,
            title: "Role gate".into(),
            summary: "Require two reviewers".into(),
            eligible_voters: vec![shell_voter("a", "reviewer"), shell_voter("b", "reviewer")],
            requirements: vec![ApprovalRequirement {
                kind: ApprovalRequirementKind::RoleQuorum,
                voter_keys: Vec::new(),
                role: Some("reviewer".into()),
                min_approvals: Some(2),
            }],
            idempotency_key: "policy-quorum".into(),
        },
    )
    .await?;

    EmitApprovalVoteTool::call(
        ctx.clone(),
        proxima_core::EmitApprovalVoteArgs {
            policy: policy.handle.clone(),
            voter_key: "a".into(),
            verdict: ApprovalVoteVerdict::Approved,
            rationale: "a ok".into(),
            idempotency_key: "vote-a".into(),
        },
    )
    .await?;
    let not_ready = TryEmitApprovalDecisionTool::call(
        ctx.clone(),
        proxima_core::TryEmitApprovalDecisionArgs {
            policy: policy.handle.clone(),
            idempotency_key: "decision-not-ready".into(),
        },
    )
    .await?;
    assert!(matches!(
        not_ready,
        TryEmitApprovalDecisionOutput::NotReady { .. }
    ));

    EmitApprovalVoteTool::call(
        ctx.clone(),
        proxima_core::EmitApprovalVoteArgs {
            policy: policy.handle.clone(),
            voter_key: "b".into(),
            verdict: ApprovalVoteVerdict::RequestChanges,
            rationale: "needs work".into(),
            idempotency_key: "vote-b".into(),
        },
    )
    .await?;
    let blocked = TryEmitApprovalDecisionTool::call(
        ctx,
        proxima_core::TryEmitApprovalDecisionArgs {
            policy: policy.handle,
            idempotency_key: "decision-blocked".into(),
        },
    )
    .await?;
    let TryEmitApprovalDecisionOutput::Written { decision, .. } = blocked else {
        panic!("expected blocked decision");
    };
    assert_eq!(decision, ApprovalDecision::Blocked);

    // ctx was moved into the final decision call.
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn personality_vote_requires_matching_self_perspective()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let personality = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Reviewer".into(),
            purpose: "Vote on approval gates".into(),
        })
        .await?;
    let personality_self = pg
        .list_personality_instances(&owner, false)
        .await?
        .into_iter()
        .find(|row| row.personality_instance_id == personality.instance_id)
        .expect("personality row")
        .current_root_perspective_memory_id;
    let fact = insert_fact(&pg, &owner, "personality gated").await?;
    let shell_self = insert_abstraction(&pg, &owner).await?;
    let shell_ctx = ctx(
        &pg,
        owner.clone(),
        Some(MemoryId::new(shell_self)),
        Some(Uuid::now_v7()),
    );
    let policy = EmitApprovalPolicyTool::call(
        shell_ctx,
        proxima_core::EmitApprovalPolicyArgs {
            target: fact.to_string(),
            target_kind: ApprovalTargetKind::Fact,
            title: "Personality gate".into(),
            summary: "Require personality".into(),
            eligible_voters: vec![ApprovalEligibleVoter {
                voter_key: "reviewer".into(),
                kind: ApprovalVoterKind::Personality,
                role: Some("reviewer".into()),
                personality_instance_id: Some(personality.instance_id.into_inner()),
                self_perspective_memory_id: Some(personality_self.into_inner()),
            }],
            requirements: vec![ApprovalRequirement {
                kind: ApprovalRequirementKind::AllOfVoters,
                voter_keys: vec!["reviewer".into()],
                role: None,
                min_approvals: None,
            }],
            idempotency_key: "policy-personality".into(),
        },
    )
    .await?;

    let wrong_ctx = ctx(&pg, owner.clone(), Some(MemoryId::new(shell_self)), None);
    let err = EmitApprovalVoteTool::call(
        wrong_ctx,
        proxima_core::EmitApprovalVoteArgs {
            policy: policy.handle.clone(),
            voter_key: "reviewer".into(),
            verdict: ApprovalVoteVerdict::Approved,
            rationale: "wrong self".into(),
            idempotency_key: "vote-wrong".into(),
        },
    )
    .await
    .expect_err("wrong self should be rejected");
    assert!(err.to_string().contains("does not match"));

    let reviewer_ctx = ctx(&pg, owner.clone(), Some(personality_self), None);
    let vote = EmitApprovalVoteTool::call(
        reviewer_ctx,
        proxima_core::EmitApprovalVoteArgs {
            policy: policy.handle,
            voter_key: "reviewer".into(),
            verdict: ApprovalVoteVerdict::Approved,
            rationale: "approved".into(),
            idempotency_key: "vote-reviewer".into(),
        },
    )
    .await?;
    assert!(!vote.handle.is_empty());

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn approval_policy_rejects_cross_owner_target() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let other_owner = Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::nil()),
    };
    let fact = insert_fact(&pg, &other_owner, "other owner fact").await?;
    let self_memory = insert_abstraction(&pg, &owner).await?;
    let ctx = ctx(
        &pg,
        owner,
        Some(MemoryId::new(self_memory)),
        Some(Uuid::now_v7()),
    );

    let err = EmitApprovalPolicyTool::call(
        ctx,
        proxima_core::EmitApprovalPolicyArgs {
            target: fact.to_string(),
            target_kind: ApprovalTargetKind::Fact,
            title: "Cross owner".into(),
            summary: "should fail".into(),
            eligible_voters: vec![shell_voter("user", "owner")],
            requirements: vec![ApprovalRequirement {
                kind: ApprovalRequirementKind::AllOfVoters,
                voter_keys: vec!["user".into()],
                role: None,
                min_approvals: None,
            }],
            idempotency_key: "policy-cross-owner".into(),
        },
    )
    .await
    .expect_err("cross-owner target should be rejected");
    assert!(err.to_string().contains("not visible"));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

fn shell_voter(voter_key: &str, role: &str) -> ApprovalEligibleVoter {
    ApprovalEligibleVoter {
        voter_key: voter_key.into(),
        kind: ApprovalVoterKind::ShellAuthor,
        role: Some(role.into()),
        personality_instance_id: None,
        self_perspective_memory_id: None,
    }
}

fn ctx(
    pg: &proxima_storage_pg::PgStorage,
    owner: Owner,
    caller_self_perspective: Option<MemoryId>,
    master_token_id: Option<Uuid>,
) -> McpToolCtx {
    let registry = Arc::new(FlavorRegistry::new().freeze());
    let engine =
        Engine::new((*registry).clone(), MemoryStore::new()).with_storage(pg.clone().into_handle());
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    McpToolCtx {
        pool: pg.pool().clone(),
        owner,
        authz,
        handles: None,
        mode: OutputMode::RawIds,
        registry,
        author: McpAuthorContext {
            model_id: "test/model".into(),
            client_name: "test".into(),
            client_version: "1".into(),
            caller_self_perspective,
        },
        caller_self_perspective,
        master_token_id,
        engine: Some(Arc::new(engine)),
    }
}

async fn insert_fact(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    label: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let now = time::OffsetDateTime::now_utc();
    let payload = serde_json::to_vec(&serde_json::json!({ "label": label }))?;
    let draft = EventDraft {
        source_id: SourceId::new("test/approval"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: owner.clone(),
        schema_id: SchemaId::new("test/approval-fact-v1".into()),
        schema_version: SchemaVersion::new(1),
        payload,
        observed_at: now,
        occurred_at: now,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new("test/approval-object-v1".into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *blake3::hash(label.as_bytes()).as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new("test/approval-whole-v1".into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    Ok(pg.ingest_event_atomic(&draft).await?.memory_id.into_inner())
}

async fn insert_abstraction(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let owner_kind = OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id)
         VALUES ($1, $2, $3, $4, 'test/approval-abstraction-v1', 1,
                 'Abstraction', 'approval abstraction', 'Wake', 'test-model',
                 'test-v1', '00000000-0000-0000-0000-000000000000'::uuid)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .execute(pg.pool())
    .await?;
    Ok(memory_id)
}

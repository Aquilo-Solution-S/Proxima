//! Replay must not mint a second goal state or close-fact write-act.
#![allow(
    clippy::doc_markdown,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]

use std::sync::Arc;

use proxima_core::publication::{
    PublicationExtensions, PublicationLimits, PublicationSource, PublicationStamp,
};
use proxima_core::storage_ports::{FactIngestPort, MemoryAuthoringPort};
use proxima_core::storage_ports::{GoalWritePort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    GoalAssignmentTarget, GoalAtomicContext, GoalAuthorship, GoalDraft, GoalEvidenceRef,
    GoalPayloadWrite, GoalState, GoalTopologyWrite, GoalTransitionKind, GoalWriteOutcome,
    IdempotencyKey, ModifyGoalAtomicRequest, OperatorKind, SystemOrigin,
    TransitionGoalAtomicRequest,
};
use proxima_core::{
    AccessKind, EdgeEndpoint, EntityKind, FlavorRegistry, GoalId, GoalPayload, InputContractId,
    ModelId, OperatorId, OwnerId, PendingWork, PromptVersion, SchemaId, SchemaVersion,
    SimpleTextGoalV1, StorageError, ToolId, UserId,
};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::verbs::forget::{MemoryColdStore, erase_memory};
use uuid::Uuid;

fn memory_draft(kind: &str) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new(format!("p5/{kind}")),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        occurred_at: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: kind.into(),
        request: None,
    }
}

async fn ingest_grounded_perspective(
    pg: &PgStorage,
    permit: &OwnerWritePermit,
) -> Result<proxima_core::verbs::fact_ingest::FactIngestOutcome, proxima_core::StorageError> {
    let fact = pg
        .ingest_fact_atomic(permit, &memory_draft("fact"), None)
        .await?;
    let mut abs = memory_draft("abstraction");
    abs.additional_references = vec![EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)];
    let abs = pg.ingest_fact_atomic(permit, &abs, None).await?;
    let mut perspective = memory_draft("perspective");
    perspective.additional_references =
        vec![EdgeEndpoint::memory(EntityKind::Abstraction, abs.memory_id)];
    pg.ingest_fact_atomic(permit, &perspective, None).await
}

async fn fresh_pg() -> (String, PgStorage) {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let pg = PgStorage::connect_for_tests(db_url(&db_name))
        .await
        .expect("connect")
        .with_cold(Arc::new(MemoryColdStore));
    (db_name, pg)
}

fn context(registry: &proxima_core::FlavorRegistryFrozen) -> GoalAtomicContext<'_> {
    GoalAtomicContext {
        registry,
        author_self_perspective_id: None,
        publication: None,
    }
}

fn simple_payload(title: &str) -> GoalPayloadWrite {
    GoalPayloadWrite {
        schema_id: SchemaId::new(SimpleTextGoalV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(SimpleTextGoalV1::SCHEMA_VERSION),
        title: title.into(),
        text: title.into(),
        payload: Vec::new(),
        sidecar_payload: None,
    }
}

fn decompose_request<'a>(
    owner: OwnerId,
    parent_goal_id: proxima_core::GoalId,
    registry: &'a proxima_core::FlavorRegistryFrozen,
    assignment: proxima_core::MemoryId,
    evidence: proxima_core::MemoryId,
    first_title: &str,
    first_request_id: &str,
    second_request_id: &str,
) -> Result<DecomposeGoalAtomicRequest<'a>, Box<dyn std::error::Error>> {
    Ok(DecomposeGoalAtomicRequest {
        owner,
        parent_goal_id,
        authorship: GoalAuthorship::User,
        context: context(registry),
        topology: GoalTopologyWrite::new(
            Some(GoalAssignmentTarget::perspective(assignment)),
            Vec::new(),
            Vec::new(),
        )?,
        children: vec![
            ChildGoalDraft {
                payload: simple_payload(first_title),
                evidence: vec![GoalEvidenceRef::new(evidence)],
                request_id: IdempotencyKey::new(first_request_id)?,
            },
            ChildGoalDraft {
                payload: simple_payload("replay child two"),
                evidence: vec![GoalEvidenceRef::new(evidence)],
                request_id: IdempotencyKey::new(second_request_id)?,
            },
        ],
    })
}

fn active_draft(
    owner: OwnerId,
    assignment: proxima_core::MemoryId,
    request_id: &str,
    title: &str,
    evidence: Vec<GoalEvidenceRef>,
) -> Result<GoalDraft, Box<dyn std::error::Error>> {
    Ok(GoalDraft {
        owner,
        schema_id: SchemaId::new(SimpleTextGoalV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(SimpleTextGoalV1::SCHEMA_VERSION),
        title: title.into(),
        text: title.into(),
        payload: Vec::new(),
        sidecar_payload: None,
        state: GoalState::Active,
        topology: GoalTopologyWrite::new(
            Some(GoalAssignmentTarget::perspective(assignment)),
            Vec::new(),
            evidence,
        )?,
        supersedes_goal_id: None,
        authorship: GoalAuthorship::User,
        request_id: IdempotencyKey::new(request_id)?.into_string(),
    })
}

fn operator_authorship(model_id: &str, prompt_version: &str) -> GoalAuthorship {
    GoalAuthorship::System(SystemOrigin::Operator {
        operator_id: OperatorId::new(Uuid::now_v7()),
        operator_kind: OperatorKind::AtoGoal,
        input_contract_id: InputContractId::new(Uuid::now_v7()),
        model_id: ModelId::new(model_id),
        prompt_version: PromptVersion::new(prompt_version),
    })
}

fn assert_exact_replay(first: &GoalWriteOutcome, replay: &GoalWriteOutcome) {
    assert!(replay.idempotent_replay);
    assert_eq!(replay.goal_id, first.goal_id);
    assert_eq!(replay.edge_count, first.edge_count);
}

async fn erase_target(
    pg: &PgStorage,
    owner: &OwnerId,
    target: proxima_core::MemoryId,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tx = pg.pool_for_tests().begin().await?;
    erase_memory(
        &mut tx,
        &proxima_storage_pg::core_pg_sidecars(),
        &pg.host_state_erase_context()?,
        owner,
        target.into_inner(),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

type ReplayState = (i64, i64, i64, i64, i64, i64, i64);

async fn replay_state(pg: &PgStorage, owner: OwnerId) -> Result<ReplayState, sqlx::Error> {
    sqlx::query_as(
        "SELECT
            (SELECT count(*)::bigint FROM proxima_core.goal WHERE owner_id = $1),
            (SELECT count(*)::bigint FROM proxima_core.goal_head WHERE owner_id = $1),
            (SELECT count(*)::bigint
               FROM proxima_core.goal_replay_declaration d
               JOIN proxima_core.goal g ON g.t = d.goal_t
              WHERE g.owner_id = $1),
            (SELECT count(*)::bigint FROM proxima_core.sketch WHERE owner_id = $1),
            (SELECT count(*)::bigint FROM proxima_core.memory WHERE owner_id = $1),
            (SELECT count(*)::bigint FROM proxima_core.memory_head WHERE owner_id = $1),
            (SELECT count(*)::bigint FROM proxima_core.task_goal_v1)",
    )
    .bind(owner.into_inner())
    .fetch_one(pg.pool_for_tests())
    .await
}

/// With `core/goal-change-v1` listenable, every verb that inserts a goal row
/// writes one change Fact and captures one record in its transaction, and a
/// replay writes neither.
#[tokio::test]
async fn every_goal_verb_writes_one_goal_change_fact_and_a_replay_writes_none() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let mut listenable = FlavorRegistry::new();
        listenable.publish_goal_changes();
        let registry = listenable.freeze_or_panic_for_tests();
        let source = PublicationSource::new("urn:proxima:goal-feed-test")?;
        let extensions = PublicationExtensions::default();
        let stamped = || GoalAtomicContext {
            publication: Some(PublicationStamp::new(
                &source,
                None,
                &extensions,
                PublicationLimits::default(),
            )),
            ..context(&registry)
        };
        let assignment = ingest_grounded_perspective(&pg, &permit).await?.memory_id;
        let evidence = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?
            .memory_id;
        let step = |prior_goal_id, transition, id: &str| TransitionGoalAtomicRequest {
            owner,
            prior_goal_id,
            transition,
            authorship: GoalAuthorship::User,
            request_id: IdempotencyKey::new(id).expect("request id"),
            context: stamped(),
        };

        let set = CreateGoalAtomicRequest {
            draft: active_draft(owner, assignment, "feed-set", "feed", Vec::new())?,
            context: stamped(),
            write_act_t: None,
        };
        let set = pg.create_goal_atomic(&set, &permit).await?.goal_id;
        let pause = step(
            set,
            GoalTransitionKind::Pause {
                pending: PendingWork::Discard,
            },
            "feed-pause",
        );
        let paused = pg.transition_goal_atomic(&pause, &permit).await?.goal_id;
        let resume = step(paused, GoalTransitionKind::Resume, "feed-resume");
        let resumed = pg.transition_goal_atomic(&resume, &permit).await?.goal_id;
        let modify = ModifyGoalAtomicRequest {
            owner,
            prior_goal_id: resumed,
            replacement: simple_payload("feed modified"),
            authorship: GoalAuthorship::User,
            request_id: IdempotencyKey::new("feed-modify")?,
            context: stamped(),
            evidence: None,
        };
        let modified = pg.modify_goal_atomic(&modify, &permit).await?.goal_id;
        let mut decompose =
            decompose_request(owner, modified, &registry, assignment, evidence, "one", "feed-one", "feed-two")?;
        decompose.context = stamped();
        let children = pg.decompose_goal_atomic(&decompose, &permit).await?.children;
        let [one, two] = [children[0].outcome.goal_id, children[1].outcome.goal_id];
        let abandoned = pg
            .transition_goal_atomic(&step(one, GoalTransitionKind::Abandon, "feed-abandon"), &permit)
            .await?
            .goal_id;
        // #514: a parent is Achieved only after its open children close.
        let two_achieved = pg
            .achieve_goal_atomic(
                &AchieveGoalAtomicRequest {
                    owner,
                    prior_goal_id: two,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("feed-achieve-two")?,
                    context: stamped(),
                    evidence: vec![GoalEvidenceRef::new(evidence)],
                },
                &permit,
            )
            .await?
            .goal_id;
        let achieve = AchieveGoalAtomicRequest {
            owner,
            prior_goal_id: modified,
            authorship: GoalAuthorship::User,
            request_id: IdempotencyKey::new("feed-achieve")?,
            context: stamped(),
            evidence: vec![GoalEvidenceRef::new(evidence)],
        };
        let achieved = pg.achieve_goal_atomic(&achieve, &permit).await?.goal_id;

        // goal, kind, state, revision, prior, parent, pending
        let expected = [
            (set, "set", "Active", 1, None, None, None),
            (paused, "pause", "Paused", 2, Some(set), None, Some("discard")),
            (resumed, "resume", "Active", 3, Some(paused), None, None),
            (modified, "modify", "Active", 4, Some(resumed), None, None),
            (one, "decompose_child", "Active", 1, None, Some(modified), None),
            (two, "decompose_child", "Active", 1, None, Some(modified), None),
            (abandoned, "abandon", "Abandoned", 2, Some(one), None, None),
            (two_achieved, "achieve", "Achieved", 2, Some(two), None, None),
            (achieved, "achieve", "Achieved", 5, Some(modified), None, None),
        ];
        for (goal, kind, state, revision, prior, parent, pending) in expected {
            let row: (String, String, i32, Option<Uuid>, Option<Uuid>, Option<String>) =
                sqlx::query_as(
                    "SELECT change_kind::text, state::text, revision, prior_t, parent_t, pending::text
                       FROM proxima_core.goal_change_v1 WHERE goal_t = $1",
                )
                .bind(goal.into_inner())
                .fetch_one(pg.pool_for_tests())
                .await?;
            let wanted = (
                kind.to_owned(),
                state.to_owned(),
                revision,
                prior.map(GoalId::into_inner),
                parent.map(GoalId::into_inner),
                pending.map(str::to_owned),
            );
            assert_eq!(row, wanted, "{kind}");
        }
        let counts = || async {
            sqlx::query_as::<_, (i64, i64)>(
                "SELECT (SELECT count(*) FROM proxima_core.goal_change_v1),
                        (SELECT count(*) FROM proxima_core.publication_outbox
                          WHERE schema_id = 'core/goal-change-v1')",
            )
            .fetch_one(pg.pool_for_tests())
            .await
        };
        assert_eq!(counts().await?, (9, 9), "one Fact and one record per write");

        // Every record's position is its goal series and the change's
        // revision, so a consumer orders one goal's changes without reading
        // anything but the envelope.
        let positions: Vec<(Uuid, i32, Vec<u8>)> = sqlx::query_as(
            "SELECT c.series, c.revision, p.envelope
               FROM proxima_core.goal_change_v1 AS c
               JOIN proxima_core.publication_outbox AS p ON p.t = c.t",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert_eq!(positions.len(), 9);
        for (series, revision, envelope) in positions {
            let envelope: serde_json::Value = serde_json::from_slice(&envelope)?;
            assert_eq!(envelope["subject"], series.to_string());
            assert_eq!(envelope["sequence"], format!("{revision:020}"));
        }

        let replay = pg.transition_goal_atomic(&pause, &permit).await?;
        assert!(replay.idempotent_replay);
        assert_eq!(counts().await?, (9, 9), "a replay writes neither");

        // A listenable schema needs the stamp the engine builds: refused.
        let unstamped = CreateGoalAtomicRequest {
            draft: active_draft(owner, assignment, "feed-bare", "bare", Vec::new())?,
            context: context(&registry),
            write_act_t: None,
        };
        let refused = pg.create_goal_atomic(&unstamped, &permit).await.expect_err("refused");
        assert!(
            matches!(&refused, StorageError::ConstraintViolation(m) if m.contains("core/goal-change-v1")),
            "{refused}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("goal change feed scenario failed");
}

#[tokio::test]
async fn exact_goal_command_replays_precede_live_admission_for_every_verb() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();

        // Create: assignment, evidence, may all disappear
        // after the command committed. The declaration still decides replay;
        // a fresh key still enters live admission and fails.
        let create_assignment = ingest_grounded_perspective(&pg, &permit).await?;
        let create_evidence = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?;
        let create_first = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: active_draft(
                        owner,
                        create_assignment.memory_id,
                        "replay-create",
                        "replay create",
                        vec![GoalEvidenceRef::new(create_evidence.memory_id)],
                    )?,
                    context: context(&registry),
                    write_act_t: None,
                },
                &permit,
            )
            .await?;
        erase_target(&pg, &owner, create_assignment.memory_id).await?;
        erase_target(&pg, &owner, create_evidence.memory_id).await?;
        let create_before = replay_state(&pg, owner).await?;
        let create_replay = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: active_draft(
                        owner,
                        create_assignment.memory_id,
                        "replay-create",
                        "replay create",
                        vec![GoalEvidenceRef::new(create_evidence.memory_id)],
                    )?,
                    context: context(&registry),
                    write_act_t: None,
                },
                &permit,
            )
            .await?;
        assert_exact_replay(&create_first, &create_replay);
        let changed = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: active_draft(
                        owner,
                        create_assignment.memory_id,
                        "replay-create",
                        "changed replay create",
                        vec![GoalEvidenceRef::new(create_evidence.memory_id)],
                    )?,
                    context: context(&registry),
                    write_act_t: None,
                },
                &permit,
            )
            .await
            .expect_err("a changed create declaration must conflict");
        assert!(matches!(changed, StorageError::IdempotencyConflict { .. }));
        let fresh = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: active_draft(
                        owner,
                        create_assignment.memory_id,
                        "replay-create-fresh",
                        "fresh create",
                        vec![GoalEvidenceRef::new(create_evidence.memory_id)],
                    )?,
                    context: context(&registry),
                    write_act_t: None,
                },
                &permit,
            )
            .await
            .expect_err("a fresh create must still require live targets");
        assert!(matches!(fresh, StorageError::ConstraintViolation(_)));
        assert_eq!(replay_state(&pg, owner).await?, create_before);

        // Transition: exact replay precedes both stale-prior and carried state
        // checks, while a new successor still reauthorizes the carried state.
        let transition_assignment = ingest_grounded_perspective(&pg, &permit).await?;
        let transition_source = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: active_draft(
                        owner,
                        transition_assignment.memory_id,
                        "replay-transition-source",
                        "transition source",
                        Vec::new(),
                    )?,
                    context: context(&registry),
                    write_act_t: None,
                },
                &permit,
            )
            .await?;
        let transition_first = pg
            .transition_goal_atomic(
                &TransitionGoalAtomicRequest {
                    owner,
                    prior_goal_id: transition_source.goal_id,
                    transition: GoalTransitionKind::Pause {
                        pending: PendingWork::Keep,
                    },
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-transition")?,
                    context: context(&registry),
                },
                &permit,
            )
            .await?;
        erase_target(&pg, &owner, transition_assignment.memory_id).await?;
        let transition_before = replay_state(&pg, owner).await?;
        let transition_replay = pg
            .transition_goal_atomic(
                &TransitionGoalAtomicRequest {
                    owner,
                    prior_goal_id: transition_source.goal_id,
                    transition: GoalTransitionKind::Pause {
                        pending: PendingWork::Keep,
                    },
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-transition")?,
                    context: context(&registry),
                },
                &permit,
            )
            .await?;
        assert_exact_replay(&transition_first, &transition_replay);
        let changed = pg
            .transition_goal_atomic(
                &TransitionGoalAtomicRequest {
                    owner,
                    prior_goal_id: transition_source.goal_id,
                    transition: GoalTransitionKind::Abandon,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-transition")?,
                    context: context(&registry),
                },
                &permit,
            )
            .await
            .expect_err("a changed transition declaration must conflict");
        assert!(matches!(changed, StorageError::IdempotencyConflict { .. }));
        let changed_pending = pg
            .transition_goal_atomic(
                &TransitionGoalAtomicRequest {
                    owner,
                    prior_goal_id: transition_source.goal_id,
                    transition: GoalTransitionKind::Pause {
                        pending: PendingWork::Discard,
                    },
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-transition")?,
                    context: context(&registry),
                },
                &permit,
            )
            .await
            .expect_err("a pause that changes what happens to pending work must conflict");
        assert!(matches!(
            changed_pending,
            StorageError::IdempotencyConflict { .. }
        ));
        let fresh = pg
            .transition_goal_atomic(
                &TransitionGoalAtomicRequest {
                    owner,
                    prior_goal_id: transition_first.goal_id,
                    transition: GoalTransitionKind::Resume,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-transition-fresh")?,
                    context: context(&registry),
                },
                &permit,
            )
            .await
            .expect_err("a fresh transition must still require live carried targets");
        assert!(matches!(fresh, StorageError::ConstraintViolation(_)));
        assert_eq!(replay_state(&pg, owner).await?, transition_before);

        // Achieve: a cooled evidence target remains part of history, not a
        // renewed admission dependency for the request that already won.
        let achieve_assignment = ingest_grounded_perspective(&pg, &permit).await?;
        let achieve_evidence = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?;
        let changed_evidence = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?;
        let achieve_source = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: active_draft(
                        owner,
                        achieve_assignment.memory_id,
                        "replay-achieve-source",
                        "achieve source",
                        Vec::new(),
                    )?,
                    context: context(&registry),
                    write_act_t: None,
                },
                &permit,
            )
            .await?;
        let achieve_fresh_source = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: active_draft(
                        owner,
                        achieve_assignment.memory_id,
                        "replay-achieve-fresh-source",
                        "achieve fresh source",
                        Vec::new(),
                    )?,
                    context: context(&registry),
                    write_act_t: None,
                },
                &permit,
            )
            .await?;
        let achieve_first = pg
            .achieve_goal_atomic(
                &AchieveGoalAtomicRequest {
                    owner,
                    prior_goal_id: achieve_source.goal_id,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-achieve")?,
                    context: context(&registry),
                    evidence: vec![GoalEvidenceRef::new(achieve_evidence.memory_id)],
                },
                &permit,
            )
            .await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, achieve_evidence.memory_id).await?;
        let achieve_before = replay_state(&pg, owner).await?;
        let achieve_replay = pg
            .achieve_goal_atomic(
                &AchieveGoalAtomicRequest {
                    owner,
                    prior_goal_id: achieve_source.goal_id,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-achieve")?,
                    context: context(&registry),
                    evidence: vec![GoalEvidenceRef::new(achieve_evidence.memory_id)],
                },
                &permit,
            )
            .await?;
        assert_exact_replay(&achieve_first, &achieve_replay);
        let changed = pg
            .achieve_goal_atomic(
                &AchieveGoalAtomicRequest {
                    owner,
                    prior_goal_id: achieve_source.goal_id,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-achieve")?,
                    context: context(&registry),
                    evidence: vec![GoalEvidenceRef::new(changed_evidence.memory_id)],
                },
                &permit,
            )
            .await
            .expect_err("changed achievement evidence must conflict");
        assert!(matches!(changed, StorageError::IdempotencyConflict { .. }));
        let fresh = pg
            .achieve_goal_atomic(
                &AchieveGoalAtomicRequest {
                    owner,
                    prior_goal_id: achieve_fresh_source.goal_id,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-achieve-fresh")?,
                    context: context(&registry),
                    evidence: vec![GoalEvidenceRef::new(achieve_evidence.memory_id)],
                },
                &permit,
            )
            .await
            .expect_err("a fresh achievement must require live evidence");
        assert!(matches!(fresh, StorageError::ConstraintViolation(_)));
        assert_eq!(replay_state(&pg, owner).await?, achieve_before);

        // Modify: omitted evidence mean carry from the immutable
        // prior declaration. Replay must not load those now-retired targets.
        let modify_assignment = ingest_grounded_perspective(&pg, &permit).await?;
        let modify_evidence = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?;
        let modify_source = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: active_draft(
                        owner,
                        modify_assignment.memory_id,
                        "replay-modify-source",
                        "modify source",
                        vec![GoalEvidenceRef::new(modify_evidence.memory_id)],
                    )?,
                    context: context(&registry),
                    write_act_t: None,
                },
                &permit,
            )
            .await?;
        let modify_first = pg
            .modify_goal_atomic(
                &ModifyGoalAtomicRequest {
                    owner,
                    prior_goal_id: modify_source.goal_id,
                    replacement: simple_payload("replay modify"),
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-modify")?,
                    context: context(&registry),
                    evidence: None,
                },
                &permit,
            )
            .await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, modify_evidence.memory_id).await?;
        let modify_before = replay_state(&pg, owner).await?;
        let modify_replay = pg
            .modify_goal_atomic(
                &ModifyGoalAtomicRequest {
                    owner,
                    prior_goal_id: modify_source.goal_id,
                    replacement: simple_payload("replay modify"),
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-modify")?,
                    context: context(&registry),
                    evidence: None,
                },
                &permit,
            )
            .await?;
        assert_exact_replay(&modify_first, &modify_replay);
        let changed = pg
            .modify_goal_atomic(
                &ModifyGoalAtomicRequest {
                    owner,
                    prior_goal_id: modify_source.goal_id,
                    replacement: simple_payload("changed replay modify"),
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-modify")?,
                    context: context(&registry),
                    evidence: None,
                },
                &permit,
            )
            .await
            .expect_err("a changed modify declaration must conflict");
        assert!(matches!(changed, StorageError::IdempotencyConflict { .. }));
        let fresh = pg
            .modify_goal_atomic(
                &ModifyGoalAtomicRequest {
                    owner,
                    prior_goal_id: modify_first.goal_id,
                    replacement: simple_payload("fresh modify"),
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-modify-fresh")?,
                    context: context(&registry),
                    evidence: None,
                },
                &permit,
            )
            .await
            .expect_err("a fresh modify must require live carried evidence");
        assert!(matches!(fresh, StorageError::ConstraintViolation(_)));
        assert_eq!(replay_state(&pg, owner).await?, modify_before);

        // Decompose is one all-or-nothing replay set. It remains replayable
        // after its parent loses the active head and child targets retire;
        // changing one child or mixing old and fresh child keys conflicts.
        let decompose_assignment = ingest_grounded_perspective(&pg, &permit).await?;
        let decompose_evidence = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?;
        let decompose_parent = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: active_draft(
                        owner,
                        decompose_assignment.memory_id,
                        "replay-decompose-parent",
                        "decompose parent",
                        Vec::new(),
                    )?,
                    context: context(&registry),
                    write_act_t: None,
                },
                &permit,
            )
            .await?;
        let decompose_first = pg
            .decompose_goal_atomic(
                &decompose_request(
                    owner,
                    decompose_parent.goal_id,
                    &registry,
                    decompose_assignment.memory_id,
                    decompose_evidence.memory_id,
                    "replay child one",
                    "replay-decompose-one",
                    "replay-decompose-two",
                )?,
                &permit,
            )
            .await?;
        assert!(!decompose_first.idempotent_replay);
        pg.transition_goal_atomic(
            &TransitionGoalAtomicRequest {
                owner,
                prior_goal_id: decompose_parent.goal_id,
                transition: GoalTransitionKind::Pause {
                    pending: PendingWork::Keep,
                },
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("replay-decompose-parent-pause")?,
                context: context(&registry),
            },
            &permit,
        )
        .await?;
        erase_target(&pg, &owner, decompose_assignment.memory_id).await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, decompose_evidence.memory_id).await?;
        let decompose_before = replay_state(&pg, owner).await?;
        let decompose_replay = pg
            .decompose_goal_atomic(
                &decompose_request(
                    owner,
                    decompose_parent.goal_id,
                    &registry,
                    decompose_assignment.memory_id,
                    decompose_evidence.memory_id,
                    "replay child one",
                    "replay-decompose-one",
                    "replay-decompose-two",
                )?,
                &permit,
            )
            .await?;
        assert!(decompose_replay.idempotent_replay);
        assert_eq!(
            decompose_replay.children.len(),
            decompose_first.children.len()
        );
        for (first, replay) in decompose_first
            .children
            .iter()
            .zip(&decompose_replay.children)
        {
            assert_exact_replay(&first.outcome, &replay.outcome);
        }
        let changed = pg
            .decompose_goal_atomic(
                &decompose_request(
                    owner,
                    decompose_parent.goal_id,
                    &registry,
                    decompose_assignment.memory_id,
                    decompose_evidence.memory_id,
                    "changed child one",
                    "replay-decompose-one",
                    "replay-decompose-two",
                )?,
                &permit,
            )
            .await
            .expect_err("a changed child declaration must conflict");
        assert!(matches!(changed, StorageError::IdempotencyConflict { .. }));
        let partial = pg
            .decompose_goal_atomic(
                &decompose_request(
                    owner,
                    decompose_parent.goal_id,
                    &registry,
                    decompose_assignment.memory_id,
                    decompose_evidence.memory_id,
                    "replay child one",
                    "replay-decompose-one",
                    "replay-decompose-new-two",
                )?,
                &permit,
            )
            .await
            .expect_err("partial child request-id reuse must conflict");
        assert!(matches!(partial, StorageError::IdempotencyConflict { .. }));
        let fresh = pg
            .decompose_goal_atomic(
                &decompose_request(
                    owner,
                    decompose_parent.goal_id,
                    &registry,
                    decompose_assignment.memory_id,
                    decompose_evidence.memory_id,
                    "fresh child one",
                    "replay-decompose-fresh-one",
                    "replay-decompose-fresh-two",
                )?,
                &permit,
            )
            .await
            .expect_err("a fresh decomposition must require a current active parent");
        assert!(matches!(
            fresh,
            StorageError::Conflict(_) | StorageError::ConstraintViolation(_)
        ));
        assert_eq!(replay_state(&pg, owner).await?, decompose_before);

        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("exact Goal command replay declaration test failed");
}

#[tokio::test]
async fn goal_replay_ignores_host_metadata_but_preserves_authorship_category() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let assignment = ingest_grounded_perspective(&pg, &permit).await?;
        let fact = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?;
        let mut abstraction = memory_draft("abstraction");
        abstraction.additional_references =
            vec![EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)];
        let abstraction = pg.ingest_fact_atomic(&permit, &abstraction, None).await?;

        let mut first_draft = active_draft(
            owner,
            assignment.memory_id,
            "metadata-rotation",
            "metadata rotation",
            vec![GoalEvidenceRef::new(abstraction.memory_id)],
        )?;
        first_draft.authorship = operator_authorship("model-a", "prompt-a");
        let first = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: first_draft,
                    context: GoalAtomicContext {
                        registry: &registry,
                        author_self_perspective_id: Some(assignment.memory_id),
                        publication: None,
                    },
                    write_act_t: None,
                },
                &permit,
            )
            .await?;

        let mut replay_draft = active_draft(
            owner,
            assignment.memory_id,
            "metadata-rotation",
            "metadata rotation",
            vec![GoalEvidenceRef::new(abstraction.memory_id)],
        )?;
        replay_draft.authorship = operator_authorship("model-b", "prompt-b");
        let replay = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: replay_draft,
                    context: GoalAtomicContext {
                        registry: &registry,
                        author_self_perspective_id: None,
                        publication: None,
                    },
                    write_act_t: None,
                },
                &permit,
            )
            .await?;
        assert_exact_replay(&first, &replay);

        let mut category_changed_draft = active_draft(
            owner,
            assignment.memory_id,
            "metadata-rotation",
            "metadata rotation",
            vec![GoalEvidenceRef::new(abstraction.memory_id)],
        )?;
        category_changed_draft.authorship = GoalAuthorship::System(SystemOrigin::Tool {
            tool_id: ToolId::new("test/tool"),
        });
        let err = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: category_changed_draft,
                    context: GoalAtomicContext {
                        registry: &registry,
                        author_self_perspective_id: None,
                        publication: None,
                    },
                    write_act_t: None,
                },
                &permit,
            )
            .await
            .expect_err("a changed authorship category must conflict");
        assert!(matches!(err, StorageError::IdempotencyConflict { .. }));
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("Goal replay declaration metadata stability test failed");
}

#[tokio::test]
async fn a_goal_without_a_replay_declaration_fails_closed() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let assignment = ingest_grounded_perspective(&pg, &permit).await?;
        let first = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: active_draft(
                        owner,
                        assignment.memory_id,
                        "replay-without-declaration",
                        "declaration source",
                        Vec::new(),
                    )?,
                    context: context(&registry),
                    write_act_t: None,
                },
                &permit,
            )
            .await?;
        let update_err = sqlx::query(
            "UPDATE proxima_core.goal_replay_declaration
                SET edge_count = edge_count + 1
              WHERE goal_t = $1",
        )
        .bind(first.goal_id.into_inner())
        .execute(pg.pool_for_tests())
        .await
        .expect_err("a replay declaration must be immutable");
        assert!(
            update_err.to_string().contains("append-only")
                || update_err.to_string().contains("25006"),
            "unexpected declaration update error: {update_err}"
        );
        sqlx::query("DELETE FROM proxima_core.goal_replay_declaration WHERE goal_t = $1")
            .bind(first.goal_id.into_inner())
            .execute(pg.pool_for_tests())
            .await?;
        let before = replay_state(&pg, owner).await?;
        let err = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: active_draft(
                        owner,
                        assignment.memory_id,
                        "replay-without-declaration",
                        "declaration source",
                        Vec::new(),
                    )?,
                    context: context(&registry),
                    write_act_t: None,
                },
                &permit,
            )
            .await
            .expect_err("a historical Goal without a declaration must not be guessed");
        assert!(matches!(err, StorageError::IdempotencyConflict { .. }));
        assert_eq!(replay_state(&pg, owner).await?, before);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("missing Goal replay declaration must fail closed");
}

/// Wait until `n` backends of this database are parked on a lock. The pause
/// point reads the server's lock graph, so the test involves no timing.
async fn await_blocked_backends(pool: &sqlx::PgPool, n: i64) -> Result<(), sqlx::Error> {
    loop {
        let blocked: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM pg_stat_activity
              WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .fetch_one(pool)
        .await?;
        if blocked >= n {
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
}

/// Two identical create commands racing past replay detection must both
/// answer with the same goal; the second is a replay, not a
/// `ConstraintViolation` from the `(owner_id, request_id)` unique index.
///
/// The race is pinned by holding the owner's admission fence exclusively,
/// which every goal write takes right after its replay check. Both writers
/// are then parked behind it before either can insert.
#[tokio::test]
async fn concurrent_identical_goal_writes_replay() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let assignment = ingest_grounded_perspective(&pg, &permit).await?.memory_id;
        let pool = pg.pool_for_tests().clone();

        let mut fence = pool.begin().await?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('proxima-owner-fence:' || $1::text, 0))",
        )
        .bind(owner.into_inner())
        .execute(fence.as_mut())
        .await?;

        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let writer = |pg: PgStorage, barrier: Arc<tokio::sync::Barrier>| {
            tokio::spawn(async move {
                let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
                let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
                let req = CreateGoalAtomicRequest {
                    draft: active_draft(owner, assignment, "race-create", "race", Vec::new())
                        .expect("draft"),
                    context: context(&registry),
                    write_act_t: None,
                };
                barrier.wait().await;
                pg.create_goal_atomic(&req, &permit).await
            })
        };
        let a = writer(pg.clone(), barrier.clone());
        let b = writer(pg.clone(), barrier.clone());
        barrier.wait().await;
        await_blocked_backends(&pool, 2).await?;
        fence.commit().await?;

        let a = a.await?;
        let b = b.await?;
        let (a, b) = match (a, b) {
            (Ok(a), Ok(b)) => (a, b),
            (a, b) => panic!("both identical writes must succeed: {a:?} / {b:?}"),
        };
        assert_eq!(a.goal_id, b.goal_id, "both writers answer with one goal");
        assert!(
            a.idempotent_replay ^ b.idempotent_replay,
            "exactly one writer is flagged replay"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("concurrent identical goal writes scenario failed");
}

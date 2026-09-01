//! Replay must not mint a second wake_config or close-fact write-act.
#![allow(
    clippy::doc_markdown,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]

use std::sync::Arc;

use proxima_core::owner_inverse::OwnerSurfaces;
use proxima_core::storage_ports::{FactIngestPort, MemoryAuthoringPort};
use proxima_core::storage_ports::{GoalWritePort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    GoalAssignmentTarget, GoalAtomicContext, GoalAuthorship, GoalDraft, GoalEvidenceRef,
    GoalPayloadWrite, GoalState, GoalTopologyWrite, GoalWakeConfigWrite, GoalWakeToolId,
    GoalWakeTrigger, GoalWriteOutcome, IdempotencyKey, ModifyGoalAtomicRequest, OperatorKind,
    SystemOrigin, TransitionGoalAtomicRequest,
};
use proxima_core::{
    AccessKind, AuthPath, AuthzContext, EdgeEndpoint, Engine, EntityKind, FlavorRegistry,
    GoalCreatePayloadWriteRequest, GoalPayload, InputContractId, ModelId, OperatorId, OwnerRef,
    PromptVersion, SchemaId, SchemaVersion, SimpleTextGoalV1, StorageError, ToolId, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::forget::{MemoryColdStore, erase_memory};
use proxima_storage_pg::verbs::goal_timeseries::WRITE_ACT_SCHEMA;
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
        citation: None,
        derived_from: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: kind.into(),
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
    abs.derived_from = vec![EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)];
    let abs = pg.ingest_fact_atomic(permit, &abs, None).await?;
    let mut perspective = memory_draft("perspective");
    perspective.derived_from = vec![EdgeEndpoint::memory(EntityKind::Abstraction, abs.memory_id)];
    pg.ingest_fact_atomic(permit, &perspective, None).await
}

async fn fresh_pg() -> (String, PgStorage) {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let pg = PgStorage::connect(&db_url(&db_name))
        .await
        .expect("connect")
        .with_cold(Arc::new(MemoryColdStore::default()));
    pg.run_migrations().await.expect("migrate");
    (db_name, pg)
}

fn context(registry: &proxima_core::FlavorRegistryFrozen) -> GoalAtomicContext<'_> {
    context_with_model(registry, None)
}

fn context_with_model<'a>(
    registry: &'a proxima_core::FlavorRegistryFrozen,
    embedding_model_id: Option<&'a str>,
) -> GoalAtomicContext<'a> {
    GoalAtomicContext {
        registry,
        embedding_model_id,
        author_self_perspective_id: None,
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

fn wake_on(
    registry: &proxima_core::FlavorRegistryFrozen,
    target: proxima_core::MemoryId,
    prompt: &str,
) -> Result<GoalWakeConfigWrite, proxima_core::ProtocolError> {
    GoalWakeConfigWrite::new(
        GoalWakeTrigger::FactMemory { memory_id: target },
        vec![GoalWakeToolId::parse("core_remember", registry)?],
        prompt,
        &[],
    )
}

fn decompose_request<'a>(
    owner: OwnerRef,
    parent_goal_id: proxima_core::GoalId,
    registry: &'a proxima_core::FlavorRegistryFrozen,
    assignment: proxima_core::MemoryId,
    evidence: proxima_core::MemoryId,
    wake_target: proxima_core::MemoryId,
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
            GoalAssignmentTarget::perspective(assignment),
            Vec::new(),
            Vec::new(),
        )?,
        children: vec![
            ChildGoalDraft {
                payload: simple_payload(first_title),
                evidence: vec![GoalEvidenceRef::new(evidence)],
                wake: Some(wake_on(registry, wake_target, "decompose wake")?),
                request_id: IdempotencyKey::new(first_request_id)?,
            },
            ChildGoalDraft {
                payload: simple_payload("replay child two"),
                evidence: vec![GoalEvidenceRef::new(evidence)],
                wake: None,
                request_id: IdempotencyKey::new(second_request_id)?,
            },
        ],
    })
}

fn active_draft(
    owner: OwnerRef,
    assignment: proxima_core::MemoryId,
    request_id: &str,
    title: &str,
    evidence: Vec<GoalEvidenceRef>,
    wake: Option<GoalWakeConfigWrite>,
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
            GoalAssignmentTarget::perspective(assignment),
            Vec::new(),
            evidence,
        )?,
        wake,
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
    assert_eq!(replay.change_event_seq, first.change_event_seq);
    assert_eq!(replay.lifecycle_memory_id, first.lifecycle_memory_id);
    assert_eq!(replay.edge_count, first.edge_count);
}

async fn erase_target(
    pg: &PgStorage,
    owner: &OwnerRef,
    target: proxima_core::MemoryId,
) -> Result<(), Box<dyn std::error::Error>> {
    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let surfaces = OwnerSurfaces::for_registry(&registry);
    let mut tx = pg.pool_for_tests().begin().await?;
    erase_memory(
        &mut tx,
        &proxima_storage_pg::core_pg_sidecars(),
        &surfaces,
        owner,
        target.into_inner(),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

type ReplayState = (i64, i64, i64, i64, i64, i64, i64, i64, i64);

async fn replay_state(pg: &PgStorage, owner: OwnerRef) -> Result<ReplayState, sqlx::Error> {
    sqlx::query_as(
        "SELECT
            (SELECT count(*)::bigint FROM proxima_core.goal WHERE owner_id = $1),
            (SELECT count(*)::bigint FROM proxima_core.goal_head WHERE owner_id = $1),
            (SELECT count(*)::bigint
               FROM proxima_core.goal_replay_declaration d
               JOIN proxima_core.goal g ON g.t = d.goal_t
              WHERE g.owner_id = $1),
            (SELECT count(*)::bigint FROM proxima_core.wake_config WHERE owner_id = $1),
            (SELECT count(*)::bigint FROM proxima_core.announce WHERE owner_id = $1),
            (SELECT count(*)::bigint FROM proxima_core.sketch WHERE owner_id = $1),
            (SELECT count(*)::bigint FROM proxima_core.memory WHERE owner_id = $1),
            (SELECT count(*)::bigint FROM proxima_core.memory_head WHERE owner_id = $1),
            (SELECT count(*)::bigint FROM proxima_core.task_goal_v1)",
    )
    .bind(owner.stored_owner_id())
    .fetch_one(pg.pool_for_tests())
    .await
}

#[tokio::test]
async fn create_wake_replay_does_not_insert_second_wake_config() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let pool = pg.pool_for_tests();
        let planner = ingest_grounded_perspective(&pg, &permit).await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let trigger = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?;
        let wake = GoalWakeConfigWrite::new(
            GoalWakeTrigger::FactMemory {
                memory_id: trigger.memory_id,
            },
            vec![GoalWakeToolId::parse("core_remember", &registry)?],
            "wake on fact",
            &[],
        )?;
        let request_id = IdempotencyKey::new("p5-create-wake")?.into_string();
        let draft = GoalDraft {
            owner,
            schema_id: SchemaId::new(SimpleTextGoalV1::SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(SimpleTextGoalV1::SCHEMA_VERSION),
            title: "armed".into(),
            text: "armed".into(),
            payload: Vec::new(),
            sidecar_payload: None,
            state: GoalState::Active,
            topology: GoalTopologyWrite::new(
                GoalAssignmentTarget::perspective(planner.memory_id),
                Vec::new(),
                Vec::new(),
            )?,
            wake: Some(wake),
            supersedes_goal_id: None,
            authorship: GoalAuthorship::User,
            request_id,
        };
        let context = GoalAtomicContext {
            registry: &registry,
            embedding_model_id: None,
            author_self_perspective_id: None,
        };
        let first = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: draft.clone(),
                    context,
                    write_act_t: None,
                },
                &permit,
            )
            .await?;
        assert!(!first.idempotent_replay);
        let wakes_after_first: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM proxima_core.wake_config WHERE owner_id = $1",
        )
        .bind(owner.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(wakes_after_first, 1);
        let stored_wake: Option<Uuid> =
            sqlx::query_scalar("SELECT wake_id FROM proxima_core.goal WHERE t = $1")
                .bind(first.goal_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert!(stored_wake.is_some());

        let replay = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft,
                    context: GoalAtomicContext {
                        registry: &registry,
                        embedding_model_id: None,
                        author_self_perspective_id: None,
                    },
                    write_act_t: None,
                },
                &permit,
            )
            .await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.goal_id, first.goal_id);
        let wakes_after_replay: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM proxima_core.wake_config WHERE owner_id = $1",
        )
        .bind(owner.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(wakes_after_replay, 1, "replay must not mint a second wake");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("create wake replay must not orphan wake_config");
}

#[tokio::test]
async fn achieve_replay_does_not_insert_second_close_fact() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let pool = pg.pool_for_tests();
        let planner = ingest_grounded_perspective(&pg, &permit).await?;
        let evidence = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let created = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: GoalDraft {
                        owner,
                        schema_id: SchemaId::new(SimpleTextGoalV1::SCHEMA_ID.into()),
                        schema_version: SchemaVersion::new(SimpleTextGoalV1::SCHEMA_VERSION),
                        title: "close me".into(),
                        text: "close me".into(),
                        payload: Vec::new(),
                        sidecar_payload: None,
                        state: GoalState::Active,
                        topology: GoalTopologyWrite::new(
                            GoalAssignmentTarget::perspective(planner.memory_id),
                            Vec::new(),
                            Vec::new(),
                        )?,
                        wake: None,
                        supersedes_goal_id: None,
                        authorship: GoalAuthorship::User,
                        request_id: IdempotencyKey::new("p5-create")?.into_string(),
                    },
                    context: GoalAtomicContext {
                        registry: &registry,
                        embedding_model_id: None,
                        author_self_perspective_id: None,
                    },
                    write_act_t: None,
                },
                &permit,
            )
            .await?;
        assert!(!created.idempotent_replay);

        let first = pg
            .achieve_goal_atomic(
                &AchieveGoalAtomicRequest {
                    owner,
                    prior_goal_id: created.goal_id,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("p5-achieve")?,
                    context: GoalAtomicContext {
                        registry: &registry,
                        embedding_model_id: None,
                        author_self_perspective_id: None,
                    },
                    evidence: vec![GoalEvidenceRef::new(evidence.memory_id)],
                },
                &permit,
            )
            .await?;
        assert!(!first.idempotent_replay);
        let acts_after_first: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM proxima_core.memory
              WHERE owner_id = $1 AND schema_id = $2",
        )
        .bind(owner.stored_owner_id())
        .bind(WRITE_ACT_SCHEMA)
        .fetch_one(pool)
        .await?;
        assert_eq!(acts_after_first, 1);
        let close_fact: Option<Uuid> =
            sqlx::query_scalar("SELECT close_fact_t FROM proxima_core.goal WHERE t = $1")
                .bind(first.goal_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert!(close_fact.is_some());

        let replay = pg
            .achieve_goal_atomic(
                &AchieveGoalAtomicRequest {
                    owner,
                    prior_goal_id: created.goal_id,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("p5-achieve")?,
                    context: GoalAtomicContext {
                        registry: &registry,
                        embedding_model_id: None,
                        author_self_perspective_id: None,
                    },
                    evidence: vec![GoalEvidenceRef::new(evidence.memory_id)],
                },
                &permit,
            )
            .await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.goal_id, first.goal_id);
        let acts_after_replay: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM proxima_core.memory
              WHERE owner_id = $1 AND schema_id = $2",
        )
        .bind(owner.stored_owner_id())
        .bind(WRITE_ACT_SCHEMA)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            acts_after_replay, 1,
            "replay must not mint a second close-fact write-act"
        );
        let close_after: Option<Uuid> =
            sqlx::query_scalar("SELECT close_fact_t FROM proxima_core.goal WHERE t = $1")
                .bind(first.goal_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(close_after, close_fact);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("achieve replay must not orphan close_fact");
}

#[tokio::test]
async fn exact_goal_command_replays_precede_live_admission_for_every_verb() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();

        // Create: assignment, evidence, and wake targets may all disappear
        // after the command committed. The declaration still decides replay;
        // a fresh key still enters live admission and fails.
        let create_assignment = ingest_grounded_perspective(&pg, &permit).await?;
        let create_evidence = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?;
        let create_wake_target = pg
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
                        Some(wake_on(
                            &registry,
                            create_wake_target.memory_id,
                            "create wake",
                        )?),
                    )?,
                    context: context_with_model(&registry, Some("replacement-embed-model")),
                    write_act_t: None,
                },
                &permit,
            )
            .await?;
        erase_target(&pg, &owner, create_assignment.memory_id).await?;
        erase_target(&pg, &owner, create_evidence.memory_id).await?;
        erase_target(&pg, &owner, create_wake_target.memory_id).await?;
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
                        Some(wake_on(
                            &registry,
                            create_wake_target.memory_id,
                            "create wake",
                        )?),
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
                        Some(wake_on(
                            &registry,
                            create_wake_target.memory_id,
                            "create wake",
                        )?),
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
                        None,
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

        // Transition: exact replay precedes both stale-prior and carried-wake
        // checks, while a new successor still reauthorizes the carried state.
        let transition_assignment = ingest_grounded_perspective(&pg, &permit).await?;
        let transition_wake_target = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?;
        let transition_source = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: active_draft(
                        owner,
                        transition_assignment.memory_id,
                        "replay-transition-source",
                        "transition source",
                        Vec::new(),
                        Some(wake_on(
                            &registry,
                            transition_wake_target.memory_id,
                            "transition wake",
                        )?),
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
                    next_state: GoalState::Paused,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-transition")?,
                    context: context(&registry),
                },
                &permit,
            )
            .await?;
        erase_target(&pg, &owner, transition_assignment.memory_id).await?;
        erase_target(&pg, &owner, transition_wake_target.memory_id).await?;
        let transition_before = replay_state(&pg, owner).await?;
        let transition_replay = pg
            .transition_goal_atomic(
                &TransitionGoalAtomicRequest {
                    owner,
                    prior_goal_id: transition_source.goal_id,
                    next_state: GoalState::Paused,
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
                    next_state: GoalState::Abandoned,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-transition")?,
                    context: context(&registry),
                },
                &permit,
            )
            .await
            .expect_err("a changed transition declaration must conflict");
        assert!(matches!(changed, StorageError::IdempotencyConflict { .. }));
        let fresh = pg
            .transition_goal_atomic(
                &TransitionGoalAtomicRequest {
                    owner,
                    prior_goal_id: transition_first.goal_id,
                    next_state: GoalState::Active,
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
                        None,
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
                        None,
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

        // Modify: omitted evidence and wake mean carry from the immutable
        // prior declaration. Replay must not load those now-retired targets.
        let modify_assignment = ingest_grounded_perspective(&pg, &permit).await?;
        let modify_evidence = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?;
        let modify_wake_target = pg
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
                        Some(wake_on(
                            &registry,
                            modify_wake_target.memory_id,
                            "modify wake",
                        )?),
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
                    wake: None,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-modify")?,
                    context: context(&registry),
                    evidence: None,
                },
                &permit,
            )
            .await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, modify_evidence.memory_id).await?;
        erase_target(&pg, &owner, modify_wake_target.memory_id).await?;
        let modify_before = replay_state(&pg, owner).await?;
        let modify_replay = pg
            .modify_goal_atomic(
                &ModifyGoalAtomicRequest {
                    owner,
                    prior_goal_id: modify_source.goal_id,
                    replacement: simple_payload("replay modify"),
                    wake: None,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-modify")?,
                    context: context(&registry),
                    evidence: None,
                },
                &permit,
            )
            .await?;
        assert_exact_replay(&modify_first, &modify_replay);
        let changed_wake_intent = pg
            .modify_goal_atomic(
                &ModifyGoalAtomicRequest {
                    owner,
                    prior_goal_id: modify_source.goal_id,
                    replacement: simple_payload("replay modify"),
                    wake: Some(None),
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-modify")?,
                    context: context(&registry),
                    evidence: None,
                },
                &permit,
            )
            .await
            .expect_err("clear-wake must not replay a command that carried wake state");
        assert!(matches!(
            changed_wake_intent,
            StorageError::IdempotencyConflict { .. }
        ));
        let changed = pg
            .modify_goal_atomic(
                &ModifyGoalAtomicRequest {
                    owner,
                    prior_goal_id: modify_source.goal_id,
                    replacement: simple_payload("changed replay modify"),
                    wake: None,
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
                    wake: None,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("replay-modify-fresh")?,
                    context: context(&registry),
                    evidence: None,
                },
                &permit,
            )
            .await
            .expect_err("a fresh modify must require live carried evidence and wake targets");
        assert!(matches!(fresh, StorageError::ConstraintViolation(_)));
        assert_eq!(replay_state(&pg, owner).await?, modify_before);

        // Decompose is one all-or-nothing replay set. It remains replayable
        // after its parent loses the active head and child targets retire;
        // changing one child or mixing old and fresh child keys conflicts.
        let decompose_assignment = ingest_grounded_perspective(&pg, &permit).await?;
        let decompose_evidence = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?;
        let decompose_wake_target = pg
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
                        None,
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
                    decompose_wake_target.memory_id,
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
                next_state: GoalState::Paused,
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("replay-decompose-parent-pause")?,
                context: context(&registry),
            },
            &permit,
        )
        .await?;
        erase_target(&pg, &owner, decompose_assignment.memory_id).await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, decompose_evidence.memory_id).await?;
        erase_target(&pg, &owner, decompose_wake_target.memory_id).await?;
        let decompose_before = replay_state(&pg, owner).await?;
        let decompose_replay = pg
            .decompose_goal_atomic(
                &decompose_request(
                    owner,
                    decompose_parent.goal_id,
                    &registry,
                    decompose_assignment.memory_id,
                    decompose_evidence.memory_id,
                    decompose_wake_target.memory_id,
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
                    decompose_wake_target.memory_id,
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
                    decompose_wake_target.memory_id,
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
                    decompose_wake_target.memory_id,
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
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let assignment = ingest_grounded_perspective(&pg, &permit).await?;
        let fact = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?;
        let mut abstraction = memory_draft("abstraction");
        abstraction.derived_from = vec![EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)];
        let abstraction = pg.ingest_fact_atomic(&permit, &abstraction, None).await?;

        let mut first_draft = active_draft(
            owner,
            assignment.memory_id,
            "metadata-rotation",
            "metadata rotation",
            vec![GoalEvidenceRef::new(abstraction.memory_id)],
            None,
        )?;
        first_draft.authorship = operator_authorship("model-a", "prompt-a");
        let first = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: first_draft,
                    context: GoalAtomicContext {
                        registry: &registry,
                        embedding_model_id: Some("model-a"),
                        author_self_perspective_id: Some(assignment.memory_id),
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
            None,
        )?;
        replay_draft.authorship = operator_authorship("model-b", "prompt-b");
        let replay = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: replay_draft,
                    context: GoalAtomicContext {
                        registry: &registry,
                        embedding_model_id: Some("model-b"),
                        author_self_perspective_id: None,
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
            None,
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
                        embedding_model_id: Some("model-c"),
                        author_self_perspective_id: None,
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
async fn unit_of_work_resolves_goal_replay_before_live_target_admission() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let assignment = ingest_grounded_perspective(&pg, &permit).await?;
        let evidence = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?;
        let first = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: active_draft(
                        owner,
                        assignment.memory_id,
                        "unit-of-work-goal-replay",
                        "unit of work replay",
                        vec![GoalEvidenceRef::new(evidence.memory_id)],
                        None,
                    )?,
                    context: context(&registry),
                    write_act_t: None,
                },
                &permit,
            )
            .await?;
        erase_target(&pg, &owner, assignment.memory_id).await?;
        erase_target(&pg, &owner, evidence.memory_id).await?;

        let engine =
            Engine::new(registry.clone()).with_storage_ports(Arc::new(pg.clone()).storage_ports());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let mut unit = engine.unit_of_work(&authz).await?;
        let replay = unit
            .create_goal(
                GoalCreatePayloadWriteRequest {
                    owner,
                    topology: GoalTopologyWrite::new(
                        GoalAssignmentTarget::perspective(assignment.memory_id),
                        Vec::new(),
                        vec![GoalEvidenceRef::new(evidence.memory_id)],
                    )?,
                    wake: None,
                    payload: simple_payload("unit of work replay"),
                    request_id: IdempotencyKey::new("unit-of-work-goal-replay")?,
                    authorship: GoalAuthorship::User,
                    author_self_perspective_id: None,
                },
                None,
            )
            .await?;
        assert_exact_replay(&first, &replay);
        unit.commit().await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("UnitOfWork Goal replay must precede live target admission");
}

#[tokio::test]
async fn a_goal_without_a_replay_declaration_fails_closed() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
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
                        None,
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
                        None,
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

#[tokio::test]
async fn concurrent_terminal_create_mints_one_close_fact() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let planner = {
            let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
            ingest_grounded_perspective(&pg, &permit).await?
        };
        let draft = GoalDraft {
            owner,
            schema_id: SchemaId::new(SimpleTextGoalV1::SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(SimpleTextGoalV1::SCHEMA_VERSION),
            title: "concurrent close".into(),
            text: "concurrent close".into(),
            payload: Vec::new(),
            sidecar_payload: None,
            state: GoalState::Abandoned,
            topology: GoalTopologyWrite::new(
                GoalAssignmentTarget::perspective(planner.memory_id),
                Vec::new(),
                Vec::new(),
            )?,
            wake: None,
            supersedes_goal_id: None,
            authorship: GoalAuthorship::User,
            request_id: IdempotencyKey::new("concurrent-terminal-create")?.into_string(),
        };
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let run = |pg: PgStorage, draft: GoalDraft, barrier: Arc<tokio::sync::Barrier>| {
            tokio::spawn(async move {
                let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
                let permit = OwnerWritePermit::new_for_tests(draft.owner, AccessKind::Goal);
                barrier.wait().await;
                pg.create_goal_atomic(
                    &CreateGoalAtomicRequest {
                        draft,
                        context: GoalAtomicContext {
                            registry: &registry,
                            embedding_model_id: None,
                            author_self_perspective_id: None,
                        },
                        write_act_t: None,
                    },
                    &permit,
                )
                .await
            })
        };
        let first = run(pg.clone(), draft.clone(), barrier.clone());
        let second = run(pg.clone(), draft, barrier);
        let first = first.await?;
        let second = second.await?;
        assert!(
            first.is_ok() || second.is_ok(),
            "at least one create must win"
        );

        let pool = pg.pool_for_tests();
        let goals: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.goal
             WHERE owner_id = $1 AND request_id = 'concurrent-terminal-create'",
        )
        .bind(owner.stored_owner_id())
        .fetch_one(pool)
        .await?;
        let close_facts: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory
             WHERE owner_id = $1 AND schema_id = $2",
        )
        .bind(owner.stored_owner_id())
        .bind(WRITE_ACT_SCHEMA)
        .fetch_one(pool)
        .await?;
        assert_eq!(goals, 1, "the request-id race leaves one Goal");
        assert_eq!(
            close_facts, 1,
            "the losing terminal transaction rolls back its close Fact"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("concurrent terminal create must not orphan close Fact");
}

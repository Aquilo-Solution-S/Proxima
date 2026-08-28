//! Evidence kind ∈ {F, A} is enforced in-tx against TOCTOU, not by an engine
//! pre-tx walk.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::sync::Arc;

use proxima_core::storage_ports::FactIngestPort;
use proxima_core::storage_ports::MemoryAuthoringPort;
use proxima_core::storage_ports::{GoalReadPort, GoalWritePort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    GoalAssignmentTarget, GoalAtomicContext, GoalAuthorship, GoalDraft, GoalEvidenceRef,
    GoalPayloadWrite, GoalState, GoalTopologyWrite, IdempotencyKey, ModifyGoalAtomicRequest,
    OperatorKind, SystemOrigin,
};
use proxima_core::{
    AccessKind, EdgeEndpoint, EntityKind, FlavorRegistry, InputContractId, ModelId, OperatorId,
    OwnerRef, PromptVersion, SchemaId, SchemaVersion, StorageError, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::forget::MemoryColdStore;
use uuid::Uuid;

fn draft(kind: &str) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new(format!("d7/{kind}")),
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

#[tokio::test]
async fn perspective_evidence_is_rejected_in_tx() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url)
            .await?
            .with_cold(Arc::new(MemoryColdStore::default()));
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let fact = pg.ingest_fact_atomic(&permit, &draft("fact"), None).await?;
        let mut abs = draft("abstraction");
        abs.derived_from = vec![EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)];
        let abs = pg.ingest_fact_atomic(&permit, &abs, None).await?;
        let mut perspective = draft("perspective");
        perspective.derived_from =
            vec![EdgeEndpoint::memory(EntityKind::Abstraction, abs.memory_id)];
        let perspective = pg.ingest_fact_atomic(&permit, &perspective, None).await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let topology = GoalTopologyWrite::new(
            GoalAssignmentTarget::perspective(perspective.memory_id),
            Vec::new(),
            vec![GoalEvidenceRef::new(perspective.memory_id)],
        )?;
        let err = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: GoalDraft {
                        owner,
                        schema_id: SchemaId::new("core/simple-text-v1".into()),
                        schema_version: SchemaVersion::new(1),
                        title: "d7".into(),
                        text: "d7".into(),
                        payload: Vec::new(),
                        sidecar_payload: None,
                        state: GoalState::Active,
                        topology,
                        wake: None,
                        supersedes_goal_id: None,
                        authorship: GoalAuthorship::User,
                        request_id: IdempotencyKey::new("d7-evidence")?.into_string(),
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
            .await
            .expect_err("Perspective evidence must fail in-tx");
        match err {
            StorageError::ConstraintViolation(msg) => {
                assert!(
                    msg.contains("Fact or Abstraction"),
                    "storage kind TOCTOU, got {msg}"
                );
            }
            other => panic!("expected ConstraintViolation, got {other:?}"),
        }

        let topology = GoalTopologyWrite::new(
            GoalAssignmentTarget::perspective(perspective.memory_id),
            Vec::new(),
            vec![GoalEvidenceRef::new(fact.memory_id)],
        )?;
        let valid = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: GoalDraft {
                        owner,
                        schema_id: SchemaId::new("core/simple-text-v1".into()),
                        schema_version: SchemaVersion::new(1),
                        title: "d7-valid".into(),
                        text: "d7-valid".into(),
                        payload: Vec::new(),
                        sidecar_payload: None,
                        state: GoalState::Active,
                        topology,
                        wake: None,
                        supersedes_goal_id: None,
                        authorship: GoalAuthorship::User,
                        request_id: IdempotencyKey::new("d7-valid")?.into_string(),
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
        assert_eq!(
            pg.load_goal_evidence(&owner, valid.goal_id).await?,
            Some(vec![fact.memory_id]),
            "Goal read must return its exact stored evidence vector"
        );
        let foreign = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        assert_eq!(
            pg.load_goal_evidence(&foreign, valid.goal_id).await?,
            None,
            "owner mismatch must collapse to None"
        );

        let orphan = pg
            .ingest_fact_atomic(&permit, &draft("fact"), None)
            .await?;
        let stale_goal = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: GoalDraft {
                        owner,
                        schema_id: SchemaId::new("core/simple-text-v1".into()),
                        schema_version: SchemaVersion::new(1),
                        title: "d7-stale-evidence".into(),
                        text: "d7-stale-evidence".into(),
                        payload: Vec::new(),
                        sidecar_payload: None,
                        state: GoalState::Active,
                        topology: GoalTopologyWrite::new(
                            GoalAssignmentTarget::perspective(perspective.memory_id),
                            Vec::new(),
                            vec![
                                GoalEvidenceRef::new(fact.memory_id),
                                GoalEvidenceRef::new(orphan.memory_id),
                            ],
                        )?,
                        wake: None,
                        supersedes_goal_id: None,
                        authorship: GoalAuthorship::User,
                        request_id: IdempotencyKey::new("d7-stale-evidence")?.into_string(),
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
        assert_eq!(
            pg.load_goal_evidence(&owner, stale_goal.goal_id).await?,
            Some(vec![fact.memory_id, orphan.memory_id]),
            "the carried vector must retain cooled or missing positions"
        );
        MemoryAuthoringPort::forget_memory(&pg, &permit, orphan.memory_id).await?;
        let err = pg
            .modify_goal_atomic(
                &ModifyGoalAtomicRequest {
                    owner,
                    prior_goal_id: stale_goal.goal_id,
                    replacement: GoalPayloadWrite {
                        schema_id: SchemaId::new("core/simple-text-v1".into()),
                        schema_version: SchemaVersion::new(1),
                        title: "d7-stale-evidence-next".into(),
                        text: "d7-stale-evidence-next".into(),
                        payload: Vec::new(),
                        sidecar_payload: None,
                    },
                    wake: None,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("d7-stale-evidence-next")?,
                    context: GoalAtomicContext {
                        registry: &registry,
                        embedding_model_id: None,
                        author_self_perspective_id: None,
                    },
                    evidence: None,
                },
                &permit,
            )
            .await
            .expect_err("omitted storage evidence must fail closed on a missing target");
        assert!(
            matches!(err, StorageError::ConstraintViolation(ref message) if message.contains("evidence does not exist")),
            "got {err:?}"
        );

        pg.achieve_goal_atomic(
            &AchieveGoalAtomicRequest {
                owner,
                prior_goal_id: valid.goal_id,
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("d7-achieve-fact")?,
                context: GoalAtomicContext {
                    registry: &registry,
                    embedding_model_id: None,
                    author_self_perspective_id: None,
                },
                evidence: vec![GoalEvidenceRef::new(fact.memory_id)],
            },
            &permit,
        )
        .await
        .expect("host mark-achieved must accept Fact evidence");

        let operator = GoalAuthorship::System(SystemOrigin::Operator {
            operator_id: OperatorId::new(Uuid::now_v7()),
            operator_kind: OperatorKind::AtoGoal,
            input_contract_id: InputContractId::new(Uuid::now_v7()),
            model_id: ModelId::new("test-model"),
            prompt_version: PromptVersion::new("v1"),
        });
        let operator_fact_topology = GoalTopologyWrite::new(
            GoalAssignmentTarget::perspective(perspective.memory_id),
            Vec::new(),
            vec![GoalEvidenceRef::new(fact.memory_id)],
        )?;
        let err = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: GoalDraft {
                        owner,
                        schema_id: SchemaId::new("core/simple-text-v1".into()),
                        schema_version: SchemaVersion::new(1),
                        title: "d7-operator-fact".into(),
                        text: "d7-operator-fact".into(),
                        payload: Vec::new(),
                        sidecar_payload: None,
                        state: GoalState::Active,
                        topology: operator_fact_topology,
                        wake: None,
                        supersedes_goal_id: None,
                        authorship: operator.clone(),
                        request_id: IdempotencyKey::new("d7-operator-fact")?.into_string(),
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
            .await
            .expect_err("operator-authored Goal must reject Fact evidence");
        assert!(
            matches!(err, StorageError::ConstraintViolation(ref message) if message.contains("Abstraction")),
            "got {err:?}"
        );

        let operator_abstraction_topology = GoalTopologyWrite::new(
            GoalAssignmentTarget::perspective(perspective.memory_id),
            Vec::new(),
            vec![GoalEvidenceRef::new(abs.memory_id)],
        )?;
        let operator_goal = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: GoalDraft {
                        owner,
                        schema_id: SchemaId::new("core/simple-text-v1".into()),
                        schema_version: SchemaVersion::new(1),
                        title: "d7-operator-abstraction".into(),
                        text: "d7-operator-abstraction".into(),
                        payload: Vec::new(),
                        sidecar_payload: None,
                        state: GoalState::Active,
                        topology: operator_abstraction_topology,
                        wake: None,
                        supersedes_goal_id: None,
                        authorship: operator,
                        request_id: IdempotencyKey::new("d7-operator-abstraction")?.into_string(),
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
        assert_eq!(
            pg.load_goal_evidence(&owner, operator_goal.goal_id).await?,
            Some(vec![abs.memory_id]),
            "operator A evidence must be admitted and persisted"
        );

        pg.achieve_goal_atomic(
            &AchieveGoalAtomicRequest {
                owner,
                prior_goal_id: operator_goal.goal_id,
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("d7-achieve-abstraction")?,
                context: GoalAtomicContext {
                    registry: &registry,
                    embedding_model_id: None,
                    author_self_perspective_id: None,
                },
                evidence: vec![GoalEvidenceRef::new(abs.memory_id)],
            },
            &permit,
        )
        .await
        .expect("host mark-achieved must accept Abstraction evidence");

        let empty_goal = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: GoalDraft {
                        owner,
                        schema_id: SchemaId::new("core/simple-text-v1".into()),
                        schema_version: SchemaVersion::new(1),
                        title: "d7-perspective-close".into(),
                        text: "d7-perspective-close".into(),
                        payload: Vec::new(),
                        sidecar_payload: None,
                        state: GoalState::Active,
                        topology: GoalTopologyWrite::new(
                            GoalAssignmentTarget::perspective(perspective.memory_id),
                            Vec::new(),
                            Vec::new(),
                        )?,
                        wake: None,
                        supersedes_goal_id: None,
                        authorship: GoalAuthorship::User,
                        request_id: IdempotencyKey::new("d7-perspective-close")?.into_string(),
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
        let err = pg
            .achieve_goal_atomic(
                &AchieveGoalAtomicRequest {
                    owner,
                    prior_goal_id: empty_goal.goal_id,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("d7-achieve-perspective")?,
                    context: GoalAtomicContext {
                        registry: &registry,
                        embedding_model_id: None,
                        author_self_perspective_id: None,
                    },
                    evidence: vec![GoalEvidenceRef::new(perspective.memory_id)],
                },
                &permit,
            )
            .await
            .expect_err("mark-achieved must reject Perspective evidence");
        assert!(
            matches!(err, StorageError::ConstraintViolation(ref message) if message.contains("Fact or Abstraction")),
            "got {err:?}"
        );

        let parent = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: GoalDraft {
                        owner,
                        schema_id: SchemaId::new("core/simple-text-v1".into()),
                        schema_version: SchemaVersion::new(1),
                        title: "d7-decompose-parent".into(),
                        text: "d7-decompose-parent".into(),
                        payload: Vec::new(),
                        sidecar_payload: None,
                        state: GoalState::Active,
                        topology: GoalTopologyWrite::new(
                            GoalAssignmentTarget::perspective(perspective.memory_id),
                            Vec::new(),
                            vec![GoalEvidenceRef::new(fact.memory_id)],
                        )?,
                        wake: None,
                        supersedes_goal_id: None,
                        authorship: GoalAuthorship::User,
                        request_id: IdempotencyKey::new("d7-decompose-parent")?.into_string(),
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
        let decompose_err = pg
            .decompose_goal_atomic(
                &DecomposeGoalAtomicRequest {
                    owner,
                    parent_goal_id: parent.goal_id,
                    authorship: GoalAuthorship::User,
                    context: GoalAtomicContext {
                        registry: &registry,
                        embedding_model_id: None,
                        author_self_perspective_id: None,
                    },
                    topology: GoalTopologyWrite::new(
                        GoalAssignmentTarget::perspective(perspective.memory_id),
                        Vec::new(),
                        Vec::new(),
                    )?,
                    children: vec![
                        ChildGoalDraft {
                            payload: GoalPayloadWrite {
                                schema_id: SchemaId::new("core/simple-text-v1".into()),
                                schema_version: SchemaVersion::new(1),
                                title: "d7-child-valid".into(),
                                text: "d7-child-valid".into(),
                                payload: Vec::new(),
                                sidecar_payload: None,
                            },
                            evidence: vec![GoalEvidenceRef::new(abs.memory_id)],
                            wake: None,
                            request_id: IdempotencyKey::new("d7-child-valid")?,
                        },
                        ChildGoalDraft {
                            payload: GoalPayloadWrite {
                                schema_id: SchemaId::new("core/simple-text-v1".into()),
                                schema_version: SchemaVersion::new(1),
                                title: "d7-child-invalid".into(),
                                text: "d7-child-invalid".into(),
                                payload: Vec::new(),
                                sidecar_payload: None,
                            },
                            evidence: vec![GoalEvidenceRef::new(perspective.memory_id)],
                            wake: None,
                            request_id: IdempotencyKey::new("d7-child-invalid")?,
                        },
                    ],
                },
                &permit,
            )
            .await
            .expect_err("one invalid child must roll back every child insert");
        assert!(
            matches!(decompose_err, StorageError::ConstraintViolation(ref message) if message.contains("Fact or Abstraction")),
            "got {decompose_err:?}"
        );
        for request_id in ["d7-child-valid", "d7-child-invalid"] {
            let inserted: i64 = sqlx::query_scalar(
                "SELECT COUNT(*)::bigint FROM proxima_core.goal WHERE request_id = $1",
            )
            .bind(request_id)
            .fetch_one(pg.pool_for_tests())
            .await?;
            assert_eq!(inserted, 0, "failed decomposition must leave no child: {request_id}");
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("evidence kind TOCTOU failed");
}

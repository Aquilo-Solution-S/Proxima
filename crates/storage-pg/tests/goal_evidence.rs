//! Evidence kind ∈ {F, A} is enforced in-tx against TOCTOU, not by an engine
//! pre-tx walk.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::sync::Arc;

use proxima_core::storage_ports::FactIngestPort;
use proxima_core::storage_ports::MemoryAuthoringPort;
use proxima_core::storage_ports::{GoalReadPort, GoalWritePort, OwnerWritePermit};
use proxima_core::test_fixtures::owner_scope_for_tests;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    GoalAssignmentTarget, GoalAtomicContext, GoalAuthorship, GoalDraft, GoalEvidenceRef,
    GoalPayloadWrite, GoalState, GoalTopologyWrite, IdempotencyKey, ModifyGoalAtomicRequest,
    OperatorKind, SystemOrigin,
};
use proxima_core::{
    AccessKind, EdgeEndpoint, EntityKind, FlavorRegistry, InputContractId, ModelId, OperatorId,
    OwnerId, PromptVersion, SchemaId, SchemaVersion, StorageError, UserId,
};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::test_fixtures::create_core_db;
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
        occurred_at: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: kind.into(),
    }
}

#[tokio::test]
async fn perspective_evidence_is_rejected_in_tx() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone())
            .await?
            .with_cold(Arc::new(MemoryColdStore));
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let owner_scope = owner_scope_for_tests(&[owner]);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let fact = pg.ingest_fact_atomic(&permit, &draft("fact"), None).await?;
        let mut abs = draft("abstraction");
        abs.additional_references = vec![EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)];
        let abs = pg.ingest_fact_atomic(&permit, &abs, None).await?;
        let mut perspective = draft("perspective");
        perspective.additional_references =
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
                        supersedes_goal_id: None,
                        authorship: GoalAuthorship::User,
                        request_id: IdempotencyKey::new("d7-evidence")?.into_string(),
                    },
                    context: GoalAtomicContext {
                        registry: &registry,
                        author_self_perspective_id: None,
                        publication: None,
                    },
                    write_act_t: None,
                },
                &permit)
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
                        supersedes_goal_id: None,
                        authorship: GoalAuthorship::User,
                        request_id: IdempotencyKey::new("d7-valid")?.into_string(),
                    },
                    context: GoalAtomicContext {
                        registry: &registry,
                        author_self_perspective_id: None,
                        publication: None,
                    },
                    write_act_t: None,
                },
                &permit)
            .await?;
        assert_eq!(
            pg.load_goal_evidence(Some(&owner_scope), &owner, valid.goal_id).await?,
            Some(vec![fact.memory_id]),
            "Goal read must return its exact stored evidence vector"
        );
        let foreign = OwnerId::from(UserId::new(Uuid::now_v7()));
        assert_eq!(
            pg.load_goal_evidence(
                Some(&owner_scope_for_tests(&[foreign])),
                &foreign,
                valid.goal_id,
            )
            .await?,
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
                        supersedes_goal_id: None,
                        authorship: GoalAuthorship::User,
                        request_id: IdempotencyKey::new("d7-stale-evidence")?.into_string(),
                    },
                    context: GoalAtomicContext {
                        registry: &registry,
                        author_self_perspective_id: None,
                        publication: None,
                    },
                    write_act_t: None,
                },
                &permit)
            .await?;
        assert_eq!(
            pg.load_goal_evidence(Some(&owner_scope), &owner, stale_goal.goal_id).await?,
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
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("d7-stale-evidence-next")?,
                    context: GoalAtomicContext {
                        registry: &registry,
                        author_self_perspective_id: None,
                        publication: None,
                    },
                    evidence: None,
                },
                &permit)
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
                    author_self_perspective_id: None,
                    publication: None,
                },
                evidence: vec![GoalEvidenceRef::new(fact.memory_id)],
            },
            &permit)
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
        let operator_fact = pg
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
                        supersedes_goal_id: None,
                        authorship: operator.clone(),
                        request_id: IdempotencyKey::new("d7-operator-fact")?.into_string(),
                    },
                    context: GoalAtomicContext {
                        registry: &registry,
                        author_self_perspective_id: None,
                        publication: None,
                    },
                    write_act_t: None,
                },
                &permit)
            .await?;
        assert_eq!(
            pg.load_goal_evidence(Some(&owner_scope), &owner, operator_fact.goal_id)
                .await?,
            Some(vec![fact.memory_id]),
            "operator-authored Goal must admit and persist Fact evidence (#513)"
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
                        supersedes_goal_id: None,
                        authorship: operator,
                        request_id: IdempotencyKey::new("d7-operator-abstraction")?.into_string(),
                    },
                    context: GoalAtomicContext {
                        registry: &registry,
                        author_self_perspective_id: None,
                        publication: None,
                    },
                    write_act_t: None,
                },
                &permit)
            .await?;
        assert_eq!(
            pg.load_goal_evidence(
                Some(&owner_scope_for_tests(&[owner])),
                &owner,
                operator_goal.goal_id,
            )
            .await?,
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
                    author_self_perspective_id: None,
                    publication: None,
                },
                evidence: vec![GoalEvidenceRef::new(abs.memory_id)],
            },
            &permit)
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
                        supersedes_goal_id: None,
                        authorship: GoalAuthorship::User,
                        request_id: IdempotencyKey::new("d7-perspective-close")?.into_string(),
                    },
                    context: GoalAtomicContext {
                        registry: &registry,
                        author_self_perspective_id: None,
                        publication: None,
                    },
                    write_act_t: None,
                },
                &permit)
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
                        author_self_perspective_id: None,
                        publication: None,
                    },
                    evidence: vec![GoalEvidenceRef::new(perspective.memory_id)],
                },
                &permit)
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
                        supersedes_goal_id: None,
                        authorship: GoalAuthorship::User,
                        request_id: IdempotencyKey::new("d7-decompose-parent")?.into_string(),
                    },
                    context: GoalAtomicContext {
                        registry: &registry,
                        author_self_perspective_id: None,
                        publication: None,
                    },
                    write_act_t: None,
                },
                &permit)
            .await?;
        let decompose_err = pg
            .decompose_goal_atomic(
                &DecomposeGoalAtomicRequest {
                    owner,
                    parent_goal_id: parent.goal_id,
                    authorship: GoalAuthorship::User,
                    context: GoalAtomicContext {
                        registry: &registry,
                        author_self_perspective_id: None,
                        publication: None,
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
                            request_id: IdempotencyKey::new("d7-child-invalid")?,
                        },
                    ],
                },
                &permit)
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

struct Fixture {
    db_name: String,
    pg: PgStorage,
    owner: OwnerId,
    permit: OwnerWritePermit,
    fact: proxima_core::MemoryId,
    abs: proxima_core::MemoryId,
    perspective: proxima_core::MemoryId,
}

async fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let pg = PgStorage::connect_for_tests(db_url(&db_name))
        .await?
        .with_cold(Arc::new(MemoryColdStore));
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
    let fact = pg.ingest_fact_atomic(&permit, &draft("fact"), None).await?;
    let mut abs = draft("abstraction");
    abs.additional_references = vec![EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)];
    let abs = pg.ingest_fact_atomic(&permit, &abs, None).await?;
    let mut perspective = draft("perspective");
    perspective.additional_references =
        vec![EdgeEndpoint::memory(EntityKind::Abstraction, abs.memory_id)];
    let perspective = pg.ingest_fact_atomic(&permit, &perspective, None).await?;
    Ok(Fixture {
        db_name,
        pg,
        owner,
        permit,
        fact: fact.memory_id,
        abs: abs.memory_id,
        perspective: perspective.memory_id,
    })
}

fn operator_authorship() -> GoalAuthorship {
    GoalAuthorship::System(SystemOrigin::Operator {
        operator_id: OperatorId::new(Uuid::now_v7()),
        operator_kind: OperatorKind::AtoGoal,
        input_contract_id: InputContractId::new(Uuid::now_v7()),
        model_id: ModelId::new("test-model"),
        prompt_version: PromptVersion::new("v1"),
    })
}

fn payload(title: &str) -> GoalPayloadWrite {
    GoalPayloadWrite {
        schema_id: SchemaId::new("core/simple-text-v1".into()),
        schema_version: SchemaVersion::new(1),
        title: title.into(),
        text: title.into(),
        payload: Vec::new(),
        sidecar_payload: None,
    }
}

fn context(registry: &proxima_core::FlavorRegistryFrozen) -> GoalAtomicContext<'_> {
    GoalAtomicContext {
        registry,
        author_self_perspective_id: None,
        publication: None,
    }
}

impl Fixture {
    async fn create(
        &self,
        registry: &proxima_core::FlavorRegistryFrozen,
        key: &str,
        authorship: GoalAuthorship,
        evidence: &[proxima_core::MemoryId],
    ) -> Result<proxima_core::verbs::goal_write::GoalWriteOutcome, StorageError> {
        let topology = GoalTopologyWrite::new(
            GoalAssignmentTarget::perspective(self.perspective),
            Vec::new(),
            evidence.iter().copied().map(GoalEvidenceRef::new).collect(),
        )
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        self.pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: GoalDraft {
                        owner: self.owner,
                        schema_id: SchemaId::new("core/simple-text-v1".into()),
                        schema_version: SchemaVersion::new(1),
                        title: key.into(),
                        text: key.into(),
                        payload: Vec::new(),
                        sidecar_payload: None,
                        state: GoalState::Active,
                        topology,
                        supersedes_goal_id: None,
                        authorship,
                        request_id: key.into(),
                    },
                    context: context(registry),
                    write_act_t: None,
                },
                &self.permit,
            )
            .await
    }

    async fn modify(
        &self,
        registry: &proxima_core::FlavorRegistryFrozen,
        key: &str,
        prior: proxima_core::GoalId,
        authorship: GoalAuthorship,
        evidence: Option<&[proxima_core::MemoryId]>,
    ) -> Result<proxima_core::verbs::goal_write::GoalWriteOutcome, StorageError> {
        self.pg
            .modify_goal_atomic(
                &ModifyGoalAtomicRequest {
                    owner: self.owner,
                    prior_goal_id: prior,
                    replacement: payload(key),
                    authorship,
                    request_id: IdempotencyKey::new(key)
                        .map_err(|e| StorageError::Internal(e.clone()))?,
                    context: context(registry),
                    evidence: evidence
                        .map(|ids| ids.iter().copied().map(GoalEvidenceRef::new).collect()),
                },
                &self.permit,
            )
            .await
    }

    async fn evidence_of(
        &self,
        goal: proxima_core::GoalId,
    ) -> Result<Option<Vec<proxima_core::MemoryId>>, StorageError> {
        self.pg
            .load_goal_evidence(
                Some(&owner_scope_for_tests(&[self.owner])),
                &self.owner,
                goal,
            )
            .await
    }
}

/// #513 F2: operator modify (explicit and carried) and operator decompose
/// admit Fact evidence, alone or mixed with an Abstraction.
#[tokio::test]
async fn operator_modify_and_decompose_accept_fact_evidence() {
    let fx = fixture().await.expect("fixture");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let (fact, abs) = (fx.fact, fx.abs);

        // (a) explicit replacement [fact] over an operator head with [abs].
        let head = fx
            .create(&registry, "f2-op-abs", operator_authorship(), &[abs])
            .await?;
        let modified = fx
            .modify(
                &registry,
                "f2-op-mod-fact",
                head.goal_id,
                operator_authorship(),
                Some(&[fact]),
            )
            .await?;
        assert_eq!(
            fx.evidence_of(modified.goal_id).await?,
            Some(vec![fact]),
            "explicit"
        );

        // (b) carried: operator modify with evidence omitted over a
        // user-authored head citing [fact].
        let user_head = fx
            .create(&registry, "f2-user-fact", GoalAuthorship::User, &[fact])
            .await?;
        let carried = fx
            .modify(
                &registry,
                "f2-op-mod-carried",
                user_head.goal_id,
                operator_authorship(),
                None,
            )
            .await?;
        assert_eq!(
            fx.evidence_of(carried.goal_id).await?,
            Some(vec![fact]),
            "carried"
        );

        // (c) operator decompose with children [fact] and [fact, abs].
        let parent = fx
            .create(&registry, "f2-parent", GoalAuthorship::User, &[fact])
            .await?;
        let child = |key: &str,
                     evidence: Vec<proxima_core::MemoryId>|
         -> Result<ChildGoalDraft, Box<dyn std::error::Error>> {
            Ok(ChildGoalDraft {
                payload: payload(key),
                evidence: evidence.into_iter().map(GoalEvidenceRef::new).collect(),
                request_id: IdempotencyKey::new(key)?,
            })
        };
        let outcome = fx
            .pg
            .decompose_goal_atomic(
                &DecomposeGoalAtomicRequest {
                    owner: fx.owner,
                    parent_goal_id: parent.goal_id,
                    authorship: operator_authorship(),
                    context: context(&registry),
                    topology: GoalTopologyWrite::new(
                        GoalAssignmentTarget::perspective(fx.perspective),
                        Vec::new(),
                        Vec::new(),
                    )?,
                    children: vec![
                        child("f2-child-fact", vec![fact])?,
                        child("f2-child-mixed", vec![fact, abs])?,
                    ],
                },
                &fx.permit,
            )
            .await?;
        let children: Vec<_> = outcome.children.iter().map(|c| c.outcome.goal_id).collect();
        assert_eq!(children.len(), 2, "two children");
        assert_eq!(
            fx.evidence_of(children[0]).await?,
            Some(vec![fact]),
            "child [fact]"
        );
        assert_eq!(
            fx.evidence_of(children[1]).await?,
            Some(vec![fact, abs]),
            "child [fact, abs]"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&fx.db_name).await;
    result.expect("operator Fact evidence on modify and decompose");
}

/// #513: with the operator kind check gone, a Perspective is still refused
/// by the one in-transaction kind rule, and an operator write still needs
/// non-empty evidence (F7: the carried-empty modify, message wording).
#[tokio::test]
async fn operator_perspective_and_empty_evidence_are_refused() {
    let fx = fixture().await.expect("fixture");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let constraint = |result: Result<_, StorageError>, what: &str| -> String {
            match result {
                Err(StorageError::ConstraintViolation(message)) => message,
                Err(other) => panic!("{what}: expected ConstraintViolation, got {other:?}"),
                Ok(_) => panic!("{what}: accepted"),
            }
        };
        let head = fx
            .create(&registry, "f7-op-abs", operator_authorship(), &[fx.abs])
            .await?;
        let message = constraint(
            fx.modify(
                &registry,
                "f7-op-mod-persp",
                head.goal_id,
                operator_authorship(),
                Some(&[fx.perspective]),
            )
            .await,
            "operator modify with a Perspective",
        );
        assert_eq!(message, "evidence must be Fact or Abstraction");

        let message = constraint(
            fx.create(&registry, "f7-op-empty", operator_authorship(), &[])
                .await,
            "operator create with empty evidence",
        );
        assert_eq!(
            message,
            "operator-authored Goal requires non-empty evidence"
        );

        let user_empty = fx
            .create(&registry, "f7-user-empty", GoalAuthorship::User, &[])
            .await?;
        let message = constraint(
            fx.modify(
                &registry,
                "f7-op-mod-carried-empty",
                user_empty.goal_id,
                operator_authorship(),
                None,
            )
            .await,
            "operator modify carrying empty evidence",
        );
        assert_eq!(
            message,
            "operator-authored Goal requires non-empty evidence"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&fx.db_name).await;
    result.expect("operator Perspective and empty evidence refused");
}

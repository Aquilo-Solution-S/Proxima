//! Goal motivation records exact declared Perspective versions on the Goal.
//! Wake execution is external to Proxiima.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "each suite uses part of the shared fixture")]
mod example_engine;

use example_engine::ExampleEngine;
use proxiima_core::verbs::goal_write::{
    GoalAuthorship, GoalCreateRequest, GoalEvidenceRef, GoalMotivationRef, GoalTopologyWrite,
    GoalTransitionKind, IdempotencyKey,
};
use proxiima_core::{
    AuthPath, AuthzContext, DerivedMemory, EntityId, FactWrite, GoalTransitionRequest, GroupId,
    InterpretationSubjectKind, InterpretationV1, MemoryId, MemoryTarget, OwnerId, PendingWork,
    SeriesHandle, SimpleTextGoalV1, UserId,
};
use proxiima_example::NoteV1;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one atomic feedback scenario keeps version history and transfer assertions together"
)]
async fn goal_perspective_feedback_keeps_declared_exact_version_context() {
    let h = ExampleEngine::new("proxiima_goal_targeting", |_| {}).await;
    let user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(user);
    let authz = proxiima_core::test_fixtures::authenticated_context(AuthzContext::for_subject(
        user,
        AuthPath::HostBearer,
    ));
    let notebook = h.notebook(&owner).await;

    let mut unit = h.engine.unit_of_work(&authz).await.expect("a unit opens");
    let evidence = unit
        .ingest_fact(FactWrite::new(
            owner,
            "goal-targeting",
            &NoteV1 {
                notebook_id: notebook,
                key: "evidence".to_owned(),
                body: "what the goal rests on".to_owned(),
                written_at: time::OffsetDateTime::now_utc(),
            },
        ))
        .await
        .expect("the evidence note is admitted")
        .memory_id;
    let mut perspective = async |label: &str| -> MemoryId {
        unit.derive_memory(DerivedMemory::interpretation(
            MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
            owner,
            label,
            interpretation(label, evidence),
        ))
        .await
        .expect("the perspective is derived")
        .memory_id
    };
    let alice_self = perspective("Alice").await;
    let bob_self = perspective("Bob").await;
    assert_ne!(alice_self, bob_self);

    // Alice's Perspective motivates this Goal; Bob's does not.
    let goal = unit
        .create_goal(GoalCreateRequest {
            owner,
            topology: GoalTopologyWrite::new(
                vec![GoalMotivationRef::perspective(alice_self)],
                Vec::new(),
                vec![GoalEvidenceRef::new(evidence)],
            )
            .expect("fixture topology is valid"),
            title: "goal targeting".to_owned(),
            text: "goal targeting".to_owned(),
            payload: SimpleTextGoalV1 {},
            request_id: IdempotencyKey::new("goal-targeting").expect("valid request id"),
            authorship: GoalAuthorship::User,
            author_self_perspective_id: None,
        })
        .await
        .expect("the goal is created");
    let second_goal = unit
        .create_goal(GoalCreateRequest {
            owner,
            topology: GoalTopologyWrite::new(
                vec![
                    GoalMotivationRef::perspective(alice_self),
                    GoalMotivationRef::perspective(bob_self),
                ],
                Vec::new(),
                Vec::new(),
            )
            .expect("multiple motivations are valid"),
            title: "shared direction".to_owned(),
            text: "both perspectives motivated this direction".to_owned(),
            payload: SimpleTextGoalV1 {},
            request_id: IdempotencyKey::new("goal-targeting-shared").expect("valid request id"),
            authorship: GoalAuthorship::User,
            author_self_perspective_id: None,
        })
        .await
        .expect("a goal can have several motivations and no evidence");
    let guided = unit
        .derive_memory(DerivedMemory::guided_interpretation(
            MemoryTarget::Revision(alice_self),
            owner,
            "Alice's revised stance",
            interpretation("Alice's revised stance", evidence),
            [goal.goal_id, second_goal.goal_id],
        ))
        .await
        .expect("a Perspective can record Goals created in the same transaction");
    unit.commit()
        .await
        .expect("the feedback loop commits atomically");
    assert_ne!(guided.memory_id, alice_self);

    let motivating: Vec<Uuid> =
        sqlx::query_scalar("SELECT motivated_by FROM proxiima_core.goal WHERE t = $1")
            .bind(goal.goal_id.into_inner())
            .fetch_one(&h.pool)
            .await
            .expect("the goal row");
    assert_eq!(motivating, vec![alice_self.into_inner()]);
    assert!(!motivating.contains(&bob_self.into_inner()));

    let later_goal = h
        .engine
        .transition_goal(
            &authz,
            &GoalTransitionRequest {
                owner,
                prior_goal_id: goal.goal_id,
                transition: GoalTransitionKind::Pause {
                    pending: PendingWork::Keep,
                },
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("pause-guiding-goal").expect("valid request id"),
                author_self_perspective_id: None,
            },
        )
        .await
        .expect("a guiding Goal can advance without retargeting the Perspective");
    assert_ne!(later_goal.goal_id, goal.goal_id);

    let stored_guidance: Vec<Uuid> =
        sqlx::query_scalar("SELECT guided_by FROM proxiima_core.memory WHERE t = $1")
            .bind(guided.memory_id.into_inner())
            .fetch_one(&h.pool)
            .await
            .expect("the guided version");
    assert_eq!(
        stored_guidance,
        vec![goal.goal_id.into_inner(), second_goal.goal_id.into_inner()]
    );
    assert!(!stored_guidance.contains(&later_goal.goal_id.into_inner()));
    let original_guidance: Vec<Uuid> =
        sqlx::query_scalar("SELECT guided_by FROM proxiima_core.memory WHERE t = $1")
            .bind(alice_self.into_inner())
            .fetch_one(&h.pool)
            .await
            .expect("the original Perspective remains intact");
    assert!(original_guidance.is_empty());
    let successor_motivations: Vec<Uuid> =
        sqlx::query_scalar("SELECT motivated_by FROM proxiima_core.goal WHERE t = $1")
            .bind(later_goal.goal_id.into_inner())
            .fetch_one(&h.pool)
            .await
            .expect("the Goal successor retains its motivation");
    assert_eq!(successor_motivations, motivating);

    let destination = GroupId::new(Uuid::now_v7());
    sqlx::query("INSERT INTO proxiima_core.owners(owner_id, kind) VALUES ($1, 'group')")
        .bind(destination.into_inner())
        .execute(&h.pool)
        .await
        .expect("destination exists");
    sqlx::query("INSERT INTO proxiima_core.group_memberships(group_id, member_user_id, relation) VALUES ($1, $2, 'admin')")
        .bind(destination.into_inner())
        .bind(user.into_inner())
        .execute(&h.pool)
        .await
        .expect("the caller manages the destination");
    for memory in [bob_self, guided.memory_id] {
        let error = h
            .engine
            .transfer_to_owner(&authz, EntityId::Memory(memory), destination.into())
            .await
            .expect_err("transfer must preserve live same-owner causal relations");
        assert!(error.to_string().contains("causal relations"), "{error}");
        let retained_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxiima_core.memory WHERE t = $1")
                .bind(memory.into_inner())
                .fetch_one(&h.pool)
                .await
                .expect("source survives refusal");
        assert_eq!(retained_owner, owner.into_inner());
    }
    h.pool.close().await;
}

fn interpretation(claim: &str, evidence: MemoryId) -> InterpretationV1 {
    InterpretationV1 {
        claim: claim.to_owned(),
        confidence: 90,
        subject_memory_ids: vec![evidence.into_inner()],
        subject_kinds: vec![InterpretationSubjectKind::Fact],
        model_id: "test".to_owned(),
        client_name: "test".to_owned(),
        client_version: "1".to_owned(),
    }
}

//! A Goal names the Perspective it inspires on its own row, and that column
//! points at that Perspective and no other. Wake execution is external to
//! Proxima.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "each suite uses part of the shared fixture")]
mod example_engine;

use example_engine::ExampleEngine;
use proxima_core::verbs::goal_write::{
    GoalAssignmentTarget, GoalAuthorship, GoalCreateRequest, GoalEvidenceRef, GoalTopologyWrite,
    IdempotencyKey,
};
use proxima_core::{
    AuthPath, AuthzContext, DerivedMemory, FactWrite, InterpretationSubjectKind, InterpretationV1,
    MemoryId, MemoryTarget, OwnerId, SeriesHandle, SimpleTextGoalV1, UserId,
};
use proxima_example::NoteV1;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn goal_assignment_targets_only_intended_engineer_instance() {
    let h = ExampleEngine::new("proxima_goal_targeting", |_| {}).await;
    let user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(user);
    let authz = proxima_core::test_fixtures::authenticated_context(AuthzContext::for_subject(
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
            InterpretationV1 {
                claim: label.to_owned(),
                confidence: 90,
                subject_memory_ids: vec![evidence.into_inner()],
                subject_kinds: vec![InterpretationSubjectKind::Fact],
                model_id: "test".to_owned(),
                client_name: "test".to_owned(),
                client_version: "1".to_owned(),
            },
        ))
        .await
        .expect("the perspective is derived")
        .memory_id
    };
    let alice_self = perspective("Alice").await;
    let bob_self = perspective("Bob").await;
    assert_ne!(alice_self, bob_self);

    // An Active Goal assigned to Alice's Perspective.
    let goal = unit
        .create_goal(GoalCreateRequest {
            owner,
            topology: GoalTopologyWrite::new(
                Some(GoalAssignmentTarget::perspective(alice_self)),
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
    unit.commit().await.expect("the unit commits");

    // The statement lives on the goal row.
    let assigned: Uuid =
        sqlx::query_scalar("SELECT assignment_t FROM proxima_core.goal WHERE t = $1")
            .bind(goal.goal_id.into_inner())
            .fetch_one(&h.pool)
            .await
            .expect("the goal row");
    assert_eq!(assigned, alice_self.into_inner());
    assert_ne!(assigned, bob_self.into_inner());
    h.pool.close().await;
}

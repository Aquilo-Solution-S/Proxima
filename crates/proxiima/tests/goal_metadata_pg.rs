//! Engine-path acceptance for Goal metadata edits through a Goal audience.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code)]
mod example_engine;

use example_engine::ExampleEngine;
use proxiima_core::test_fixtures::authenticated_context;
use proxiima_core::verbs::goal_write::{
    ChildGoalDraft, GoalAuthorship, GoalCreateRequest, GoalPayloadWrite, GoalTopologyWrite,
    IdempotencyKey,
};
use proxiima_core::{
    AuthPath, AuthzContext, GoalAudienceGrantInput, GoalDecomposeRequest, GoalHandle, GoalId,
    GoalModifyRequest as EngineGoalModifyRequest, OwnerId, PendingWork, Role, SimpleTextGoalV1,
    UserId,
};
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one live-target scenario covers carried evidence and motivation"
)]
async fn guest_metadata_edit_refuses_forgotten_carried_targets() {
    if proxiima_pg_testkit::admin_url_or_skip().is_none() {
        return;
    }
    let h = ExampleEngine::new("proxiima_goal_metadata_forgotten", |_| {}).await;
    let author = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(author);
    let author_authz = direct(author);
    let guest = UserId::new(Uuid::now_v7());
    let notebook = h.notebook(&owner).await;
    let mut unit = h.engine.unit_of_work(&author_authz).await.unwrap();
    let fact = unit
        .ingest_fact(proxiima_core::FactWrite::new(
            owner,
            "metadata-grounding",
            &proxiima_example::NoteV1 {
                notebook_id: notebook,
                key: "grounding".into(),
                body: "private evidence".into(),
                written_at: time::OffsetDateTime::now_utc(),
            },
        ))
        .await
        .unwrap()
        .memory_id;
    let perspective = unit
        .derive_memory(proxiima_core::DerivedMemory::interpretation(
            proxiima_core::MemoryTarget::Series(proxiima_core::SeriesHandle::new(Uuid::now_v7())),
            owner,
            "motivation",
            proxiima_core::InterpretationV1 {
                claim: "motivation".into(),
                confidence: 90,
                subject_memory_ids: vec![fact.into_inner()],
                subject_kinds: vec![proxiima_core::InterpretationSubjectKind::Fact],
                model_id: "test".into(),
                client_name: "test".into(),
                client_version: "1".into(),
            },
        ))
        .await
        .unwrap()
        .memory_id;
    let goal = unit
        .create_goal(GoalCreateRequest {
            owner,
            topology: GoalTopologyWrite::new(
                vec![proxiima_core::GoalMotivationRef::perspective(perspective)],
                Vec::new(),
                vec![proxiima_core::GoalEvidenceRef::new(fact)],
            )
            .unwrap(),
            title: "live sources".into(),
            text: "progress".into(),
            payload: SimpleTextGoalV1 {},
            request_id: IdempotencyKey::new("live-sources").unwrap(),
            authorship: GoalAuthorship::User,
            author_self_perspective_id: None,
        })
        .await
        .unwrap();
    unit.commit().await.unwrap();
    h.engine
        .grant_goal_access(
            &author_authz,
            &GoalAudienceGrantInput {
                goal: goal.goal_handle,
                subject: guest,
                role: Role::new(
                    proxiima_core::AccessCeiling::Goal,
                    proxiima_core::AccessCeiling::Goal,
                    false,
                )
                .unwrap(),
                expires_at: None,
            },
        )
        .await
        .unwrap();
    for target in [fact, perspective] {
        h.engine
            .forget_memory(&author_authz, owner, target)
            .await
            .unwrap();
        let result = h
            .engine
            .modify_goal(
                &direct(guest),
                &EngineGoalModifyRequest {
                    owner,
                    prior_goal_id: goal.goal_id,
                    replacement: GoalPayloadWrite::from_payload(
                        "edited",
                        "progress",
                        SimpleTextGoalV1 {},
                    )
                    .unwrap(),
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new(format!("forgotten-{}", target.into_inner()))
                        .unwrap(),
                    evidence: None,
                    motivated_by: None,
                    author_self_perspective_id: None,
                },
            )
            .await;
        assert!(
            result.is_err(),
            "guest edit must refuse forgotten carried target {target:?}"
        );
        h.engine
            .restore_memory(&author_authz, owner, target)
            .await
            .unwrap();
    }
    h.pool.close().await;
}

fn direct(subject: UserId) -> AuthzContext {
    authenticated_context(AuthzContext::for_subject(subject, AuthPath::HostBearer))
}

async fn goal(h: &ExampleEngine, owner: OwnerId, authz: &AuthzContext) -> (GoalHandle, GoalId) {
    goal_named(h, owner, authz, "owner title", "metadata-goal").await
}

async fn goal_named(
    h: &ExampleEngine,
    owner: OwnerId,
    authz: &AuthzContext,
    title: &str,
    request_id: &str,
) -> (GoalHandle, GoalId) {
    let mut unit = h.engine.unit_of_work(authz).await.expect("unit");
    let outcome = unit
        .create_goal(GoalCreateRequest {
            owner,
            topology: GoalTopologyWrite::new(Vec::new(), Vec::new(), Vec::new()).unwrap(),
            title: title.into(),
            text: "owner progress".into(),
            payload: SimpleTextGoalV1 {},
            request_id: IdempotencyKey::new(request_id).unwrap(),
            authorship: GoalAuthorship::User,
            author_self_perspective_id: None,
        })
        .await
        .expect("goal");
    unit.commit().await.expect("commit");
    (outcome.goal_handle, outcome.goal_id)
}

#[tokio::test(flavor = "multi_thread")]
async fn goal_access_regrant_updates_granted_by_and_ceilings() {
    if proxiima_pg_testkit::admin_url_or_skip().is_none() {
        return;
    }
    let h = ExampleEngine::new("proxiima_goal_metadata_grant_provenance", |_| {}).await;
    let alice = UserId::new(Uuid::now_v7());
    let alice_owner = OwnerId::from(alice);
    let alice_authz = direct(alice);
    let bob = UserId::new(Uuid::now_v7());
    let carol = UserId::new(Uuid::now_v7());
    let (goal, _) = goal(&h, alice_owner, &alice_authz).await;

    h.engine
        .grant_goal_access(
            &alice_authz,
            &GoalAudienceGrantInput {
                goal,
                subject: bob,
                role: Role::new(
                    proxiima_core::AccessCeiling::Goal,
                    proxiima_core::AccessCeiling::Goal,
                    true,
                )
                .unwrap(),
                expires_at: None,
            },
        )
        .await
        .expect("Alice grants Bob managing Goal access");
    h.engine
        .grant_goal_access(
            &alice_authz,
            &GoalAudienceGrantInput {
                goal,
                subject: carol,
                role: Role::viewer(),
                expires_at: None,
            },
        )
        .await
        .expect("Alice grants Carol viewer access");

    h.engine
        .grant_goal_access(
            &direct(bob),
            &GoalAudienceGrantInput {
                goal,
                subject: carol,
                role: Role::editor(),
                expires_at: None,
            },
        )
        .await
        .expect("Bob upgrades Carol to editor access");

    let row: (Uuid, i16, i16, bool) = sqlx::query_as(
        "SELECT granted_by, read_ceiling, write_ceiling, manage
           FROM proxiima_core.goal_access_grant
          WHERE goal_handle = $1 AND subject_id = $2",
    )
    .bind(goal.into_inner())
    .bind(carol.into_inner())
    .fetch_one(&h.pool)
    .await
    .expect("Carol's regranted row");
    assert_eq!(row.0, bob.into_inner(), "regrant records its actual actor");
    assert_eq!((row.1, row.2, row.3), (4, 3, false));
    h.pool.close().await;
}

fn assert_redacted_child_error(error: impl std::fmt::Display, child: &GoalId, title: &str) {
    let text = error.to_string();
    assert!(
        !text.contains(&child.into_inner().to_string()),
        "child id leaked: {text}"
    );
    assert!(!text.contains(title), "child title leaked: {text}");
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "metadata acceptance keeps edit, replay, and revocation evidence together"
)]
async fn g4_guest_edits_metadata_and_revocation_rechecks_replay() {
    if proxiima_pg_testkit::admin_url_or_skip().is_none() {
        return;
    }
    let h = ExampleEngine::new("proxiima_goal_metadata", |_| {}).await;
    let owner_user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(owner_user);
    let owner_authz = direct(owner_user);
    let guest = UserId::new(Uuid::now_v7());
    let guest_authz = direct(guest);
    let (handle, prior) = goal(&h, owner, &owner_authz).await;

    h.engine
        .grant_goal_access(
            &owner_authz,
            &GoalAudienceGrantInput {
                goal: handle,
                subject: guest,
                role: Role::new(
                    proxiima_core::AccessCeiling::Goal,
                    proxiima_core::AccessCeiling::Goal,
                    false,
                )
                .unwrap(),
                expires_at: None,
            },
        )
        .await
        .expect("grant G4");
    let viewer = UserId::new(Uuid::now_v7());
    h.engine
        .grant_goal_access(
            &owner_authz,
            &GoalAudienceGrantInput {
                goal: handle,
                subject: viewer,
                role: Role::viewer(),
                expires_at: None,
            },
        )
        .await
        .expect("grant viewer");
    let viewer_result = h
        .engine
        .modify_goal(
            &direct(viewer),
            &EngineGoalModifyRequest {
                owner,
                prior_goal_id: prior,
                replacement: GoalPayloadWrite::from_payload(
                    "viewer title",
                    "viewer progress",
                    SimpleTextGoalV1 {},
                )
                .unwrap(),
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("viewer-edit").unwrap(),
                evidence: None,
                motivated_by: None,
                author_self_perspective_id: None,
            },
        )
        .await;
    assert!(viewer_result.is_err(), "viewer cannot edit");

    let modified = h
        .engine
        .modify_goal(
            &guest_authz,
            &EngineGoalModifyRequest {
                owner,
                prior_goal_id: prior,
                replacement: GoalPayloadWrite::from_payload(
                    "guest title",
                    "guest progress",
                    SimpleTextGoalV1 {},
                )
                .unwrap(),
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("guest-edit").unwrap(),
                evidence: None,
                motivated_by: None,
                author_self_perspective_id: None,
            },
        )
        .await;
    assert!(modified.is_ok(), "G4 guest metadata edit: {modified:?}");
    let modified = modified.unwrap();
    let paused = h
        .engine
        .transition_goal(
            &guest_authz,
            &proxiima_core::GoalTransitionRequest {
                owner,
                prior_goal_id: modified.goal_id,
                transition: proxiima_core::verbs::goal_write::GoalTransitionKind::Pause {
                    pending: PendingWork::Keep,
                },
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("guest-pause").unwrap(),
                author_self_perspective_id: None,
            },
        )
        .await
        .expect("guest pause");
    let resumed = h
        .engine
        .transition_goal(
            &guest_authz,
            &proxiima_core::GoalTransitionRequest {
                owner,
                prior_goal_id: paused.goal_id,
                transition: proxiima_core::verbs::goal_write::GoalTransitionKind::Resume,
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("guest-resume").unwrap(),
                author_self_perspective_id: None,
            },
        )
        .await
        .expect("guest resume");
    h.engine
        .mark_goal_achieved(
            &guest_authz,
            &proxiima_core::GoalMarkAchievedRequest {
                owner,
                prior_goal_id: resumed.goal_id,
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("guest-achieve").unwrap(),
                evidence: Vec::new(),
                author_self_perspective_id: None,
            },
        )
        .await
        .expect("guest achieve");

    h.engine
        .revoke_goal_access(&owner_authz, handle, guest)
        .await
        .expect("revoke guest");
    let replay = h
        .engine
        .modify_goal(
            &guest_authz,
            &EngineGoalModifyRequest {
                owner,
                prior_goal_id: prior,
                replacement: GoalPayloadWrite::from_payload(
                    "guest title",
                    "guest progress",
                    SimpleTextGoalV1 {},
                )
                .unwrap(),
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("guest-edit").unwrap(),
                evidence: None,
                motivated_by: None,
                author_self_perspective_id: None,
            },
        )
        .await;
    assert!(replay.is_err(), "revocation must precede replay");
    h.pool.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn g4_guest_cannot_discover_open_child_on_achievement_or_abandonment() {
    if proxiima_pg_testkit::admin_url_or_skip().is_none() {
        return;
    }
    let h = ExampleEngine::new("proxiima_goal_metadata_redaction", |_| {}).await;
    let owner_user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(owner_user);
    let owner_authz = direct(owner_user);
    let guest = UserId::new(Uuid::now_v7());
    let guest_authz = direct(guest);
    let (parent_handle, parent_id) = goal_named(
        &h,
        owner,
        &owner_authz,
        "public parent title",
        "redaction-parent",
    )
    .await;
    let decomposition = h
        .engine
        .decompose_goal(
            &owner_authz,
            &GoalDecomposeRequest {
                owner,
                parent_goal_id: parent_id,
                authorship: GoalAuthorship::User,
                topology: GoalTopologyWrite::new(Vec::new(), Vec::new(), Vec::new()).unwrap(),
                children: vec![ChildGoalDraft {
                    payload: GoalPayloadWrite::from_payload(
                        "private child title",
                        "private child progress",
                        SimpleTextGoalV1 {},
                    )
                    .unwrap(),
                    evidence: Vec::new(),
                    request_id: IdempotencyKey::new("redaction-child").unwrap(),
                }],
                author_self_perspective_id: None,
            },
        )
        .await
        .expect("owner decomposes parent");
    let child_id = decomposition.children[0].outcome.goal_id;
    h.engine
        .grant_goal_access(
            &owner_authz,
            &GoalAudienceGrantInput {
                goal: parent_handle,
                subject: guest,
                role: Role::new(
                    proxiima_core::AccessCeiling::Goal,
                    proxiima_core::AccessCeiling::Goal,
                    false,
                )
                .unwrap(),
                expires_at: None,
            },
        )
        .await
        .expect("grant parent G4 only");

    let achieved = h
        .engine
        .mark_goal_achieved(
            &guest_authz,
            &proxiima_core::GoalMarkAchievedRequest {
                owner,
                prior_goal_id: parent_id,
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("guest-achieve-parent").unwrap(),
                evidence: Vec::new(),
                author_self_perspective_id: None,
            },
        )
        .await
        .expect_err("guest achievement must be refused");
    assert_redacted_child_error(achieved, &child_id, "private child title");

    let abandoned = h
        .engine
        .transition_goal(
            &guest_authz,
            &proxiima_core::GoalTransitionRequest {
                owner,
                prior_goal_id: parent_id,
                transition: proxiima_core::verbs::goal_write::GoalTransitionKind::Abandon,
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("guest-abandon-parent").unwrap(),
                author_self_perspective_id: None,
            },
        )
        .await
        .expect_err("guest abandonment must be refused");
    assert_redacted_child_error(abandoned, &child_id, "private child title");
    h.pool.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "negative metadata acceptance keeps unrelated mutation refusals together"
)]
async fn g4_guest_cannot_create_decompose_edit_unrelated_or_change_motivation() {
    if proxiima_pg_testkit::admin_url_or_skip().is_none() {
        return;
    }
    let h = ExampleEngine::new("proxiima_goal_metadata_denials", |_| {}).await;
    let owner_user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(owner_user);
    let owner_authz = direct(owner_user);
    let guest = UserId::new(Uuid::now_v7());
    let guest_authz = direct(guest);
    let (parent_handle, parent_id) = goal(&h, owner, &owner_authz).await;
    let (_, unrelated_id) =
        goal_named(&h, owner, &owner_authz, "unrelated", "unrelated-goal").await;
    h.engine
        .grant_goal_access(
            &owner_authz,
            &GoalAudienceGrantInput {
                goal: parent_handle,
                subject: guest,
                role: Role::new(
                    proxiima_core::AccessCeiling::Goal,
                    proxiima_core::AccessCeiling::Goal,
                    false,
                )
                .unwrap(),
                expires_at: None,
            },
        )
        .await
        .expect("grant parent G4 only");

    let mut unit = h
        .engine
        .unit_of_work(&guest_authz)
        .await
        .expect("guest unit");
    let created = unit
        .create_goal(GoalCreateRequest {
            owner,
            topology: GoalTopologyWrite::new(Vec::new(), Vec::new(), Vec::new()).unwrap(),
            title: "guest-created".into(),
            text: "must not exist".into(),
            payload: SimpleTextGoalV1 {},
            request_id: IdempotencyKey::new("guest-create-denied").unwrap(),
            authorship: GoalAuthorship::User,
            author_self_perspective_id: None,
        })
        .await;
    assert!(
        created.is_err(),
        "G4 guest cannot create Goals: {created:?}"
    );
    drop(unit);

    let decomposed = h
        .engine
        .decompose_goal(
            &guest_authz,
            &GoalDecomposeRequest {
                owner,
                parent_goal_id: parent_id,
                authorship: GoalAuthorship::User,
                topology: GoalTopologyWrite::new(Vec::new(), Vec::new(), Vec::new()).unwrap(),
                children: vec![ChildGoalDraft {
                    payload: GoalPayloadWrite::from_payload(
                        "guest child",
                        "must not exist",
                        SimpleTextGoalV1 {},
                    )
                    .unwrap(),
                    evidence: Vec::new(),
                    request_id: IdempotencyKey::new("guest-decompose-denied").unwrap(),
                }],
                author_self_perspective_id: None,
            },
        )
        .await;
    assert!(
        decomposed.is_err(),
        "G4 guest cannot decompose: {decomposed:?}"
    );

    let unrelated = h
        .engine
        .modify_goal(
            &guest_authz,
            &EngineGoalModifyRequest {
                owner,
                prior_goal_id: unrelated_id,
                replacement: GoalPayloadWrite::from_payload(
                    "changed",
                    "must not change",
                    SimpleTextGoalV1 {},
                )
                .unwrap(),
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("guest-unrelated-edit").unwrap(),
                evidence: None,
                motivated_by: None,
                author_self_perspective_id: None,
            },
        )
        .await;
    assert!(
        unrelated.is_err(),
        "G4 guest cannot edit unrelated Goal: {unrelated:?}"
    );

    let motivation = h
        .engine
        .modify_goal(
            &guest_authz,
            &EngineGoalModifyRequest {
                owner,
                prior_goal_id: parent_id,
                replacement: GoalPayloadWrite::from_payload(
                    "guest title",
                    "guest progress",
                    SimpleTextGoalV1 {},
                )
                .unwrap(),
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("guest-motivation-edit").unwrap(),
                evidence: None,
                motivated_by: Some(Vec::new()),
                author_self_perspective_id: None,
            },
        )
        .await;
    assert!(
        motivation.is_err(),
        "G4 guest cannot change motivation: {motivation:?}"
    );
    h.pool.close().await;
}

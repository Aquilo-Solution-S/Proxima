#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "shared fixture helpers")]
mod example_engine;

use example_engine::ExampleEngine;
use proxiima_core::test_fixtures::authenticated_context;
use proxiima_core::verbs::goal_write::{
    GoalAuthorship, GoalCreateRequest, GoalTopologyWrite, GoalTransitionKind, IdempotencyKey,
};
use proxiima_core::verbs::idempotency::{ContentFingerprint, KeyScope, RequestKey};
use proxiima_core::{
    AuthPath, AuthzContext, DerivedMemory, FactWrite, GoalAudienceGrantInput, GoalPublicationInput,
    GoalTransitionRequest, GroupId, InterpretationV1, MemoryDestination, MemoryId, MemoryTarget,
    OwnerId, OwnerRoles, PendingWork, PublicationUse, Relation, Role, SeriesHandle,
    SimpleTextGoalV1, UserId,
};
use proxiima_example::NoteV1;
use proxiima_storage_pg::PgPlatformScope;
use uuid::Uuid;

fn auth(user: UserId) -> AuthzContext {
    authenticated_context(AuthzContext::for_subject(user, AuthPath::HostBearer))
}

fn interpretation(fact: MemoryId, text: &str) -> InterpretationV1 {
    InterpretationV1 {
        claim: text.into(),
        confidence: 90,
        subject_memory_ids: vec![fact.into_inner()],
        subject_kinds: vec![proxiima_core::InterpretationSubjectKind::Fact],
        model_id: "goal-work-test".into(),
        client_name: "goal-work-test".into(),
        client_version: "1".into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one acceptance scenario keeps external edit and revision receipts together"
)]
async fn external_goal_editor_can_create_and_revise_bound_perspective() {
    let h = ExampleEngine::new("proxiima_goal_work", |_| {}).await;
    let owner_user = UserId::new(Uuid::now_v7());
    let writer_user = UserId::new(Uuid::now_v7());
    let viewer_user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(owner_user);
    let owner_auth = auth(owner_user);
    let writer_auth = auth(writer_user);
    let viewer_auth = auth(viewer_user);
    let notebook = h.notebook(&owner).await;
    let mut unit = h.engine.unit_of_work(&owner_auth).await.expect("unit");
    let fact = unit
        .ingest_fact(FactWrite::new(
            owner,
            "goal-work",
            &NoteV1 {
                notebook_id: notebook,
                key: "brief".into(),
                body: "source".into(),
                written_at: time::OffsetDateTime::now_utc(),
            },
        ))
        .await
        .expect("fact")
        .memory_id;
    let goal = unit
        .create_goal(GoalCreateRequest {
            owner,
            topology: GoalTopologyWrite::new(Vec::new(), Vec::new(), Vec::new()).expect("topology"),
            title: "Customer A".into(),
            text: "work".into(),
            payload: SimpleTextGoalV1 {},
            request_id: IdempotencyKey::new("goal-work").expect("key"),
            authorship: GoalAuthorship::User,
            author_self_perspective_id: None,
        })
        .await
        .expect("Goal")
        .goal_handle;
    unit.commit().await.expect("commit");
    h.engine
        .publish_to_goal(
            &owner_auth,
            &GoalPublicationInput {
                goal,
                memory: fact,
                permitted_use: PublicationUse::Read,
            },
        )
        .await
        .expect("publish");
    h.engine
        .grant_goal_access(
            &owner_auth,
            &GoalAudienceGrantInput {
                goal,
                subject: writer_user,
                role: Role::editor(),
                expires_at: None,
            },
        )
        .await
        .expect("grant");
    h.engine
        .grant_goal_access(
            &owner_auth,
            &GoalAudienceGrantInput {
                goal,
                subject: viewer_user,
                role: Role::viewer(),
                expires_at: None,
            },
        )
        .await
        .expect("viewer grant");
    assert!(
        h.engine
            .derive_memory(
                &viewer_auth,
                DerivedMemory::interpretation(
                    MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                    MemoryDestination::WorkingGoal(goal),
                    "viewer must not write",
                    interpretation(fact, "viewer"),
                ),
            )
            .await
            .is_err()
    );
    let first = h
        .engine
        .derive_memory(
            &writer_auth,
            DerivedMemory::interpretation(
                MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                MemoryDestination::WorkingGoal(goal),
                "work",
                interpretation(fact, "first"),
            ),
        )
        .await
        .expect("first work")
        .memory_id;
    let second = h
        .engine
        .derive_memory(
            &writer_auth,
            DerivedMemory::interpretation(
                MemoryTarget::Revision(first),
                MemoryDestination::WorkingGoal(goal),
                "work",
                interpretation(fact, "second"),
            ),
        )
        .await
        .expect("second work")
        .memory_id;
    assert_ne!(first, second);
    let bound: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM proxiima_core.goal_work_binding w JOIN proxiima_core.memory_head h ON h.handle=w.memory_handle WHERE w.goal_handle=$1 AND h.t=$2 AND w.source_owner=$3)").bind(goal.into_inner()).bind(second.into_inner()).bind(owner.into_inner()).fetch_one(&h.pool).await.expect("binding");
    assert!(bound);
    h.engine
        .revoke_goal_access(&owner_auth, goal, writer_user)
        .await
        .expect("revoke writer");
    assert!(
        h.engine
            .derive_memory(
                &writer_auth,
                DerivedMemory::interpretation(
                    MemoryTarget::Revision(second),
                    MemoryDestination::WorkingGoal(goal),
                    "revoked",
                    interpretation(fact, "revoked")
                )
            )
            .await
            .is_err()
    );
    h.engine
        .grant_goal_access(
            &owner_auth,
            &GoalAudienceGrantInput {
                goal,
                subject: writer_user,
                role: Role::editor(),
                expires_at: None,
            },
        )
        .await
        .expect("restore writer");
    let unbound = h
        .engine
        .derive_memory(
            &owner_auth,
            DerivedMemory::interpretation(
                MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                MemoryDestination::Owner(owner),
                "unbound",
                interpretation(fact, "unbound"),
            ),
        )
        .await
        .expect("unbound owner series")
        .memory_id;
    assert!(
        h.engine
            .derive_memory(
                &writer_auth,
                DerivedMemory::interpretation(
                    MemoryTarget::Revision(unbound),
                    MemoryDestination::WorkingGoal(goal),
                    "unbound revision",
                    interpretation(fact, "should fail")
                )
            )
            .await
            .is_err()
    );
    let _paused = h
        .engine
        .transition_goal(
            &owner_auth,
            &GoalTransitionRequest {
                owner,
                prior_goal_id: proxiima_core::GoalId::new(
                    sqlx::query_scalar::<_, Uuid>(
                        "SELECT t FROM proxiima_core.goal_head WHERE handle=$1",
                    )
                    .bind(goal.into_inner())
                    .fetch_one(&h.pool)
                    .await
                    .expect("current Goal version"),
                ),
                transition: GoalTransitionKind::Pause {
                    pending: PendingWork::Keep,
                },
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("pause-goal-work").expect("key"),
                author_self_perspective_id: None,
            },
        )
        .await
        .expect("pause Goal");
    assert!(
        h.engine
            .derive_memory(
                &writer_auth,
                DerivedMemory::interpretation(
                    MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                    MemoryDestination::WorkingGoal(goal),
                    "paused Goal must not write",
                    interpretation(fact, "paused"),
                ),
            )
            .await
            .is_err()
    );
    h.pool.close().await;
}

async fn identity_goal(
    h: &ExampleEngine,
    owner_auth: &AuthzContext,
    owner: OwnerId,
    first_writer: UserId,
    second_writer: UserId,
    fact: MemoryId,
    title: &str,
) -> proxiima_core::GoalHandle {
    let mut unit = h.engine.unit_of_work(owner_auth).await.expect("goal unit");
    let goal = unit
        .create_goal(GoalCreateRequest {
            owner,
            topology: GoalTopologyWrite::new(Vec::new(), Vec::new(), Vec::new()).expect("topology"),
            title: title.into(),
            text: "identity work".into(),
            payload: SimpleTextGoalV1 {},
            request_id: IdempotencyKey::new(title).expect("request key"),
            authorship: GoalAuthorship::User,
            author_self_perspective_id: None,
        })
        .await
        .expect("Goal")
        .goal_handle;
    unit.commit().await.expect("commit Goal");
    h.engine
        .publish_to_goal(
            owner_auth,
            &GoalPublicationInput {
                goal,
                memory: fact,
                permitted_use: PublicationUse::Read,
            },
        )
        .await
        .expect("publish fact");
    for subject in [first_writer, second_writer] {
        h.engine
            .grant_goal_access(
                owner_auth,
                &GoalAudienceGrantInput {
                    goal,
                    subject,
                    role: Role::editor(),
                    expires_at: None,
                },
            )
            .await
            .expect("grant editor");
    }
    goal
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one acceptance scenario keeps Goal and caller identity evidence together"
)]
async fn goal_work_identity_is_scoped_by_goal_and_caller() {
    let h = ExampleEngine::new("proxiima_goal_work_identity", |_| {}).await;
    let owner_user = UserId::new(Uuid::now_v7());
    let first_writer = UserId::new(Uuid::now_v7());
    let second_writer = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(owner_user);
    let owner_auth = auth(owner_user);
    let first_auth = auth(first_writer);
    let second_auth = auth(second_writer);
    let notebook = h.notebook(&owner).await;
    let mut unit = h.engine.unit_of_work(&owner_auth).await.expect("fact unit");
    let fact = unit
        .ingest_fact(FactWrite::new(
            owner,
            "goal-work-identity",
            &NoteV1 {
                notebook_id: notebook,
                key: "identity fact".into(),
                body: "identity source".into(),
                written_at: time::OffsetDateTime::now_utc(),
            },
        ))
        .await
        .expect("fact")
        .memory_id;
    unit.commit().await.expect("commit fact");
    let goal_a = identity_goal(
        &h,
        &owner_auth,
        owner,
        first_writer,
        second_writer,
        fact,
        "identity-a",
    )
    .await;
    let goal_b = identity_goal(
        &h,
        &owner_auth,
        owner,
        first_writer,
        second_writer,
        fact,
        "identity-b",
    )
    .await;

    let keyless = |goal| {
        DerivedMemory::interpretation(
            MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
            MemoryDestination::WorkingGoal(goal),
            "same keyless work",
            interpretation(fact, "same keyless claim"),
        )
    };
    let keyless_a = h
        .engine
        .derive_memory(&first_auth, keyless(goal_a))
        .await
        .expect("Goal A keyless");
    let keyless_b = h
        .engine
        .derive_memory(&first_auth, keyless(goal_b))
        .await
        .expect("Goal B keyless");
    assert_ne!(keyless_a.memory_id, keyless_b.memory_id);

    let request = || {
        RequestKey::new(
            KeyScope::Perspective,
            IdempotencyKey::new("shared-editor-key").expect("key"),
            ContentFingerprint::builder(KeyScope::Perspective)
                .text("test", "shared editor work")
                .build(),
        )
    };
    let first = h
        .engine
        .derive_memory(
            &first_auth,
            DerivedMemory::interpretation(
                MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                MemoryDestination::WorkingGoal(goal_a),
                "shared explicit work",
                interpretation(fact, "shared explicit claim"),
            )
            .request_key(request()),
        )
        .await
        .expect("first explicit");
    let second = h
        .engine
        .derive_memory(
            &second_auth,
            DerivedMemory::interpretation(
                MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                MemoryDestination::WorkingGoal(goal_a),
                "shared explicit work",
                interpretation(fact, "shared explicit claim"),
            )
            .request_key(request()),
        )
        .await
        .expect("second explicit");
    assert_ne!(first.memory_id, second.memory_id);
    let replay = h
        .engine
        .derive_memory(
            &first_auth,
            DerivedMemory::interpretation(
                MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                MemoryDestination::WorkingGoal(goal_a),
                "shared explicit work",
                interpretation(fact, "shared explicit claim"),
            )
            .request_key(request()),
        )
        .await
        .expect("same caller replay");
    assert_eq!(replay.memory_id, first.memory_id);
    assert!(replay.idempotent_replay);
    h.pool.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn owning_group_editor_can_work_without_goal_guest_grant() {
    let h = ExampleEngine::new("proxiima_goal_group_work", |_| {}).await;
    let owner_user = UserId::new(Uuid::now_v7());
    let writer_user = UserId::new(Uuid::now_v7());
    let group = GroupId::new(Uuid::now_v7());
    let owner = OwnerId::from(group);
    let platform_pool = example_engine::fixture_pool(h.db.platform_url(), 1)
        .await
        .expect("platform pool");
    let platform = PgPlatformScope::new(platform_pool, &["proxiima_core", "proxiima_example"])
        .await
        .expect("platform scope");
    let mut tx = platform.begin().await.expect("platform tx");
    proxiima::register_group(&mut tx, group)
        .await
        .expect("register group");
    proxiima::add_group_member(&mut tx, group, owner_user.into(), Relation::Admin)
        .await
        .expect("owner group admin");
    proxiima::add_group_member(&mut tx, group, writer_user.into(), Relation::Editor)
        .await
        .expect("writer group editor");
    tx.commit().await.expect("membership commit");
    let owner_auth = authenticated_context(AuthzContext::server_resolved(
        OwnerRoles::for_subject(owner_user, [(owner, Role::admin())]).expect("owner role"),
        AuthPath::HostBearer,
    ));
    let writer_auth = authenticated_context(AuthzContext::server_resolved(
        OwnerRoles::for_subject(writer_user, [(owner, Role::editor())]).expect("writer role"),
        AuthPath::HostBearer,
    ));
    let notebook = h.notebook(&owner).await;
    let mut unit = h.engine.unit_of_work(&owner_auth).await.expect("unit");
    let fact = unit
        .ingest_fact(FactWrite::new(
            owner,
            "group-goal-work",
            &NoteV1 {
                notebook_id: notebook,
                key: "brief".into(),
                body: "group source".into(),
                written_at: time::OffsetDateTime::now_utc(),
            },
        ))
        .await
        .expect("group fact")
        .memory_id;
    let goal = unit
        .create_goal(GoalCreateRequest {
            owner,
            topology: GoalTopologyWrite::new(Vec::new(), Vec::new(), Vec::new()).expect("topology"),
            title: "Group customer".into(),
            text: "group work".into(),
            payload: SimpleTextGoalV1 {},
            request_id: IdempotencyKey::new("group-goal-work").expect("key"),
            authorship: GoalAuthorship::User,
            author_self_perspective_id: None,
        })
        .await
        .expect("group Goal")
        .goal_handle;
    unit.commit().await.expect("commit");
    h.engine
        .publish_to_goal(
            &owner_auth,
            &GoalPublicationInput {
                goal,
                memory: fact,
                permitted_use: PublicationUse::Read,
            },
        )
        .await
        .expect("group publication");
    let work = h
        .engine
        .derive_memory(
            &writer_auth,
            DerivedMemory::interpretation(
                MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                MemoryDestination::WorkingGoal(goal),
                "group work",
                interpretation(fact, "group editor"),
            ),
        )
        .await
        .expect("group editor work without guest grant");
    let bound: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM proxiima_core.goal_work_binding w JOIN proxiima_core.memory m ON m.handle=w.memory_handle WHERE w.goal_handle=$1 AND m.t=$2)",
    )
    .bind(goal.into_inner())
    .bind(work.memory_id.into_inner())
    .fetch_one(&h.pool)
    .await
    .expect("binding");
    assert!(bound);
    h.pool.close().await;
}

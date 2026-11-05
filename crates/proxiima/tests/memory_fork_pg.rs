//! Exact-version typed fork coverage.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "this suite uses the shared host fixture")]
mod example_engine;

use example_engine::ExampleEngine;
use proxiima::spiice::SpiiceBundle;
use proxiima_core::owner_inverse::{EraseAuthorization, OwnerEraseTarget, OwnerSurfaces};
use proxiima_core::storage_ports::OwnerInversePort;
use proxiima_core::test_fixtures::authenticated_context;
use proxiima_core::verbs::goal_write::{GoalAuthorship, GoalCreateRequest, GoalTopologyWrite};
use proxiima_core::verbs::idempotency::IdempotencyKey;
use proxiima_core::verbs::query::{
    EdgeFilter, EdgeReadRequest, EntityKind, QueryRequest, SupersessionStatus,
};
use proxiima_core::{
    AccessKind, AuthPath, AuthzContext, DerivedMemory, EdgeKind, EntityRef, FactWrite,
    ForkMemoryRequest, GoalAudienceGrantInput, GoalHandle, GoalPublicationInput,
    InterpretationSubjectKind, InterpretationV1, MemoryDestination, MemoryId, MemoryTarget,
    OwnerId, PublicationUse, Role, SeriesHandle, SimpleTextGoalV1, SpiiceRegistry, UserId,
};
use proxiima_example::NoteV1;
use uuid::Uuid;

fn direct(subject: UserId) -> AuthzContext {
    authenticated_context(AuthzContext::for_subject(subject, AuthPath::HostBearer))
}

fn interpretation(fact: MemoryId, claim: &str) -> InterpretationV1 {
    InterpretationV1 {
        claim: claim.into(),
        confidence: 90,
        subject_memory_ids: vec![fact.into_inner()],
        subject_kinds: vec![InterpretationSubjectKind::Fact],
        model_id: "fork-test".into(),
        client_name: "fork-test".into(),
        client_version: "1".into(),
    }
}

async fn erase_grounding_and_clean_fork(
    host: &ExampleEngine,
    source_auth: &AuthzContext,
    destination_auth: &AuthzContext,
    fact: MemoryId,
    fork_head: MemoryId,
) {
    let source_owner = OwnerId::from(source_auth.subject().expect("source subject"));
    let destination_owner = OwnerId::from(destination_auth.subject().expect("destination subject"));
    let mut erase = host
        .engine
        .unit_of_work(source_auth)
        .await
        .expect("erase unit");
    let receipt = erase
        .erase_own_series(
            proxiima_example::SPIICE_ID,
            source_owner,
            proxiima_core::verbs::own_erase::SeriesSelection::Ids(vec![fact]),
            proxiima_core::verbs::own_erase::EraseMode::Erase,
        )
        .await
        .expect("source owner erases its Fact");
    assert!(receipt.versions.contains(&fact));
    erase.commit().await.expect("commit source erase");

    let mut rejected = host
        .engine
        .unit_of_work(destination_auth)
        .await
        .expect("rejected revision unit");
    assert!(
        rejected
            .derive_memory(DerivedMemory::interpretation(
                MemoryTarget::Revision(fork_head),
                destination_owner,
                "erased evidence cannot be declared again",
                interpretation(fact, "must fail"),
            ))
            .await
            .is_err()
    );
    drop(rejected);

    let notebook = host.notebook(&destination_owner).await;
    let mut clean = host
        .engine
        .unit_of_work(destination_auth)
        .await
        .expect("cleanup revision unit");
    let replacement = clean
        .ingest_fact(FactWrite::new(
            destination_owner,
            "fork-cleanup",
            &NoteV1 {
                notebook_id: notebook,
                key: "replacement evidence".into(),
                body: "independent evidence".into(),
                written_at: time::OffsetDateTime::now_utc(),
            },
        ))
        .await
        .expect("destination evidence")
        .memory_id;
    let successor = clean
        .derive_memory(DerivedMemory::interpretation(
            MemoryTarget::Revision(fork_head),
            destination_owner,
            "replace erased grounding",
            interpretation(replacement, "independent claim"),
        ))
        .await
        .expect("erased grounding can be removed from a surviving fork")
        .memory_id;
    clean.commit().await.expect("commit independent revision");
    let mut query = QueryRequest::readable();
    query.memory_ids = vec![successor];
    let row = host
        .engine
        .query(destination_auth, &query)
        .await
        .expect("read surviving fork")
        .memories
        .into_iter()
        .next()
        .expect("surviving fork revision");
    assert!(row.refs.contains(&replacement));
    assert!(!row.refs.contains(&fact));
}

async fn create_goal(
    host: &ExampleEngine,
    authz: &AuthzContext,
    owner: OwnerId,
    key: &str,
) -> GoalHandle {
    let mut unit = host
        .engine
        .unit_of_work(authz)
        .await
        .expect("open goal unit");
    let goal = unit
        .create_goal(GoalCreateRequest {
            owner,
            topology: GoalTopologyWrite::new(Vec::new(), Vec::new(), Vec::new())
                .expect("unmotivated Goal"),
            title: key.into(),
            text: "fork destination".into(),
            payload: SimpleTextGoalV1 {},
            request_id: IdempotencyKey::new(key).expect("goal request key"),
            authorship: GoalAuthorship::User,
            author_self_perspective_id: None,
        })
        .await
        .expect("create Goal");
    unit.commit().await.expect("commit Goal");
    let handle: Uuid = sqlx::query_scalar("SELECT handle FROM proxiima_core.goal WHERE t=$1")
        .bind(goal.goal_id.into_inner())
        .fetch_one(&host.pool)
        .await
        .expect("Goal handle");
    GoalHandle::new(handle)
}

#[tokio::test(flavor = "multi_thread")]
async fn typed_perspective_fork_is_a_new_series_and_replays_by_key() {
    let host = ExampleEngine::new("proxiima_memory_fork", |_| {}).await;
    let user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(user);
    let authz = authenticated_context(AuthzContext::for_subject(user, AuthPath::HostBearer));
    let notebook = host.notebook(&owner).await;
    let mut unit = host.engine.unit_of_work(&authz).await.expect("open unit");
    let fact = unit
        .ingest_fact(FactWrite::new(
            owner,
            "fork",
            &NoteV1 {
                notebook_id: notebook,
                key: "source".into(),
                body: "source evidence".into(),
                written_at: time::OffsetDateTime::now_utc(),
            },
        ))
        .await
        .expect("fact")
        .memory_id;
    let source = unit
        .derive_memory(DerivedMemory::interpretation(
            MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
            owner,
            "source perspective",
            InterpretationV1 {
                claim: "source".into(),
                confidence: 90,
                subject_memory_ids: vec![fact.into_inner()],
                subject_kinds: vec![InterpretationSubjectKind::Fact],
                model_id: "test".into(),
                client_name: "test".into(),
                client_version: "1".into(),
            },
        ))
        .await
        .expect("perspective")
        .memory_id;
    unit.commit().await.expect("commit source");
    let request = ForkMemoryRequest {
        source,
        destination: MemoryDestination::Owner(owner),
        request_id: IdempotencyKey::new("fork-key").expect("key"),
    };
    let first = host
        .engine
        .fork_memory(&authz, &request)
        .await
        .expect("fork");
    assert_ne!(first.memory_id, source);
    assert_ne!(first.handle.into_inner(), source.into_inner());
    assert_eq!(first.forked_from, source);
    let replay = host
        .engine
        .fork_memory(&authz, &request)
        .await
        .expect("replay");
    assert_eq!(replay.memory_id, first.memory_id);
    assert!(replay.idempotent_replay);
    host.pool.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "cross-owner fork acceptance keeps consent, visibility, and erase evidence together"
)]
async fn cross_owner_fork_requires_fork_consent_and_goal_write_role() {
    let host = ExampleEngine::new("proxiima_memory_fork_cross_owner", |_| {}).await;
    let alice = UserId::new(Uuid::now_v7());
    let bob = UserId::new(Uuid::now_v7());
    let carol = UserId::new(Uuid::now_v7());
    let alice_owner = OwnerId::from(alice);
    let bob_owner = OwnerId::from(bob);
    let alice_auth = direct(alice);
    let bob_auth = direct(bob);
    let carol_auth = direct(carol);
    assert!(!bob_auth.may_read(&alice_owner, AccessKind::Perspective));
    assert_eq!(bob_auth.role_for_owner(&alice_owner), None);

    let notebook = host.notebook(&alice_owner).await;
    let mut unit = host
        .engine
        .unit_of_work(&alice_auth)
        .await
        .expect("open source unit");
    let fact = unit
        .ingest_fact(FactWrite::new(
            alice_owner,
            "cross-owner-fork",
            &NoteV1 {
                notebook_id: notebook,
                key: "private fact".into(),
                body: "Alice-only evidence".into(),
                written_at: time::OffsetDateTime::now_utc(),
            },
        ))
        .await
        .expect("fact")
        .memory_id;
    let source = unit
        .derive_memory(DerivedMemory::interpretation(
            MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
            alice_owner,
            "published perspective",
            interpretation(fact, "same payload"),
        ))
        .await
        .expect("perspective")
        .memory_id;
    unit.commit().await.expect("commit source");

    let source_row = {
        let mut query = QueryRequest::readable();
        query.entity_kind = Some(EntityKind::Perspective);
        query.memory_ids = vec![source];
        host.engine
            .query(&alice_auth, &query)
            .await
            .expect("read source")
            .memories
            .into_iter()
            .next()
            .expect("source row")
    };

    let read_goal = create_goal(&host, &alice_auth, alice_owner, "read-only").await;
    host.engine
        .grant_goal_access(
            &alice_auth,
            &GoalAudienceGrantInput {
                goal: read_goal,
                subject: bob,
                role: Role::viewer(),
                expires_at: None,
            },
        )
        .await
        .expect("grant source viewer");
    host.engine
        .publish_to_goal(
            &alice_auth,
            &GoalPublicationInput {
                goal: read_goal,
                memory: source,
                permitted_use: PublicationUse::Read,
            },
        )
        .await
        .expect("publish read-only source");
    let mut published_query = QueryRequest::readable();
    published_query.entity_kind = Some(EntityKind::Perspective);
    published_query.memory_ids = vec![source];
    assert_eq!(
        host.engine
            .query(&bob_auth, &published_query)
            .await
            .expect("read published perspective")
            .memories
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        vec![source]
    );

    let owner_request = |key: &str| ForkMemoryRequest {
        source,
        destination: MemoryDestination::Owner(bob_owner),
        request_id: IdempotencyKey::new(key).expect("fork request key"),
    };
    assert!(
        host.engine
            .fork_memory(&bob_auth, &owner_request("read-only-fork"))
            .await
            .is_err()
    );

    host.engine
        .publish_to_goal(
            &alice_auth,
            &GoalPublicationInput {
                goal: read_goal,
                memory: source,
                permitted_use: PublicationUse::Fork,
            },
        )
        .await
        .expect("upgrade source to fork publication");
    let owner_fork = host
        .engine
        .fork_memory(&bob_auth, &owner_request("fork-owner"))
        .await
        .expect("fork into Bob owner");
    assert_eq!(owner_fork.forked_from, source);
    assert_ne!(owner_fork.memory_id, source);
    assert_ne!(owner_fork.handle.into_inner(), source_row.handle);

    let mut fork_query = QueryRequest::readable();
    fork_query.entity_kind = Some(EntityKind::Perspective);
    fork_query.memory_ids = vec![owner_fork.memory_id];
    let fork_row = host
        .engine
        .query(&bob_auth, &fork_query)
        .await
        .expect("read Bob fork")
        .memories
        .into_iter()
        .next()
        .expect("fork row");
    assert_eq!(fork_row.owner, bob_owner);
    assert_eq!(fork_row.payload, source_row.payload);
    assert_eq!(fork_row.refs, source_row.refs);
    assert_eq!(fork_row.origins, source_row.origins);

    let mut fact_query = QueryRequest::readable();
    fact_query.entity_kind = Some(EntityKind::Fact);
    fact_query.memory_ids = vec![fact];
    assert!(
        host.engine
            .query(&bob_auth, &fact_query)
            .await
            .expect("hidden fact query")
            .memories
            .is_empty()
    );

    let unknown_fact = MemoryId::new(Uuid::now_v7());
    let mut invalid_bob_unit = host
        .engine
        .unit_of_work(&bob_auth)
        .await
        .expect("open invalid Bob revision unit");
    assert!(
        invalid_bob_unit
            .derive_memory(DerivedMemory::interpretation(
                MemoryTarget::Revision(owner_fork.memory_id),
                bob_owner,
                "unknown hidden evidence",
                interpretation(unknown_fact, "must fail"),
            ))
            .await
            .is_err()
    );

    let mut bob_unit = host
        .engine
        .unit_of_work(&bob_auth)
        .await
        .expect("open Bob revision unit");
    let revised_fork = bob_unit
        .derive_memory(DerivedMemory::interpretation(
            MemoryTarget::Revision(owner_fork.memory_id),
            bob_owner,
            "Bob revision",
            interpretation(fact, "revised without evidence access"),
        ))
        .await
        .expect("revise fork without reading hidden Fact")
        .memory_id;
    bob_unit.commit().await.expect("commit Bob revision");
    assert_ne!(revised_fork, owner_fork.memory_id);

    let mut fork_history_query = QueryRequest::readable();
    fork_history_query.entity_kind = Some(EntityKind::Perspective);
    fork_history_query.supersession = SupersessionStatus::IncludeSuperseded;
    fork_history_query.memory_ids = vec![owner_fork.memory_id, revised_fork];
    let fork_history = host
        .engine
        .query(&bob_auth, &fork_history_query)
        .await
        .expect("read fork history")
        .memories;
    let revised_row = fork_history
        .iter()
        .find(|row| row.id == revised_fork)
        .expect("revised fork row");
    assert!(revised_row.refs.contains(&fact));
    assert!(
        host.engine
            .query(&bob_auth, &fact_query)
            .await
            .expect("hidden fact remains hidden")
            .memories
            .is_empty()
    );

    let replay = host
        .engine
        .fork_memory(&bob_auth, &owner_request("fork-owner"))
        .await
        .expect("replay original fork after later revision");
    assert!(replay.idempotent_replay);
    assert_eq!(replay.memory_id, owner_fork.memory_id);
    let original_after_revision = fork_history
        .iter()
        .find(|row| row.id == owner_fork.memory_id)
        .expect("original fork row");
    assert_eq!(original_after_revision.payload, source_row.payload);

    let source_two = {
        let mut unit = host
            .engine
            .unit_of_work(&alice_auth)
            .await
            .expect("open second source unit");
        let second = unit
            .derive_memory(DerivedMemory::interpretation(
                MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                alice_owner,
                "second source",
                interpretation(fact, "second source"),
            ))
            .await
            .expect("second perspective")
            .memory_id;
        unit.commit().await.expect("commit second source");
        second
    };
    host.engine
        .publish_to_goal(
            &alice_auth,
            &GoalPublicationInput {
                goal: read_goal,
                memory: source_two,
                permitted_use: PublicationUse::Fork,
            },
        )
        .await
        .expect("publish second fork source");
    assert!(
        host.engine
            .fork_memory(
                &bob_auth,
                &ForkMemoryRequest {
                    source: source_two,
                    destination: MemoryDestination::Owner(bob_owner),
                    request_id: IdempotencyKey::new("fork-owner").expect("reused request key"),
                },
            )
            .await
            .is_err()
    );

    let editor_goal = create_goal(&host, &alice_auth, alice_owner, "editor-destination").await;
    host.engine
        .grant_goal_access(
            &alice_auth,
            &GoalAudienceGrantInput {
                goal: editor_goal,
                subject: bob,
                role: Role::editor(),
                expires_at: None,
            },
        )
        .await
        .expect("grant editor");
    host.engine
        .publish_to_goal(
            &alice_auth,
            &GoalPublicationInput {
                goal: editor_goal,
                memory: source,
                permitted_use: PublicationUse::Fork,
            },
        )
        .await
        .expect("publish goal fork source");
    host.engine
        .grant_goal_access(
            &alice_auth,
            &GoalAudienceGrantInput {
                goal: editor_goal,
                subject: carol,
                role: Role::editor(),
                expires_at: None,
            },
        )
        .await
        .expect("grant second editor");
    let bob_same_key = host
        .engine
        .fork_memory(
            &bob_auth,
            &ForkMemoryRequest {
                source,
                destination: MemoryDestination::WorkingGoal(editor_goal),
                request_id: IdempotencyKey::new("same-goal-collaborator-key").expect("fork key"),
            },
        )
        .await
        .expect("Bob same-key Goal fork");
    let carol_same_key = host
        .engine
        .fork_memory(
            &carol_auth,
            &ForkMemoryRequest {
                source,
                destination: MemoryDestination::WorkingGoal(editor_goal),
                request_id: IdempotencyKey::new("same-goal-collaborator-key").expect("fork key"),
            },
        )
        .await
        .expect("Carol same-key Goal fork");
    assert_ne!(bob_same_key.memory_id, carol_same_key.memory_id);
    let bob_replay = host
        .engine
        .fork_memory(
            &bob_auth,
            &ForkMemoryRequest {
                source,
                destination: MemoryDestination::WorkingGoal(editor_goal),
                request_id: IdempotencyKey::new("same-goal-collaborator-key").expect("fork key"),
            },
        )
        .await
        .expect("Bob same-key replay");
    assert!(bob_replay.idempotent_replay);
    assert_eq!(bob_replay.memory_id, bob_same_key.memory_id);
    let goal_fork = host
        .engine
        .fork_memory(
            &bob_auth,
            &ForkMemoryRequest {
                source,
                destination: MemoryDestination::WorkingGoal(editor_goal),
                request_id: IdempotencyKey::new("fork-editor-goal").expect("fork key"),
            },
        )
        .await
        .expect("fork into editor Goal");
    assert_eq!(goal_fork.forked_from, source);

    let fork_edges = host
        .engine
        .read_edges(
            &alice_auth,
            &EdgeReadRequest {
                filter: EdgeFilter {
                    kind: Some(EdgeKind::Reference),
                    source: Some(EntityRef::Memory(goal_fork.memory_id)),
                    target: None,
                },
                limit: 10,
                cursor: None,
            },
        )
        .await
        .expect("read fork reference edge")
        .edges;
    assert!(fork_edges.iter().any(|edge| {
        edge.kind == EdgeKind::Reference
            && edge.source.entity == EntityRef::Memory(goal_fork.memory_id)
            && edge.target
                == proxiima_core::EdgeTargetProjection::Visible {
                    target: proxiima_core::EdgeEndpoint::memory(EntityKind::Fact, fact),
                }
    }));

    let unknown_goal_fact = MemoryId::new(Uuid::now_v7());
    assert!(
        host.engine
            .derive_memory(
                &bob_auth,
                DerivedMemory::interpretation(
                    MemoryTarget::Revision(goal_fork.memory_id),
                    MemoryDestination::WorkingGoal(editor_goal),
                    "unknown hidden Goal evidence",
                    interpretation(unknown_goal_fact, "must fail"),
                ),
            )
            .await
            .is_err()
    );
    let revised_goal_fork = host
        .engine
        .derive_memory(
            &bob_auth,
            DerivedMemory::interpretation(
                MemoryTarget::Revision(goal_fork.memory_id),
                MemoryDestination::WorkingGoal(editor_goal),
                "Goal revision without evidence access",
                interpretation(fact, "retained hidden evidence"),
            ),
        )
        .await
        .expect("revise Goal fork without reading hidden Fact")
        .memory_id;
    assert_ne!(revised_goal_fork, goal_fork.memory_id);

    let mut goal_history_query = QueryRequest::readable();
    goal_history_query.entity_kind = Some(EntityKind::Perspective);
    goal_history_query.supersession = SupersessionStatus::IncludeSuperseded;
    goal_history_query.memory_ids = vec![goal_fork.memory_id, revised_goal_fork];
    let goal_history = host
        .engine
        .query(&bob_auth, &goal_history_query)
        .await
        .expect("read Goal fork history")
        .memories;
    let revised_goal_row = goal_history
        .iter()
        .find(|row| row.id == revised_goal_fork)
        .expect("revised Goal fork row");
    assert!(revised_goal_row.refs.contains(&fact));
    let revised_edges = host
        .engine
        .read_edges(
            &bob_auth,
            &EdgeReadRequest {
                filter: EdgeFilter {
                    kind: Some(EdgeKind::Reference),
                    source: Some(EntityRef::Memory(revised_goal_fork)),
                    target: None,
                },
                limit: 10,
                cursor: None,
            },
        )
        .await
        .expect("read revised fork reference edges")
        .edges;
    assert!(revised_edges.iter().any(|edge| {
        edge.kind == EdgeKind::Reference
            && edge.source.entity == EntityRef::Memory(revised_goal_fork)
            && edge.target
                == proxiima_core::EdgeTargetProjection::Visible {
                    target: proxiima_core::EdgeEndpoint::memory(EntityKind::Perspective, source),
                }
    }));

    let bound: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM proxiima_core.goal_work_binding w JOIN proxiima_core.memory_head h ON h.handle=w.memory_handle WHERE w.goal_handle=$1 AND h.handle=$2)",
    )
    .bind(editor_goal.into_inner())
    .bind(goal_fork.handle.into_inner())
    .fetch_one(&host.pool)
    .await
    .expect("goal binding");
    assert!(bound);

    let viewer_goal = create_goal(&host, &alice_auth, alice_owner, "viewer-destination").await;
    host.engine
        .grant_goal_access(
            &alice_auth,
            &GoalAudienceGrantInput {
                goal: viewer_goal,
                subject: bob,
                role: Role::viewer(),
                expires_at: None,
            },
        )
        .await
        .expect("grant viewer");
    host.engine
        .publish_to_goal(
            &alice_auth,
            &GoalPublicationInput {
                goal: viewer_goal,
                memory: source,
                permitted_use: PublicationUse::Fork,
            },
        )
        .await
        .expect("publish viewer goal source");
    assert!(
        host.engine
            .fork_memory(
                &bob_auth,
                &ForkMemoryRequest {
                    source,
                    destination: MemoryDestination::WorkingGoal(viewer_goal),
                    request_id: IdempotencyKey::new("fork-viewer-goal").expect("fork key"),
                },
            )
            .await
            .is_err()
    );
    erase_grounding_and_clean_fork(&host, &alice_auth, &bob_auth, fact, revised_fork).await;

    // Exercise the real owner inverse over the complete core+example
    // composition. The source owner disappears; foreign-owned fork rows and
    // their provenance do not.
    let mut registry = SpiiceRegistry::new();
    proxiima_example::ExampleSpiice::register(&mut registry).expect("example registry");
    let registry = registry.freeze_or_panic_for_tests();
    let pg = example_engine::storage_over(host.db.runtime_url(), host.db.platform_url(), &registry)
        .await;
    let surfaces = OwnerSurfaces::for_registry(&registry);
    let erase_auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
        user_id: alice,
        drop_event_id: "cross-owner-fork-alice-erase".into(),
    });
    pg.erase_personal_owner(&erase_auth, alice, &surfaces)
        .await
        .expect("authorized Alice owner erase");
    drop(pg);

    for memory in [source, source_two] {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM proxiima_core.memory WHERE t = $1)")
                .bind(memory.into_inner())
                .fetch_one(&host.pool)
                .await
                .expect("source memory existence");
        assert!(
            !exists,
            "Alice source Perspective survives owner erase: {memory:?}"
        );
    }
    let publications: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxiima_core.goal_memory_publication
          WHERE source_owner = $1",
    )
    .bind(alice_owner.into_inner())
    .fetch_one(&host.pool)
    .await
    .expect("source publication count");
    assert_eq!(publications, 0, "Alice publications survive owner erase");

    let (fork_body, fork_anchor): (String, Uuid) = sqlx::query_as(
        "SELECT i.claim, m.forked_from
           FROM proxiima_core.memory m
           JOIN proxiima_core.interpretation_v1 i ON i.t = m.t
          WHERE m.t = $1 AND m.owner_id = $2",
    )
    .bind(owner_fork.memory_id.into_inner())
    .bind(bob_owner.into_inner())
    .fetch_one(&host.pool)
    .await
    .expect("Bob fork survives Alice erase");
    assert_eq!(fork_body, "same payload");
    assert_eq!(fork_anchor, source.into_inner());

    for memory in [goal_fork.memory_id, revised_goal_fork] {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM proxiima_core.memory WHERE t = $1)")
                .bind(memory.into_inner())
                .fetch_one(&host.pool)
                .await
                .expect("Goal fork existence");
        assert!(
            !exists,
            "Alice-owned Goal work copy survives erase: {memory:?}"
        );
    }
    host.pool.close().await;
}

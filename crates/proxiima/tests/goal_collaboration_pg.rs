//! Real split-role host coverage for exact publications and managed audiences.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "this suite uses part of the shared fixture")]
mod example_engine;

use example_engine::ExampleEngine;
use proxiima_core::test_fixtures::authenticated_context;
use proxiima_core::verbs::goal_write::{
    GoalAuthorship, GoalCreateRequest, GoalTopologyWrite, IdempotencyKey,
};
use proxiima_core::verbs::query::{EntityKind, QueryRequest};
use proxiima_core::{
    AuthPath, AuthzContext, DerivedMemory, FactWrite, GoalAudienceGrantInput, GoalHandle,
    GoalPublicationInput, InterpretationSubjectKind, InterpretationV1, MemoryId, MemoryTarget,
    OwnerId, OwnerRoles, PublicationUse, Relation, Role, SeriesHandle, SimpleTextGoalV1, UserId,
};
use proxiima_example::NoteV1;
use proxiima_storage_pg::PgPlatformScope;
use uuid::Uuid;

fn direct(subject: UserId) -> AuthzContext {
    authenticated_context(AuthzContext::for_subject(subject, AuthPath::HostBearer))
}

fn interpretation(claim: &str, evidence: MemoryId) -> InterpretationV1 {
    InterpretationV1 {
        claim: claim.to_owned(),
        confidence: 90,
        subject_memory_ids: vec![evidence.into_inner()],
        subject_kinds: vec![InterpretationSubjectKind::Fact],
        model_id: "test".into(),
        client_name: "test".into(),
        client_version: "1".into(),
    }
}

async fn groups(h: &ExampleEngine, pairs: &[(proxiima_core::GroupId, UserId)]) {
    let pool = example_engine::fixture_pool(h.db.platform_url(), 1)
        .await
        .expect("platform pool");
    let platform = PgPlatformScope::new(pool, &["proxiima_core", "proxiima_example"])
        .await
        .expect("platform scope");
    let mut tx = platform.begin().await.expect("platform transaction");
    for (group, member) in pairs {
        proxiima::register_group(&mut tx, *group)
            .await
            .expect("register group");
        proxiima::add_group_member(&mut tx, *group, (*member).into(), Relation::Admin)
            .await
            .expect("add group member");
    }
    tx.commit().await.expect("membership commit");
}

async fn group_membership(
    h: &ExampleEngine,
    group: proxiima_core::GroupId,
    members: &[(UserId, Relation)],
) {
    let pool = example_engine::fixture_pool(h.db.platform_url(), 1)
        .await
        .expect("platform pool");
    let platform = PgPlatformScope::new(pool, &["proxiima_core", "proxiima_example"])
        .await
        .expect("platform scope");
    let mut tx = platform.begin().await.expect("platform transaction");
    proxiima::register_group(&mut tx, group)
        .await
        .expect("register group");
    for (member, relation) in members {
        proxiima::add_group_member(&mut tx, group, (*member).into(), *relation)
            .await
            .expect("add group member");
    }
    tx.commit().await.expect("membership commit");
}

async fn make_goal(
    h: &ExampleEngine,
    authz: &AuthzContext,
    owner: OwnerId,
    key: &str,
) -> (GoalHandle, MemoryId) {
    let notebook = h.notebook(&owner).await;
    let mut unit = h.engine.unit_of_work(authz).await.expect("unit");
    let fact = unit
        .ingest_fact(FactWrite::new(
            owner,
            "goal-collaboration",
            &NoteV1 {
                notebook_id: notebook,
                key: key.into(),
                body: "source evidence".into(),
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
            title: key.into(),
            text: "collaboration goal".into(),
            payload: SimpleTextGoalV1 {},
            request_id: IdempotencyKey::new(key).expect("request key"),
            authorship: GoalAuthorship::User,
            author_self_perspective_id: None,
        })
        .await
        .expect("goal")
        .goal_handle;
    unit.commit().await.expect("commit");
    (goal, fact)
}

fn perspective_query() -> QueryRequest {
    let mut query = QueryRequest::readable();
    query.entity_kind = Some(EntityKind::Perspective);
    query
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one acceptance scenario keeps publication, transfer, and revocation evidence together"
)]
async fn publication_keeps_exact_version_and_includes_later_invitees() {
    let h = ExampleEngine::new("proxiima_goal_collaboration", |_| {}).await;
    let author = UserId::new(Uuid::now_v7());
    let reader = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(author);
    let writer = direct(author);
    let outsider = direct(reader);
    assert!(!outsider.may_read(&owner, proxiima_core::AccessKind::Perspective));
    let notebook = h.notebook(&owner).await;
    let mut unit = h
        .engine
        .unit_of_work(&writer)
        .await
        .expect("open transaction");
    let evidence = unit
        .ingest_fact(FactWrite::new(
            owner,
            "collaboration",
            &NoteV1 {
                notebook_id: notebook,
                key: "private grounding".into(),
                body: "Internal evidence stays with its owner".into(),
                written_at: time::OffsetDateTime::now_utc(),
            },
        ))
        .await
        .expect("admit private evidence")
        .memory_id;
    let first = unit
        .derive_memory(DerivedMemory::interpretation(
            MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
            owner,
            "Approved customer interpretation",
            interpretation("Approved", evidence),
        ))
        .await
        .expect("admit approved version")
        .memory_id;
    let goal = unit
        .create_goal(GoalCreateRequest {
            owner,
            topology: GoalTopologyWrite::new(Vec::new(), Vec::new(), Vec::new())
                .expect("unmotivated Goal"),
            title: "Customer A".into(),
            text: "Selected customer material".into(),
            payload: SimpleTextGoalV1 {},
            request_id: IdempotencyKey::new("customer-a").expect("request key"),
            authorship: GoalAuthorship::User,
            author_self_perspective_id: None,
        })
        .await
        .expect("create Goal");
    unit.commit().await.expect("commit initial knowledge");
    let handle: Uuid = sqlx::query_scalar("SELECT handle FROM proxiima_core.goal WHERE t=$1")
        .bind(goal.goal_id.into_inner())
        .fetch_one(&h.pool)
        .await
        .expect("fixture Goal handle");
    let goal = GoalHandle::new(handle);
    h.engine
        .publish_to_goal(
            &writer,
            &GoalPublicationInput {
                goal,
                memory: first,
                permitted_use: PublicationUse::Read,
            },
        )
        .await
        .expect("source owner publishes selected exact version");
    let private = h
        .engine
        .derive_memory(
            &writer,
            DerivedMemory::interpretation(
                MemoryTarget::Revision(first),
                owner,
                "Private pricing revision",
                interpretation("Private price", evidence),
            ),
        )
        .await
        .expect("append unpublished private version")
        .memory_id;
    let mut query = QueryRequest::readable();
    query.entity_kind = Some(EntityKind::Perspective);
    assert!(
        h.engine
            .query(&outsider, &query)
            .await
            .expect("no audience yet")
            .memories
            .is_empty()
    );
    h.engine
        .grant_goal_access(
            &writer,
            &GoalAudienceGrantInput {
                goal,
                subject: reader,
                role: Role::viewer(),
                expires_at: None,
            },
        )
        .await
        .expect("invite a later audience member");
    let page = h
        .engine
        .query(&outsider, &query)
        .await
        .expect("Goal reader query");
    assert_eq!(
        page.memories.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![first]
    );
    assert!(!page.memories.iter().any(|row| row.id == private));
    assert!(!outsider.may_read(&owner, proxiima_core::AccessKind::Perspective));
    query.entity_kind = Some(EntityKind::Fact);
    assert!(
        h.engine
            .query(&outsider, &query)
            .await
            .expect("private grounding query")
            .memories
            .is_empty()
    );
    h.engine
        .revoke_goal_access(&writer, goal, reader)
        .await
        .expect("revoke audience route");
    query.entity_kind = Some(EntityKind::Perspective);
    assert!(
        h.engine
            .query(&outsider, &query)
            .await
            .expect("revoked query")
            .memories
            .is_empty()
    );
    h.pool.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one acceptance scenario keeps stale and fresh publication evidence together"
)]
async fn transfer_invalidates_publication_until_fresh_source_publish() {
    let h = ExampleEngine::new("proxiima_goal_collab_transfer", |_| {}).await;
    let source_group = proxiima_core::GroupId::new(Uuid::now_v7());
    let destination_group = proxiima_core::GroupId::new(Uuid::now_v7());
    let operator = UserId::new(Uuid::now_v7());
    groups(
        &h,
        &[(source_group, operator), (destination_group, operator)],
    )
    .await;
    let source = OwnerId::from(source_group);
    let destination = OwnerId::from(destination_group);
    let authz = authenticated_context(AuthzContext::server_resolved(
        OwnerRoles::for_subject(
            operator,
            [(source, Role::admin()), (destination, Role::admin())],
        )
        .expect("roles"),
        AuthPath::HostBearer,
    ));
    let reader = UserId::new(Uuid::now_v7());
    let reader_auth = direct(reader);
    let (goal, memory) = make_goal(&h, &authz, source, "transfer-publication").await;
    h.engine
        .publish_to_goal(
            &authz,
            &GoalPublicationInput {
                goal,
                memory,
                permitted_use: PublicationUse::Read,
            },
        )
        .await
        .expect("publish");
    h.engine
        .grant_goal_access(
            &authz,
            &GoalAudienceGrantInput {
                goal,
                subject: reader,
                role: Role::viewer(),
                expires_at: None,
            },
        )
        .await
        .expect("grant");
    let query = perspective_query();
    // The fact itself is not a Perspective; publish a derived version so the
    // route proves exact-version visibility rather than merely Goal access.
    let perspective = h
        .engine
        .derive_memory(
            &authz,
            DerivedMemory::interpretation(
                MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                source,
                "published perspective",
                interpretation("published", memory),
            ),
        )
        .await
        .expect("perspective")
        .memory_id;
    h.engine
        .publish_to_goal(
            &authz,
            &GoalPublicationInput {
                goal,
                memory: perspective,
                permitted_use: PublicationUse::Read,
            },
        )
        .await
        .expect("publish perspective");
    assert_eq!(
        h.engine
            .query(&reader_auth, &query)
            .await
            .expect("read")
            .memories
            .len(),
        1
    );
    h.engine
        .transfer_to_owner(
            &authz,
            proxiima_core::EntityId::Memory(perspective),
            destination,
        )
        .await
        .expect("transfer to B");
    assert!(
        h.engine
            .query(&reader_auth, &query)
            .await
            .expect("stale route")
            .memories
            .is_empty()
    );
    h.engine
        .transfer_to_owner(&authz, proxiima_core::EntityId::Memory(perspective), source)
        .await
        .expect("transfer back to A");
    assert!(
        h.engine
            .query(&reader_auth, &query)
            .await
            .expect("old route")
            .memories
            .is_empty()
    );
    h.engine
        .publish_to_goal(
            &authz,
            &GoalPublicationInput {
                goal,
                memory: perspective,
                permitted_use: PublicationUse::Read,
            },
        )
        .await
        .expect("fresh publish");
    assert_eq!(
        h.engine
            .query(&reader_auth, &query)
            .await
            .expect("fresh route")
            .memories
            .len(),
        1
    );
    h.pool.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn independent_goal_routes_revoke_one_without_revoking_the_other() {
    let h = ExampleEngine::new("proxiima_goal_collab_routes", |_| {}).await;
    let owner_user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(owner_user);
    let owner_auth = direct(owner_user);
    let first = UserId::new(Uuid::now_v7());
    let second = UserId::new(Uuid::now_v7());
    let (goal, fact) = make_goal(&h, &owner_auth, owner, "independent-routes").await;
    let perspective = h
        .engine
        .derive_memory(
            &owner_auth,
            DerivedMemory::interpretation(
                MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                owner,
                "route",
                interpretation("route", fact),
            ),
        )
        .await
        .expect("perspective")
        .memory_id;
    h.engine
        .publish_to_goal(
            &owner_auth,
            &GoalPublicationInput {
                goal,
                memory: perspective,
                permitted_use: PublicationUse::Read,
            },
        )
        .await
        .expect("publish");
    for subject in [first, second] {
        h.engine
            .grant_goal_access(
                &owner_auth,
                &GoalAudienceGrantInput {
                    goal,
                    subject,
                    role: Role::viewer(),
                    expires_at: None,
                },
            )
            .await
            .expect("grant");
    }
    let query = perspective_query();
    assert_eq!(
        h.engine
            .query(&direct(first), &query)
            .await
            .expect("first")
            .memories
            .len(),
        1
    );
    assert_eq!(
        h.engine
            .query(&direct(second), &query)
            .await
            .expect("second")
            .memories
            .len(),
        1
    );
    h.engine
        .revoke_goal_access(&owner_auth, goal, first)
        .await
        .expect("revoke first");
    assert!(
        h.engine
            .query(&direct(first), &query)
            .await
            .expect("revoked first")
            .memories
            .is_empty()
    );
    assert_eq!(
        h.engine
            .query(&direct(second), &query)
            .await
            .expect("second remains")
            .memories
            .len(),
        1
    );
    h.engine
        .revoke_goal_access(&owner_auth, goal, second)
        .await
        .expect("revoke second");
    assert!(
        h.engine
            .query(&direct(second), &query)
            .await
            .expect("revoked second")
            .memories
            .is_empty()
    );
    h.pool.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn owning_group_role_reads_foreign_publication_without_guest_grant() {
    let h = ExampleEngine::new("proxiima_goal_collab_group", |_| {}).await;
    let goal_group = proxiima_core::GroupId::new(Uuid::now_v7());
    let source_group = proxiima_core::GroupId::new(Uuid::now_v7());
    let goal_admin = UserId::new(Uuid::now_v7());
    let reader = UserId::new(Uuid::now_v7());
    group_membership(
        &h,
        goal_group,
        &[(goal_admin, Relation::Admin), (reader, Relation::Viewer)],
    )
    .await;
    group_membership(&h, source_group, &[(goal_admin, Relation::Admin)]).await;
    let source = OwnerId::from(source_group);
    let goal_owner = OwnerId::from(goal_group);
    let goal_auth = authenticated_context(AuthzContext::server_resolved(
        OwnerRoles::for_subject(
            goal_admin,
            [(goal_owner, Role::admin()), (source, Role::admin())],
        )
        .expect("goal and source roles"),
        AuthPath::HostBearer,
    ));
    let reader_auth = authenticated_context(AuthzContext::server_resolved(
        OwnerRoles::for_subject(reader, [(goal_owner, Role::viewer())]).expect("reader role"),
        AuthPath::HostBearer,
    ));
    let (goal, _) = make_goal(&h, &goal_auth, goal_owner, "group-foreign").await;
    let (_, source_memory) = make_goal(&h, &goal_auth, source, "foreign-source").await;
    let foreign = h
        .engine
        .derive_memory(
            &goal_auth,
            DerivedMemory::interpretation(
                MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                source,
                "foreign source",
                interpretation("foreign", source_memory),
            ),
        )
        .await
        .expect("foreign perspective")
        .memory_id;
    h.engine
        .publish_to_goal(
            &goal_auth,
            &GoalPublicationInput {
                goal,
                memory: foreign,
                permitted_use: PublicationUse::Read,
            },
        )
        .await
        .expect("foreign publication");
    h.engine
        .grant_goal_access(
            &goal_auth,
            &GoalAudienceGrantInput {
                goal,
                subject: reader,
                role: Role::viewer(),
                expires_at: None,
            },
        )
        .await
        .expect("temporary guest grant");
    let query = perspective_query();
    assert_eq!(
        h.engine
            .query(&reader_auth, &query)
            .await
            .expect("group read")
            .memories
            .len(),
        1
    );
    h.engine
        .revoke_goal_access(&goal_auth, goal, reader)
        .await
        .expect("remove guest grant");
    assert_eq!(
        h.engine
            .query(&reader_auth, &query)
            .await
            .expect("group role read")
            .memories
            .len(),
        1
    );
    h.pool.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_cannot_write_goal_collaboration_tables() {
    let h = ExampleEngine::new("proxiima_goal_collab_rls", |_| {}).await;
    let owner_user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(owner_user);
    let owner_auth = direct(owner_user);
    let reader = UserId::new(Uuid::now_v7());
    let (goal, _) = make_goal(&h, &owner_auth, owner, "runtime-rls").await;
    h.engine
        .grant_goal_access(
            &owner_auth,
            &GoalAudienceGrantInput {
                goal,
                subject: reader,
                role: Role::viewer(),
                expires_at: None,
            },
        )
        .await
        .expect("fixture grant");
    let before: (i16, i16, bool) = sqlx::query_as(
        "SELECT read_ceiling, write_ceiling, manage FROM proxiima_core.goal_access_grant WHERE goal_handle=$1 AND subject_id=$2",
    )
    .bind(goal.into_inner())
    .bind(reader.into_inner())
    .fetch_one(&h.pool)
    .await
    .expect("grant exists");
    let runtime = example_engine::fixture_pool(h.db.runtime_url(), 1)
        .await
        .expect("runtime pool");
    for statement in [
        "INSERT INTO proxiima_core.goal_access_grant (goal_handle, subject_id, read_ceiling, write_ceiling, manage, controller_generation) VALUES (gen_random_uuid(), gen_random_uuid(), 1, 0, false, 0)",
        "INSERT INTO proxiima_core.goal_memory_publication (goal_handle, memory_t, source_owner, source_generation, controller_generation, permitted_use, published_by) VALUES (gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 0, 0, 'read', gen_random_uuid())",
        "INSERT INTO proxiima_core.goal_work_binding (goal_handle, memory_handle, source_owner, source_generation, controller_generation, bound_by) VALUES (gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 0, 0, gen_random_uuid())",
    ] {
        // SQL-POLICY: fixed-fragment (closed literal DML fixture statements)
        assert!(sqlx::query(statement).execute(&runtime).await.is_err());
    }
    for statement in [
        "UPDATE proxiima_core.goal_access_grant SET manage = true",
        "UPDATE proxiima_core.goal_memory_publication SET permitted_use = 'fork'",
        "UPDATE proxiima_core.goal_work_binding SET source_generation = source_generation + 1",
        "DELETE FROM proxiima_core.goal_access_grant",
        "DELETE FROM proxiima_core.goal_memory_publication",
        "DELETE FROM proxiima_core.goal_work_binding",
    ] {
        assert_eq!(
            // SQL-POLICY: fixed-fragment (closed literal DML fixture statements)
            sqlx::query(statement)
                .execute(&runtime)
                .await
                .expect("RLS denial is not a SQL error")
                .rows_affected(),
            0
        );
    }
    runtime.close().await;
    let after: (i16, i16, bool) = sqlx::query_as(
        "SELECT read_ceiling, write_ceiling, manage FROM proxiima_core.goal_access_grant WHERE goal_handle=$1 AND subject_id=$2",
    )
    .bind(goal.into_inner())
    .bind(reader.into_inner())
    .fetch_one(&h.pool)
    .await
    .expect("grant survives runtime attempts");
    assert_eq!(after, before);
    h.pool.close().await;
}

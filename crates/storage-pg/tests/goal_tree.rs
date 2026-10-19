//! #514: a goal is a plain tree. A decompose child names its parent series
//! (`goal.parent_handle`), and a parent is Achieved only when no child head
//! is Active or Paused and at least one is Achieved.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::sync::Arc;

use proxima_core::storage_ports::{FactIngestPort, GoalWritePort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    GoalAssignmentTarget, GoalAtomicContext, GoalAuthorship, GoalDependencyRef, GoalDraft,
    GoalEvidenceRef, GoalPayloadWrite, GoalState, GoalTopologyWrite, GoalTransitionKind,
    GoalWriteOutcome, IdempotencyKey, ModifyGoalAtomicRequest, TransitionGoalAtomicRequest,
};
use proxima_core::{
    AccessKind, EdgeEndpoint, EntityKind, FlavorRegistry, FlavorRegistryFrozen, GoalId, MemoryId,
    OwnerId, PendingWork, SchemaId, SchemaVersion, StorageError, UserId,
};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::verbs::forget::MemoryColdStore;
use uuid::Uuid;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

fn fact_draft(kind: &str) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new(format!("tree/{kind}")),
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

fn key(raw: &str) -> IdempotencyKey {
    IdempotencyKey::new(raw).expect("test idempotency key")
}

struct Tree {
    db_name: String,
    pg: PgStorage,
    owner: OwnerId,
    permit: OwnerWritePermit,
    registry: FlavorRegistryFrozen,
    fact: MemoryId,
    perspective: MemoryId,
}

impl Tree {
    async fn new() -> TestResult<Self> {
        let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
        if let Err(e) = create_core_db(&db_name).await {
            panic!("PG required for tests but admin connect failed: {e}");
        }
        let pg = PgStorage::connect_for_tests(db_url(&db_name))
            .await?
            .with_cold(Arc::new(MemoryColdStore));
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let fact = pg
            .ingest_fact_atomic(&permit, &fact_draft("fact"), None)
            .await?;
        let mut abs = fact_draft("abstraction");
        abs.additional_references = vec![EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)];
        let abs = pg.ingest_fact_atomic(&permit, &abs, None).await?;
        let mut perspective = fact_draft("perspective");
        perspective.additional_references =
            vec![EdgeEndpoint::memory(EntityKind::Abstraction, abs.memory_id)];
        let perspective = pg.ingest_fact_atomic(&permit, &perspective, None).await?;
        Ok(Self {
            db_name,
            pg,
            owner,
            permit,
            registry: FlavorRegistry::new().freeze_or_panic_for_tests(),
            fact: fact.memory_id,
            perspective: perspective.memory_id,
        })
    }

    fn context(&self) -> GoalAtomicContext<'_> {
        GoalAtomicContext {
            registry: &self.registry,
            author_self_perspective_id: None,
            publication: None,
        }
    }

    fn topology(&self, dependencies: &[GoalId], evidence: bool) -> GoalTopologyWrite {
        GoalTopologyWrite::new(
            GoalAssignmentTarget::perspective(self.perspective),
            dependencies
                .iter()
                .copied()
                .map(GoalDependencyRef::new)
                .collect(),
            if evidence {
                vec![GoalEvidenceRef::new(self.fact)]
            } else {
                Vec::new()
            },
        )
        .expect("test topology")
    }

    async fn create(&self, request: &str, dependencies: &[GoalId]) -> TestResult<GoalId> {
        let draft = GoalDraft {
            owner: self.owner,
            schema_id: SchemaId::new("core/simple-text-v1".into()),
            schema_version: SchemaVersion::new(1),
            title: request.into(),
            text: request.into(),
            payload: Vec::new(),
            sidecar_payload: None,
            state: GoalState::Active,
            topology: self.topology(dependencies, true),
            supersedes_goal_id: None,
            authorship: GoalAuthorship::User,
            request_id: request.into(),
        };
        let outcome = self
            .pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft,
                    context: self.context(),
                    write_act_t: None,
                },
                &self.permit,
            )
            .await?;
        Ok(outcome.goal_id)
    }

    async fn decompose(&self, parent: GoalId, children: &[&str]) -> TestResult<Vec<GoalId>> {
        let outcome = self
            .pg
            .decompose_goal_atomic(
                &DecomposeGoalAtomicRequest {
                    owner: self.owner,
                    parent_goal_id: parent,
                    authorship: GoalAuthorship::User,
                    context: self.context(),
                    topology: self.topology(&[], false),
                    children: children
                        .iter()
                        .map(|request| ChildGoalDraft {
                            payload: payload(request),
                            evidence: vec![GoalEvidenceRef::new(self.fact)],
                            request_id: key(request),
                        })
                        .collect(),
                },
                &self.permit,
            )
            .await?;
        Ok(outcome.children.iter().map(|c| c.outcome.goal_id).collect())
    }

    async fn transition(
        &self,
        prior: GoalId,
        transition: GoalTransitionKind,
        request: &str,
    ) -> TestResult<GoalId> {
        let outcome = self
            .pg
            .transition_goal_atomic(
                &TransitionGoalAtomicRequest {
                    owner: self.owner,
                    prior_goal_id: prior,
                    transition,
                    authorship: GoalAuthorship::User,
                    request_id: key(request),
                    context: self.context(),
                },
                &self.permit,
            )
            .await?;
        Ok(outcome.goal_id)
    }

    async fn pause(&self, prior: GoalId, request: &str) -> TestResult<GoalId> {
        self.transition(
            prior,
            GoalTransitionKind::Pause {
                pending: PendingWork::Keep,
            },
            request,
        )
        .await
    }

    async fn modify(&self, prior: GoalId, request: &str) -> TestResult<GoalId> {
        let outcome = self
            .pg
            .modify_goal_atomic(
                &ModifyGoalAtomicRequest {
                    owner: self.owner,
                    prior_goal_id: prior,
                    replacement: payload(request),
                    authorship: GoalAuthorship::User,
                    request_id: key(request),
                    context: self.context(),
                    evidence: None,
                },
                &self.permit,
            )
            .await?;
        Ok(outcome.goal_id)
    }

    async fn achieve(
        &self,
        prior: GoalId,
        request: &str,
    ) -> Result<GoalWriteOutcome, StorageError> {
        self.pg
            .achieve_goal_atomic(
                &AchieveGoalAtomicRequest {
                    owner: self.owner,
                    prior_goal_id: prior,
                    authorship: GoalAuthorship::User,
                    request_id: key(request),
                    context: self.context(),
                    evidence: vec![GoalEvidenceRef::new(self.fact)],
                },
                &self.permit,
            )
            .await
    }

    async fn handle_of(&self, goal: GoalId) -> TestResult<Uuid> {
        Ok(
            sqlx::query_scalar("SELECT handle FROM proxima_core.goal WHERE t = $1")
                .bind(goal.into_inner())
                .fetch_one(self.pg.pool_for_tests())
                .await?,
        )
    }

    async fn head_of(&self, goal: GoalId) -> TestResult<Uuid> {
        Ok(sqlx::query_scalar(
            "SELECT h.t FROM proxima_core.goal_head h
               JOIN proxima_core.goal g ON g.handle = h.handle
              WHERE g.t = $1",
        )
        .bind(goal.into_inner())
        .fetch_one(self.pg.pool_for_tests())
        .await?)
    }

    async fn counts(&self) -> TestResult<Vec<i64>> {
        let row: (i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM proxima_core.goal),
                    (SELECT count(*) FROM proxima_core.goal_change_v1),
                    (SELECT count(*) FROM proxima_core.goal_replay_declaration),
                    (SELECT count(*) FROM proxima_core.memory),
                    (SELECT count(*) FROM proxima_core.publication_outbox)",
        )
        .fetch_one(self.pg.pool_for_tests())
        .await?;
        Ok(vec![row.0, row.1, row.2, row.3, row.4])
    }

    async fn finish(self, result: TestResult<()>, what: &str) {
        let _ = drop_db(&self.db_name).await;
        result.unwrap_or_else(|e| panic!("{what}: {e}"));
    }
}

fn open_children(result: Result<GoalWriteOutcome, StorageError>, what: &str) -> Vec<GoalId> {
    match result {
        Err(StorageError::GoalChildrenOpen { open }) => open,
        other => panic!("{what}: expected GoalChildrenOpen, got {other:?}"),
    }
}

/// F1: decompose P into A, B; achieving P names both open children.
#[tokio::test]
async fn achieving_a_parent_with_active_children_names_them() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p = tree.create("f1-p", &[]).await?;
        let children = tree.decompose(p, &["f1-a", "f1-b"]).await?;
        let open = open_children(tree.achieve(p, "f1-achieve").await, "F1");
        assert_eq!(open, children);
        Ok(())
    }
    .await;
    tree.finish(result, "F1").await;
}

/// F2: a Paused child is open and is named by its current head t.
#[tokio::test]
async fn a_paused_child_is_open_and_named_by_its_head() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p = tree.create("f2-p", &[]).await?;
        let children = tree.decompose(p, &["f2-a", "f2-b"]).await?;
        let paused = tree.pause(children[0], "f2-a-pause").await?;
        tree.achieve(children[1], "f2-b-achieve").await?;
        let open = open_children(tree.achieve(p, "f2-achieve").await, "F2");
        assert_eq!(open, vec![paused]);
        assert_ne!(paused, children[0], "the head t, not the first t");
        Ok(())
    }
    .await;
    tree.finish(result, "F2").await;
}

/// F3: all children Abandoned refuses.
#[tokio::test]
async fn all_abandoned_children_refuse_the_parent() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p = tree.create("f3-p", &[]).await?;
        let children = tree.decompose(p, &["f3-a", "f3-b"]).await?;
        tree.transition(children[0], GoalTransitionKind::Abandon, "f3-a-abandon")
            .await?;
        tree.transition(children[1], GoalTransitionKind::Abandon, "f3-b-abandon")
            .await?;
        match tree.achieve(p, "f3-achieve").await {
            Err(StorageError::GoalChildrenAllAbandoned) => {}
            other => panic!("F3: expected GoalChildrenAllAbandoned, got {other:?}"),
        }
        Ok(())
    }
    .await;
    tree.finish(result, "F3").await;
}

/// F4: children of an older parent version count after modify, pause and
/// resume.
#[tokio::test]
async fn children_of_an_older_parent_version_count() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p1 = tree.create("f4-p", &[]).await?;
        let children = tree.decompose(p1, &["f4-a"]).await?;
        let p2 = tree.modify(p1, "f4-modify").await?;
        let p3 = tree.pause(p2, "f4-pause").await?;
        let p4 = tree
            .transition(p3, GoalTransitionKind::Resume, "f4-resume")
            .await?;
        let open = open_children(tree.achieve(p4, "f4-achieve").await, "F4");
        assert_eq!(open, children);
        Ok(())
    }
    .await;
    tree.finish(result, "F4").await;
}

/// F5: a refusal writes nothing.
#[tokio::test]
async fn a_refused_achieve_writes_nothing() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p = tree.create("f5-p", &[]).await?;
        tree.decompose(p, &["f5-a"]).await?;
        let before = tree.counts().await?;
        let head = tree.head_of(p).await?;
        open_children(tree.achieve(p, "f5-achieve").await, "F5");
        assert_eq!(
            tree.counts().await?,
            before,
            "goal, change, declaration, memory, outbox"
        );
        assert_eq!(tree.head_of(p).await?, head, "parent head unchanged");
        Ok(())
    }
    .await;
    tree.finish(result, "F5").await;
}

/// F7: the edge is fixed for the life of a series, and a goal is not its
/// own parent.
#[tokio::test]
async fn the_parent_edge_is_fixed_and_never_self() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p = tree.create("f7-p", &[]).await?;
        let children = tree.decompose(p, &["f7-a"]).await?;
        for (parent, label) in [(None, "NULL"), (Some(Uuid::now_v7()), "another handle")] {
            let err = sqlx::query(
                "INSERT INTO proxima_core.goal
                    (handle, t, owner_id, title, state, request_id, close_fact_t,
                     assignment_t, dependency_t, evidence_t, write_act_t, text, \"by\", via,
                     parent_handle)
                 SELECT handle, $2, owner_id, title, state, $3, close_fact_t,
                        assignment_t, dependency_t, evidence_t, write_act_t, text, \"by\", via,
                        $4
                   FROM proxima_core.goal WHERE t = $1",
            )
            .bind(children[0].into_inner())
            .bind(Uuid::now_v7())
            .bind(format!("f7-raw-{label}"))
            .bind(parent)
            .execute(tree.pg.pool_for_tests())
            .await
            .expect_err(label);
            let db = err.as_database_error().ok_or("database error")?;
            assert_eq!(db.code().as_deref(), Some("23514"), "{label}: {db}");
            assert!(
                db.message().contains("goal series parent is fixed"),
                "{label}: {db}"
            );
        }
        // A fresh series naming itself as parent.
        let fresh = Uuid::now_v7();
        let err = sqlx::query(
            "INSERT INTO proxima_core.goal
                (handle, t, owner_id, title, state, request_id, close_fact_t,
                 assignment_t, dependency_t, evidence_t, write_act_t, text, \"by\", via,
                 parent_handle)
             SELECT $2, $2, owner_id, title, state, 'f7-raw-self', close_fact_t,
                    assignment_t, '{}', evidence_t, write_act_t, text, \"by\", via, $2
               FROM proxima_core.goal WHERE t = $1",
        )
        .bind(p.into_inner())
        .bind(fresh)
        .execute(tree.pg.pool_for_tests())
        .await
        .expect_err("self parent");
        let db = err.as_database_error().ok_or("database error")?;
        assert_eq!(db.constraint(), Some("goal_parent_not_self_chk"), "{db}");
        Ok(())
    }
    .await;
    tree.finish(result, "F7").await;
}

/// F8: decompose sets parent_handle to the parent's series handle on every
/// child, and each later child version carries it.
#[tokio::test]
async fn decompose_sets_the_parent_edge_and_versions_carry_it() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p = tree.create("f8-p", &[]).await?;
        let parent_handle = tree.handle_of(p).await?;
        let children = tree.decompose(p, &["f8-a", "f8-b"]).await?;
        let paused = tree.pause(children[0], "f8-a-pause").await?;
        let resumed = tree
            .transition(paused, GoalTransitionKind::Resume, "f8-a-resume")
            .await?;
        let abandoned = tree
            .transition(resumed, GoalTransitionKind::Abandon, "f8-a-abandon")
            .await?;
        let p_edge: Option<Uuid> =
            sqlx::query_scalar("SELECT parent_handle FROM proxima_core.goal WHERE t = $1")
                .bind(p.into_inner())
                .fetch_one(tree.pg.pool_for_tests())
                .await?;
        assert_eq!(p_edge, None, "a root has no parent");
        for (label, goal) in [
            ("a", children[0]),
            ("b", children[1]),
            ("a paused", paused),
            ("a resumed", resumed),
            ("a abandoned", abandoned),
        ] {
            let edge: Option<Uuid> =
                sqlx::query_scalar("SELECT parent_handle FROM proxima_core.goal WHERE t = $1")
                    .bind(goal.into_inner())
                    .fetch_one(tree.pg.pool_for_tests())
                    .await?;
            assert_eq!(edge, Some(parent_handle), "{label}");
        }
        Ok(())
    }
    .await;
    tree.finish(result, "F8").await;
}

/// G1-G3: no children, all Achieved, Achieved plus Abandoned: Ok.
#[tokio::test]
async fn closed_children_or_none_admit_the_parent() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let lone = tree.create("g1-p", &[]).await?;
        tree.achieve(lone, "g1-achieve").await?;

        let p = tree.create("g2-p", &[]).await?;
        let children = tree.decompose(p, &["g2-a", "g2-b"]).await?;
        tree.achieve(children[0], "g2-a-achieve").await?;
        tree.achieve(children[1], "g2-b-achieve").await?;
        tree.achieve(p, "g2-achieve").await?;

        let p = tree.create("g3-p", &[]).await?;
        let children = tree.decompose(p, &["g3-a", "g3-b"]).await?;
        tree.achieve(children[0], "g3-a-achieve").await?;
        tree.transition(children[1], GoalTransitionKind::Abandon, "g3-b-abandon")
            .await?;
        tree.achieve(p, "g3-achieve").await?;
        Ok(())
    }
    .await;
    tree.finish(result, "G1-G3").await;
}

/// G4: a plain topology dependency is not a child.
#[tokio::test]
async fn a_plain_dependency_is_not_a_child() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let x = tree.create("g4-x", &[]).await?;
        let y = tree.create("g4-y", &[x]).await?;
        let y_dependencies: Vec<Uuid> =
            sqlx::query_scalar("SELECT dependency_t FROM proxima_core.goal WHERE t = $1")
                .bind(y.into_inner())
                .fetch_one(tree.pg.pool_for_tests())
                .await?;
        assert_eq!(y_dependencies, vec![x.into_inner()], "Y depends on X");
        tree.achieve(x, "g4-achieve").await?;
        Ok(())
    }
    .await;
    tree.finish(result, "G4").await;
}

/// G5: a successful achieve of a parent replays, writing nothing new.
#[tokio::test]
async fn an_achieved_parent_replays() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p = tree.create("g5-p", &[]).await?;
        let children = tree.decompose(p, &["g5-a"]).await?;
        tree.achieve(children[0], "g5-a-achieve").await?;
        let first = tree.achieve(p, "g5-achieve").await?;
        let before = tree.counts().await?;
        let replay = tree.achieve(p, "g5-achieve").await?;
        assert!(replay.idempotent_replay, "{replay:?}");
        assert_eq!(replay.goal_id, first.goal_id);
        assert_eq!(tree.counts().await?, before, "a replay writes nothing");
        Ok(())
    }
    .await;
    tree.finish(result, "G5").await;
}

/// Sessions of this test database waiting on an advisory lock. A real lock
/// signal: the race tests wait on it instead of sleeping.
async fn lock_waiters(pool: &sqlx::PgPool) -> TestResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT count(DISTINCT pid)
           FROM pg_locks
          WHERE locktype = 'advisory' AND NOT granted
            AND database = (SELECT oid FROM pg_database WHERE datname = current_database())",
    )
    .fetch_one(pool)
    .await?)
}

/// Yield until at least `n` sessions wait on an advisory lock, under one deadline.
async fn await_lock_waiters(pool: &sqlx::PgPool, n: i64, what: &str) -> TestResult<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while lock_waiters(pool).await? < n {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: {n} lock waiter(s) never appeared"
        );
        tokio::task::yield_now().await;
    }
    Ok(())
}

/// Hold the lifecycle lock on `t`, the same advisory key every goal write
/// takes on its targets.
async fn hold_lifecycle_lock(
    pool: &sqlx::PgPool,
    t: Uuid,
) -> TestResult<sqlx::Transaction<'static, sqlx::Postgres>> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('proxima-forget:' || $1::text, 0))")
        .bind(t)
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// No Achieved parent head has an Active or Paused child head.
async fn assert_tree_invariant(pool: &sqlx::PgPool, what: &str) -> TestResult<()> {
    let broken: i64 = sqlx::query_scalar(
        "SELECT count(*)
           FROM proxima_core.goal p
           JOIN proxima_core.goal_head ph ON ph.handle = p.handle AND ph.t = p.t
           JOIN proxima_core.goal c ON c.parent_handle = p.handle
           JOIN proxima_core.goal_head ch ON ch.handle = c.handle AND ch.t = c.t
          WHERE p.state = 'Achieved' AND c.state IN ('Active', 'Paused')",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(broken, 0, "{what}: an Achieved parent has an open child");
    Ok(())
}

/// F6: decompose and achieve race on one parent; decompose takes the parent
/// lock first. Achieve has already read the parent when it queues, so only a
/// check after its lock sees the child: it is refused, naming the child.
#[tokio::test]
async fn a_racing_decompose_first_refuses_the_achieve() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p = tree.create("f6-p", &[]).await?;
        let pool = tree.pg.pool_for_tests();
        let blocker = hold_lifecycle_lock(pool, p.into_inner()).await?;
        let (decomposed, achieved, released) = tokio::join!(
            tree.decompose(p, &["f6-a"]),
            async {
                await_lock_waiters(pool, 1, "F6 decompose queued").await?;
                Ok::<_, Box<dyn std::error::Error>>(tree.achieve(p, "f6-achieve").await)
            },
            async {
                await_lock_waiters(pool, 2, "F6 achieve queued").await?;
                blocker.commit().await?;
                Ok::<_, Box<dyn std::error::Error>>(())
            },
        );
        released?;
        let children = decomposed?;
        let open = open_children(achieved?, "F6");
        assert_eq!(open, children);
        assert_eq!(
            tree.head_of(p).await?,
            p.into_inner(),
            "parent not achieved"
        );
        assert_tree_invariant(pool, "F6").await?;
        Ok(())
    }
    .await;
    tree.finish(result, "F6").await;
}

/// G6: achieve takes the parent lock first. The queued decompose then finds
/// the parent head moved and terminal, and adds no child.
#[tokio::test]
async fn a_racing_achieve_first_refuses_the_decompose() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p = tree.create("g6-p", &[]).await?;
        let pool = tree.pg.pool_for_tests();
        let blocker = hold_lifecycle_lock(pool, p.into_inner()).await?;
        let (achieved, decomposed, released) = tokio::join!(
            tree.achieve(p, "g6-achieve"),
            async {
                await_lock_waiters(pool, 1, "G6 achieve queued").await?;
                Ok::<_, Box<dyn std::error::Error>>(tree.decompose(p, &["g6-a"]).await)
            },
            async {
                await_lock_waiters(pool, 2, "G6 decompose queued").await?;
                blocker.commit().await?;
                Ok::<_, Box<dyn std::error::Error>>(())
            },
        );
        released?;
        achieved?;
        let err = decomposed?.expect_err("decompose after achieve is refused");
        let err = err
            .downcast::<StorageError>()
            .map_err(|e| format!("G6: not a StorageError: {e}"))?;
        assert!(
            matches!(
                *err,
                StorageError::Conflict(_) | StorageError::ConstraintViolation(_)
            ),
            "G6: {err:?}"
        );
        let children: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.goal WHERE parent_handle IS NOT NULL",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(children, 0, "no child row");
        assert_tree_invariant(pool, "G6").await?;
        Ok(())
    }
    .await;
    tree.finish(result, "G6").await;
}

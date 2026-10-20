//! #514: a goal is a plain tree. A decompose child names its parent series
//! (`goal.parent_handle`), and a parent is Achieved only when no child head
//! is Active or Paused and at least one is Achieved.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::sync::Arc;

use proxima_core::publication::{
    PublicationExtensions, PublicationLimits, PublicationSource, PublicationStamp,
};
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
type History = (String, String, i32, Option<Uuid>, Option<String>);

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
        request: None,
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
    /// Set by `new_listenable`: goal writes carry a publication stamp, so
    /// each one also records a `core/goal-change-v1` change.
    feed: Option<(PublicationSource, PublicationExtensions)>,
    fact: MemoryId,
    perspective: MemoryId,
}

impl Tree {
    /// A tree whose goal writes publish `core/goal-change-v1` records.
    async fn new_listenable() -> TestResult<Self> {
        let mut tree = Self::new().await?;
        let mut listenable = FlavorRegistry::new();
        listenable.publish_goal_changes();
        tree.registry = listenable.freeze_or_panic_for_tests();
        tree.feed = Some((
            PublicationSource::new("urn:proxima:goal-tree-test")?,
            PublicationExtensions::default(),
        ));
        Ok(tree)
    }

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
            feed: None,
            fact: fact.memory_id,
            perspective: perspective.memory_id,
        })
    }

    fn context(&self) -> GoalAtomicContext<'_> {
        GoalAtomicContext {
            registry: &self.registry,
            author_self_perspective_id: None,
            publication: self.feed.as_ref().map(|(source, extensions)| {
                PublicationStamp::new(source, None, extensions, PublicationLimits::default())
            }),
        }
    }

    fn topology(&self, dependencies: &[GoalId], evidence: bool) -> GoalTopologyWrite {
        GoalTopologyWrite::new(
            Some(GoalAssignmentTarget::perspective(self.perspective)),
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

    async fn abandon(
        &self,
        prior: GoalId,
        request: &str,
    ) -> Result<GoalWriteOutcome, StorageError> {
        self.pg
            .transition_goal_atomic(
                &TransitionGoalAtomicRequest {
                    owner: self.owner,
                    prior_goal_id: prior,
                    transition: GoalTransitionKind::Abandon,
                    authorship: GoalAuthorship::User,
                    request_id: key(request),
                    context: self.context(),
                },
                &self.permit,
            )
            .await
    }

    async fn resume_raw(
        &self,
        prior: GoalId,
        request: &str,
    ) -> Result<GoalWriteOutcome, StorageError> {
        self.pg
            .transition_goal_atomic(
                &TransitionGoalAtomicRequest {
                    owner: self.owner,
                    prior_goal_id: prior,
                    transition: GoalTransitionKind::Resume,
                    authorship: GoalAuthorship::User,
                    request_id: key(request),
                    context: self.context(),
                },
                &self.permit,
            )
            .await
    }

    /// The series' goal-change records: kind, state, revision, prior, pending.
    async fn history(&self, goal: GoalId) -> TestResult<Vec<History>> {
        Ok(sqlx::query_as(
            "SELECT c.change_kind::text, c.state::text, c.revision, c.prior_t, c.pending::text
               FROM proxima_core.goal_change_v1 c
               JOIN proxima_core.goal g ON g.t = c.goal_t
              WHERE g.handle = (SELECT handle FROM proxima_core.goal WHERE t = $1)
              ORDER BY c.revision",
        )
        .bind(goal.into_inner())
        .fetch_all(self.pg.pool_for_tests())
        .await?)
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

/// No closed (Achieved or Abandoned) parent head has an Active or Paused
/// child head: by induction, the depth-independent form of the rule.
async fn assert_tree_invariant(pool: &sqlx::PgPool, what: &str) -> TestResult<()> {
    let broken: i64 = sqlx::query_scalar(
        "SELECT count(*)
           FROM proxima_core.goal p
           JOIN proxima_core.goal_head ph ON ph.handle = p.handle AND ph.t = p.t
           JOIN proxima_core.goal c ON c.parent_handle = p.handle
           JOIN proxima_core.goal_head ch ON ch.handle = c.handle AND ch.t = c.t
          WHERE p.state IN ('Achieved', 'Abandoned') AND c.state IN ('Active', 'Paused')",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(broken, 0, "{what}: a closed parent has an open child");
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

// ============================================================
// An open goal never sits under a closed one: abandon (refs #514)
// ============================================================

/// The open children a refusal names, as an error rather than a panic so a
/// red run still drops its database.
fn refused_open(
    result: Result<GoalWriteOutcome, StorageError>,
    what: &str,
) -> TestResult<Vec<GoalId>> {
    match result {
        Err(StorageError::GoalChildrenOpen { open }) => Ok(open),
        other => Err(format!("{what}: expected GoalChildrenOpen, got {other:?}").into()),
    }
}

/// Closed heads with an open descendant at any depth (test-only recursive
/// walk; product code checks one level, post-lock).
async fn open_below_closed(pool: &sqlx::PgPool) -> TestResult<i64> {
    Ok(sqlx::query_scalar(
        "WITH RECURSIVE heads AS (
             SELECT g.handle, g.state, g.parent_handle
               FROM proxima_core.goal g
               JOIN proxima_core.goal_head h ON h.handle = g.handle AND h.t = g.t
         ), below(ancestor, descendant) AS (
             SELECT parent_handle, handle FROM heads WHERE parent_handle IS NOT NULL
             UNION
             SELECT b.ancestor, c.handle FROM below b JOIN heads c ON c.parent_handle = b.descendant
         )
         SELECT count(*)
           FROM below b
           JOIN heads a ON a.handle = b.ancestor
           JOIN heads d ON d.handle = b.descendant
          WHERE a.state IN ('Achieved', 'Abandoned') AND d.state IN ('Active', 'Paused')",
    )
    .fetch_one(pool)
    .await?)
}

/// T1: decompose P into A, B; abandoning P names both open children.
#[tokio::test]
async fn abandoning_a_parent_with_active_children_names_them() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p = tree.create("t1-p", &[]).await?;
        let children = tree.decompose(p, &["t1-a", "t1-b"]).await?;
        let open = refused_open(tree.abandon(p, "t1-abandon").await, "T1")?;
        assert_eq!(open, children);
        Ok(())
    }
    .await;
    tree.finish(result, "T1").await;
}

/// T2: a Paused child blocks abandon and is named by its head t.
#[tokio::test]
async fn a_paused_child_blocks_abandon_by_its_head() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p = tree.create("t2-p", &[]).await?;
        let children = tree.decompose(p, &["t2-a"]).await?;
        let paused = tree.pause(children[0], "t2-a-pause").await?;
        let open = refused_open(tree.abandon(p, "t2-abandon").await, "T2")?;
        assert_eq!(open, vec![paused]);
        Ok(())
    }
    .await;
    tree.finish(result, "T2").await;
}

/// T3, hole (a): an Abandoned child cannot keep an open grandchild, and the
/// parent closes only bottom-up.
#[tokio::test]
async fn an_abandoned_child_keeps_no_open_grandchild() {
    let tree = Tree::new().await.expect("fixture");
    let pool = tree.pg.pool_for_tests();
    let result = async {
        let p = tree.create("t3-p", &[]).await?;
        let a = tree.decompose(p, &["t3-a"]).await?[0];
        let a1 = tree.decompose(a, &["t3-a1"]).await?[0];
        let open = refused_open(tree.abandon(a, "t3-a-abandon-early").await, "T3")?;
        assert_eq!(open, vec![a1]);
        assert_eq!(tree.head_of(a).await?, a.into_inner(), "A unchanged");
        assert_tree_invariant(pool, "T3 refused").await?;
        tree.achieve(a1, "t3-a1-achieve").await?;
        tree.abandon(a, "t3-a-abandon").await?;
        assert_tree_invariant(pool, "T3 A abandoned").await?;
        match tree.achieve(p, "t3-p-achieve-early").await {
            Err(StorageError::GoalChildrenAllAbandoned) => {}
            other => {
                return Err(format!("T3: expected GoalChildrenAllAbandoned, got {other:?}").into());
            }
        }
        let b = tree.decompose(p, &["t3-b"]).await?[0];
        tree.achieve(b, "t3-b-achieve").await?;
        tree.achieve(p, "t3-p-achieve").await?;
        assert_tree_invariant(pool, "T3 done").await?;
        assert_eq!(open_below_closed(pool).await?, 0, "T3 any depth");
        Ok(())
    }
    .await;
    tree.finish(result, "T3").await;
}

/// T4: children of an older parent version block abandon.
#[tokio::test]
async fn children_of_an_older_parent_version_block_abandon() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p1 = tree.create("t4-p", &[]).await?;
        let children = tree.decompose(p1, &["t4-a"]).await?;
        let p2 = tree.modify(p1, "t4-modify").await?;
        let p3 = tree.pause(p2, "t4-pause").await?;
        let p4 = tree
            .transition(p3, GoalTransitionKind::Resume, "t4-resume")
            .await?;
        let open = refused_open(tree.abandon(p4, "t4-abandon").await, "T4")?;
        assert_eq!(open, children);
        Ok(())
    }
    .await;
    tree.finish(result, "T4").await;
}

/// T5: a refused abandon writes nothing.
#[tokio::test]
async fn a_refused_abandon_writes_nothing() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p = tree.create("t5-p", &[]).await?;
        tree.decompose(p, &["t5-a"]).await?;
        let before = tree.counts().await?;
        let head = tree.head_of(p).await?;
        refused_open(tree.abandon(p, "t5-abandon").await, "T5")?;
        assert_eq!(
            tree.counts().await?,
            before,
            "goal, change, declaration, memory, outbox"
        );
        assert_eq!(tree.head_of(p).await?, head, "parent head unchanged");
        Ok(())
    }
    .await;
    tree.finish(result, "T5").await;
}

/// T6: decompose and abandon race on one parent; decompose takes the parent
/// lock first. Only a check after abandon's lock sees the child.
#[tokio::test]
async fn a_racing_decompose_first_refuses_the_abandon() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p = tree.create("t6-p", &[]).await?;
        let pool = tree.pg.pool_for_tests();
        let blocker = hold_lifecycle_lock(pool, p.into_inner()).await?;
        let (decomposed, abandoned, released) = tokio::join!(
            tree.decompose(p, &["t6-a"]),
            async {
                await_lock_waiters(pool, 1, "T6 decompose queued").await?;
                Ok::<_, Box<dyn std::error::Error>>(tree.abandon(p, "t6-abandon").await)
            },
            async {
                await_lock_waiters(pool, 2, "T6 abandon queued").await?;
                blocker.commit().await?;
                Ok::<_, Box<dyn std::error::Error>>(())
            },
        );
        released?;
        let children = decomposed?;
        let open = refused_open(abandoned?, "T6")?;
        assert_eq!(open, children);
        assert_eq!(
            tree.head_of(p).await?,
            p.into_inner(),
            "parent not abandoned"
        );
        assert_tree_invariant(pool, "T6").await?;
        Ok(())
    }
    .await;
    tree.finish(result, "T6").await;
}

/// T8: depth 3 by the one-level rule alone: every close above an open level
/// is refused, and the tree closes bottom-up.
#[tokio::test]
async fn a_three_level_tree_closes_only_bottom_up() {
    let tree = Tree::new().await.expect("fixture");
    let pool = tree.pg.pool_for_tests();
    let result = async {
        let p = tree.create("t8-p", &[]).await?;
        let a = tree.decompose(p, &["t8-a"]).await?[0];
        let a1 = tree.decompose(a, &["t8-a1"]).await?[0];
        let a1x = tree.decompose(a1, &["t8-a1x"]).await?[0];
        let levels = [(p, a, "p"), (a, a1, "a"), (a1, a1x, "a1")];
        for (goal, child, label) in levels {
            let open = refused_open(
                tree.abandon(goal, &format!("t8-{label}-abandon-0")).await,
                label,
            )?;
            assert_eq!(open, vec![child], "abandon {label}");
            let open = refused_open(
                tree.achieve(goal, &format!("t8-{label}-achieve-0")).await,
                label,
            )?;
            assert_eq!(open, vec![child], "achieve {label}");
        }
        tree.achieve(a1x, "t8-a1x-achieve").await?;
        // A and P still sit above an open level.
        refused_open(tree.abandon(a, "t8-a-abandon-1").await, "a after a1x")?;
        refused_open(tree.abandon(p, "t8-p-abandon-1").await, "p after a1x")?;
        tree.achieve(a1, "t8-a1-achieve").await?;
        refused_open(tree.abandon(p, "t8-p-abandon-2").await, "p after a1")?;
        tree.abandon(a, "t8-a-abandon").await?;
        tree.abandon(p, "t8-p-abandon").await?;
        assert_tree_invariant(pool, "T8").await?;
        assert_eq!(open_below_closed(pool).await?, 0, "T8 any depth");
        Ok(())
    }
    .await;
    tree.finish(result, "T8").await;
}

/// G1/G2: abandon a goal without children, with all children closed (mixed),
/// and with all children Abandoned (no "one Achieved" rule on abandon).
#[tokio::test]
async fn closed_children_or_none_admit_the_abandon() {
    let tree = Tree::new().await.expect("fixture");
    let pool = tree.pg.pool_for_tests();
    let result = async {
        let lone = tree.create("ag1-p", &[]).await?;
        tree.abandon(lone, "ag1-abandon").await?;

        let p = tree.create("ag2-p", &[]).await?;
        let children = tree.decompose(p, &["ag2-a", "ag2-b"]).await?;
        tree.achieve(children[0], "ag2-a-achieve").await?;
        tree.abandon(children[1], "ag2-b-abandon").await?;
        tree.abandon(p, "ag2-abandon").await?;

        let p = tree.create("ag2b-p", &[]).await?;
        let children = tree.decompose(p, &["ag2b-a", "ag2b-b"]).await?;
        tree.abandon(children[0], "ag2b-a-abandon").await?;
        tree.abandon(children[1], "ag2b-b-abandon").await?;
        tree.abandon(p, "ag2b-abandon").await?;
        assert_tree_invariant(pool, "G1/G2").await?;
        Ok(())
    }
    .await;
    tree.finish(result, "G1/G2 abandon").await;
}

/// G3: pausing a parent over Active children is allowed, and so is resume.
#[tokio::test]
async fn pause_and_resume_ignore_open_children() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p = tree.create("ag3-p", &[]).await?;
        tree.decompose(p, &["ag3-a"]).await?;
        let paused = tree.pause(p, "ag3-pause").await?;
        tree.transition(paused, GoalTransitionKind::Resume, "ag3-resume")
            .await?;
        Ok(())
    }
    .await;
    tree.finish(result, "G3 pause").await;
}

/// G4: a plain dependency is not a child: abandoning X under an Active
/// dependent Y is allowed.
#[tokio::test]
async fn a_plain_dependent_does_not_block_abandon() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let x = tree.create("ag4-x", &[]).await?;
        tree.create("ag4-y", &[x]).await?;
        tree.abandon(x, "ag4-abandon").await?;
        Ok(())
    }
    .await;
    tree.finish(result, "G4 abandon").await;
}

/// G5: a successful abandon replays, writing nothing new.
#[tokio::test]
async fn an_abandoned_parent_replays() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p = tree.create("ag5-p", &[]).await?;
        let children = tree.decompose(p, &["ag5-a"]).await?;
        tree.abandon(children[0], "ag5-a-abandon").await?;
        let first = tree.abandon(p, "ag5-abandon").await?;
        let before = tree.counts().await?;
        let replay = tree.abandon(p, "ag5-abandon").await?;
        assert!(replay.idempotent_replay, "{replay:?}");
        assert_eq!(replay.goal_id, first.goal_id);
        assert_eq!(tree.counts().await?, before, "a replay writes nothing");
        Ok(())
    }
    .await;
    tree.finish(result, "G5 abandon").await;
}

/// G6: abandon takes the parent lock first; the queued decompose finds the
/// parent terminal and adds no child.
#[tokio::test]
async fn a_racing_abandon_first_refuses_the_decompose() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let p = tree.create("ag6-p", &[]).await?;
        let pool = tree.pg.pool_for_tests();
        let blocker = hold_lifecycle_lock(pool, p.into_inner()).await?;
        let (abandoned, decomposed, released) = tokio::join!(
            tree.abandon(p, "ag6-abandon"),
            async {
                await_lock_waiters(pool, 1, "G6 abandon queued").await?;
                Ok::<_, Box<dyn std::error::Error>>(tree.decompose(p, &["ag6-a"]).await)
            },
            async {
                await_lock_waiters(pool, 2, "G6 decompose queued").await?;
                blocker.commit().await?;
                Ok::<_, Box<dyn std::error::Error>>(())
            },
        );
        released?;
        abandoned?;
        let Err(err) = decomposed? else {
            return Err("G6: decompose after abandon must be refused".into());
        };
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
        assert_tree_invariant(pool, "G6 abandon").await?;
        Ok(())
    }
    .await;
    tree.finish(result, "G6 abandon").await;
}

// ============================================================
// Abandon from Paused (refs #515)
// ============================================================

/// The exact invalid-transition refusal, as an error rather than a panic.
fn refused_transition(
    result: Result<GoalWriteOutcome, StorageError>,
    expected: &str,
    what: &str,
) -> TestResult<()> {
    match result {
        Err(StorageError::ConstraintViolation(m)) if m == expected => Ok(()),
        other => {
            Err(format!("{what}: expected ConstraintViolation({expected:?}), got {other:?}").into())
        }
    }
}

fn delta(after: &[i64], before: &[i64]) -> Vec<i64> {
    after.iter().zip(before).map(|(a, b)| a - b).collect()
}

/// P1: a Paused goal abandons in one write, with the same footprint as an
/// Active abandon.
#[tokio::test]
async fn a_paused_goal_abandons_in_one_write() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let active = tree.create("p1-active", &[]).await?;
        let before = tree.counts().await?;
        tree.abandon(active, "p1-active-abandon").await?;
        let active_delta = delta(&tree.counts().await?, &before);

        let g = tree.create("p1-g", &[]).await?;
        let paused = tree.pause(g, "p1-pause").await?;
        let before = tree.counts().await?;
        let out = tree.abandon(paused, "p1-abandon").await?;
        let paused_delta = delta(&tree.counts().await?, &before);
        assert_eq!(
            tree.head_of(g).await?,
            out.goal_id.into_inner(),
            "the abandon is the head"
        );
        assert_eq!(
            paused_delta, active_delta,
            "goal, change, declaration, memory, outbox: same footprint as an Active abandon"
        );
        assert_eq!(paused_delta[0], 1, "one goal row");
        assert_eq!(paused_delta[2], 1, "one replay declaration");
        assert_eq!(paused_delta[4], 0, "no outbox row");
        let (state, close): (String, Option<Uuid>) =
            sqlx::query_as("SELECT state::text, close_fact_t FROM proxima_core.goal WHERE t = $1")
                .bind(out.goal_id.into_inner())
                .fetch_one(tree.pg.pool_for_tests())
                .await?;
        assert_eq!(state, "Abandoned");
        assert!(close.is_some(), "a terminal row carries its close fact");
        eprintln!("P1 delta (goal, change, declaration, memory, outbox): {paused_delta:?}");
        Ok(())
    }
    .await;
    tree.finish(result, "P1").await;
}

/// P2: no Active version between the pause and the abandon.
#[tokio::test]
async fn a_paused_abandon_has_no_active_version_between() {
    let tree = Tree::new_listenable().await.expect("fixture");
    let result = async {
        let g = tree.create("p2-g", &[]).await?;
        let paused = tree.pause(g, "p2-pause").await?;
        tree.abandon(paused, "p2-abandon").await?;
        let s = |v: &str| v.to_owned();
        assert_eq!(
            tree.history(g).await?,
            vec![
                (s("set"), s("Active"), 1, None, None),
                (
                    s("pause"),
                    s("Paused"),
                    2,
                    Some(g.into_inner()),
                    Some(s("keep"))
                ),
                (
                    s("abandon"),
                    s("Abandoned"),
                    3,
                    Some(paused.into_inner()),
                    None
                ),
            ]
        );
        Ok(())
    }
    .await;
    tree.finish(result, "P2").await;
}

/// P3: closed goals still refuse abandon, and the refusal writes nothing.
#[tokio::test]
async fn closed_goals_still_refuse_abandon() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let x = tree.create("p3-x", &[]).await?;
        let x = tree.abandon(x, "p3-x-abandon").await?.goal_id;
        let y = tree.create("p3-y", &[]).await?;
        let y = tree.achieve(y, "p3-y-achieve").await?.goal_id;
        let before = tree.counts().await?;
        refused_transition(
            tree.abandon(x, "p3-x-again").await,
            "invalid goal transition: Abandoned -> Abandoned",
            "P3 abandoned",
        )?;
        refused_transition(
            tree.abandon(y, "p3-y-abandon").await,
            "invalid goal transition: Achieved -> Abandoned",
            "P3 achieved",
        )?;
        assert_eq!(tree.counts().await?, before, "refusals write nothing");
        Ok(())
    }
    .await;
    tree.finish(result, "P3").await;
}

/// P4: a Paused abandon replays, and abandoning its result again is refused.
#[tokio::test]
async fn a_paused_abandon_replays_and_then_refuses() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let g = tree.create("p4-g", &[]).await?;
        let paused = tree.pause(g, "p4-pause").await?;
        let first = tree.abandon(paused, "p4-abandon").await?;
        let before = tree.counts().await?;
        let replay = tree.abandon(paused, "p4-abandon").await?;
        assert!(replay.idempotent_replay, "{replay:?}");
        assert_eq!(replay.goal_id, first.goal_id);
        assert_eq!(tree.counts().await?, before, "a replay writes nothing");
        refused_transition(
            tree.abandon(first.goal_id, "p4-again").await,
            "invalid goal transition: Abandoned -> Abandoned",
            "P4",
        )?;
        Ok(())
    }
    .await;
    tree.finish(result, "P4").await;
}

/// P5: a Paused parent with an open child is still refused; children close
/// first, a Paused child in one write too.
#[tokio::test]
async fn a_paused_parent_with_an_open_child_is_refused() {
    let tree = Tree::new().await.expect("fixture");
    let pool = tree.pg.pool_for_tests();
    let result = async {
        let p = tree.create("p5-p", &[]).await?;
        let children = tree.decompose(p, &["p5-a", "p5-b"]).await?;
        let (a, b) = (children[0], children[1]);
        let b_paused = tree.pause(b, "p5-b-pause").await?;
        let p_paused = tree.pause(p, "p5-p-pause").await?;
        let before = tree.counts().await?;
        let mut open = refused_open(tree.abandon(p_paused, "p5-p-abandon-early").await, "P5")?;
        let mut wanted = vec![a, b_paused];
        open.sort();
        wanted.sort();
        assert_eq!(open, wanted, "the open child heads");
        assert_eq!(tree.counts().await?, before, "a refusal writes nothing");
        assert_tree_invariant(pool, "P5 refused").await?;
        tree.abandon(a, "p5-a-abandon").await?;
        tree.abandon(b_paused, "p5-b-abandon").await?;
        let out = tree.abandon(p_paused, "p5-p-abandon").await?;
        assert_eq!(tree.head_of(p).await?, out.goal_id.into_inner());
        assert_tree_invariant(pool, "P5 done").await?;
        Ok(())
    }
    .await;
    tree.finish(result, "P5").await;
}

/// P6: a Paused chain closes only bottom-up.
#[tokio::test]
async fn a_paused_chain_closes_only_bottom_up() {
    let tree = Tree::new().await.expect("fixture");
    let pool = tree.pg.pool_for_tests();
    let result = async {
        let p = tree.create("p6-p", &[]).await?;
        let a = tree.decompose(p, &["p6-a"]).await?[0];
        let a1 = tree.decompose(a, &["p6-a1"]).await?[0];
        let p_paused = tree.pause(p, "p6-p-pause").await?;
        let a_paused = tree.pause(a, "p6-a-pause").await?;
        assert_tree_invariant(pool, "P6 paused").await?;
        let open = refused_open(tree.abandon(p_paused, "p6-p-abandon-early").await, "P6 p")?;
        assert_eq!(open, vec![a_paused], "P names A's head");
        assert_tree_invariant(pool, "P6 p refused").await?;
        let open = refused_open(tree.abandon(a_paused, "p6-a-abandon-early").await, "P6 a")?;
        assert_eq!(open, vec![a1], "A names A1");
        assert_tree_invariant(pool, "P6 a refused").await?;
        tree.abandon(a1, "p6-a1-abandon").await?;
        assert_tree_invariant(pool, "P6 a1").await?;
        tree.abandon(a_paused, "p6-a-abandon").await?;
        assert_tree_invariant(pool, "P6 a").await?;
        tree.abandon(p_paused, "p6-p-abandon").await?;
        assert_tree_invariant(pool, "P6 p").await?;
        Ok(())
    }
    .await;
    tree.finish(result, "P6").await;
}

async fn head_state(tree: &Tree, goal: GoalId) -> TestResult<String> {
    Ok(sqlx::query_scalar(
        "SELECT g.state::text FROM proxima_core.goal_head h
           JOIN proxima_core.goal g ON g.t = h.t
          WHERE h.handle = (SELECT handle FROM proxima_core.goal WHERE t = $1)",
    )
    .bind(goal.into_inner())
    .fetch_one(tree.pg.pool_for_tests())
    .await?)
}

/// P7a: abandon takes the Paused head's lock first; the queued resume loses.
#[tokio::test]
async fn a_racing_abandon_first_refuses_the_resume() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let g = tree.create("p7a-g", &[]).await?;
        let paused = tree.pause(g, "p7a-pause").await?;
        let pool = tree.pg.pool_for_tests();
        let before = tree.counts().await?;
        let blocker = hold_lifecycle_lock(pool, paused.into_inner()).await?;
        let (abandoned, resumed, released) = tokio::join!(
            tree.abandon(paused, "p7a-abandon"),
            async {
                await_lock_waiters(pool, 1, "P7a abandon queued").await?;
                Ok::<_, Box<dyn std::error::Error>>(tree.resume_raw(paused, "p7a-resume").await)
            },
            async {
                await_lock_waiters(pool, 2, "P7a resume queued").await?;
                blocker.commit().await?;
                Ok::<_, Box<dyn std::error::Error>>(())
            },
        );
        released?;
        abandoned?;
        let err = match resumed? {
            Ok(out) => return Err(format!("P7a: the queued resume must lose, got {out:?}").into()),
            Err(err) => err,
        };
        eprintln!("P7a loser: {err:?}");
        assert!(matches!(err, StorageError::Conflict(_)), "P7a: {err:?}");
        assert_eq!(head_state(&tree, g).await?, "Abandoned");
        assert_eq!(
            tree.counts().await?[0] - before[0],
            1,
            "exactly one new goal row"
        );
        Ok(())
    }
    .await;
    tree.finish(result, "P7a").await;
}

/// P7b: resume takes the Paused head's lock first; the queued abandon loses.
#[tokio::test]
async fn a_racing_resume_first_refuses_the_abandon() {
    let tree = Tree::new().await.expect("fixture");
    let result = async {
        let g = tree.create("p7b-g", &[]).await?;
        let paused = tree.pause(g, "p7b-pause").await?;
        let pool = tree.pg.pool_for_tests();
        let before = tree.counts().await?;
        let blocker = hold_lifecycle_lock(pool, paused.into_inner()).await?;
        let (resumed, abandoned, released) = tokio::join!(
            tree.resume_raw(paused, "p7b-resume"),
            async {
                await_lock_waiters(pool, 1, "P7b resume queued").await?;
                Ok::<_, Box<dyn std::error::Error>>(tree.abandon(paused, "p7b-abandon").await)
            },
            async {
                await_lock_waiters(pool, 2, "P7b abandon queued").await?;
                blocker.commit().await?;
                Ok::<_, Box<dyn std::error::Error>>(())
            },
        );
        released?;
        resumed?;
        let err = match abandoned? {
            Ok(out) => return Err(format!("P7b: the queued abandon must lose, got {out:?}").into()),
            Err(err) => err,
        };
        eprintln!("P7b loser: {err:?}");
        assert!(matches!(err, StorageError::Conflict(_)), "P7b: {err:?}");
        assert_eq!(head_state(&tree, g).await?, "Active");
        assert_eq!(
            tree.counts().await?[0] - before[0],
            1,
            "exactly one new goal row"
        );
        Ok(())
    }
    .await;
    tree.finish(result, "P7b").await;
}

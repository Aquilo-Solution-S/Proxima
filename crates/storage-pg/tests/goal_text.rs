//! A Goal version stores its prompt text, and a read returns it with when the
//! series was set and when that version was written.
//!
//! Every write goes through the real Goal write verbs on a real Postgres; the
//! reads are `Engine::query`'s storage leg.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use proxima_core::owner_inverse::{
    EraseAuthorization, ExportAuthorization, OwnerEraseOutcome, OwnerEraseTarget,
    OwnerExportTarget, OwnerSurfaces,
};
use proxima_core::storage_ports::{
    FactIngestPort, GoalWritePort, MemoryReadPort, OwnerInversePort, OwnerWritePermit,
};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    GoalAssignmentTarget, GoalAtomicContext, GoalAuthorship, GoalDraft, GoalEvidenceRef,
    GoalPayloadWrite, GoalReplayRequest, GoalState, GoalTopologyWrite, GoalWriteOutcome,
    IdempotencyKey, ModifyGoalAtomicRequest, TransitionGoalAtomicRequest,
};
use proxima_core::verbs::query::{EntityKind, GoalRow, QueryRequest};
use proxima_core::{
    AccessKind, EdgeEndpoint, FlavorRegistry, FlavorRegistryFrozen, GoalId, GoalPayload, MemoryId,
    OwnerRef, SchemaId, SchemaVersion, SimpleTextGoalV1, StorageError, UserId,
};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::verbs::forget::MemoryColdStore;
use proxima_storage_pg::verbs::goal_timeseries::{
    GoalWriteCommand, GoalWriteOutcome as RowWritten, write_goal,
};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The same pause a real clock gives two writes: `t` carries milliseconds, so
/// two versions written closer than that could not tell `changed_at` from
/// `set_at`.
const TICK: Duration = Duration::from_millis(15);

fn memory_draft(kind: &str) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new(format!("goal-text/{kind}")),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: kind.into(),
    }
}

async fn fresh_pg() -> (String, PgStorage) {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let pg = PgStorage::connect(&db_url(&db_name))
        .await
        .expect("connect")
        .with_cold(Arc::new(MemoryColdStore::default()));
    pg.run_before_owner_rls_migrations().await.expect("migrate");
    (db_name, pg)
}

/// The instant a v7 `t` was minted at, read out of the identifier itself:
/// an answer that does not come through the query under test.
fn minted_at(id: GoalId) -> time::OffsetDateTime {
    let (secs, nanos) = id
        .into_inner()
        .get_timestamp()
        .expect("a Goal t is a UUIDv7")
        .to_unix();
    time::OffsetDateTime::from_unix_timestamp_nanos(
        i128::from(secs) * 1_000_000_000 + i128::from(nanos),
    )
    .expect("a minted instant is in range")
}

/// One owner with an assigned Perspective, and the verbs that write Goals.
struct Harness {
    pg: PgStorage,
    owner: OwnerRef,
    permit: OwnerWritePermit,
    registry: FlavorRegistryFrozen,
    assignment: MemoryId,
}

impl Harness {
    async fn new(pg: PgStorage) -> TestResult<Self> {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let fact = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?;
        let mut abstraction = memory_draft("abstraction");
        abstraction.additional_references =
            vec![EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)];
        let abstraction = pg.ingest_fact_atomic(&permit, &abstraction, None).await?;
        let mut perspective = memory_draft("perspective");
        perspective.additional_references = vec![EdgeEndpoint::memory(
            EntityKind::Abstraction,
            abstraction.memory_id,
        )];
        let perspective = pg.ingest_fact_atomic(&permit, &perspective, None).await?;
        Ok(Self {
            pg,
            owner,
            permit,
            registry: FlavorRegistry::new().freeze_or_panic_for_tests(),
            assignment: perspective.memory_id,
        })
    }

    fn context(&self) -> GoalAtomicContext<'_> {
        GoalAtomicContext {
            registry: &self.registry,
            author_self_perspective_id: None,
        }
    }

    fn payload(title: &str, text: &str) -> GoalPayloadWrite {
        GoalPayloadWrite {
            schema_id: SchemaId::new(SimpleTextGoalV1::SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(SimpleTextGoalV1::SCHEMA_VERSION),
            title: title.into(),
            text: text.into(),
            payload: Vec::new(),
            sidecar_payload: None,
        }
    }

    fn topology(&self) -> TestResult<GoalTopologyWrite> {
        Ok(GoalTopologyWrite::new(
            GoalAssignmentTarget::perspective(self.assignment),
            Vec::new(),
            Vec::new(),
        )?)
    }

    async fn create(
        &self,
        request_id: &str,
        title: &str,
        text: &str,
    ) -> TestResult<GoalWriteOutcome> {
        let draft = GoalDraft::active_from_payload_write(
            self.owner,
            Self::payload(title, text),
            self.topology()?,
            None,
            GoalAuthorship::User,
            IdempotencyKey::new(request_id)?,
        );
        Ok(self
            .pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft,
                    context: self.context(),
                    write_act_t: None,
                },
                &self.permit,
            )
            .await?)
    }

    async fn transition(
        &self,
        prior: GoalId,
        next_state: GoalState,
        request_id: &str,
    ) -> TestResult<GoalWriteOutcome> {
        Ok(self
            .pg
            .transition_goal_atomic(
                &TransitionGoalAtomicRequest {
                    owner: self.owner,
                    prior_goal_id: prior,
                    next_state,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new(request_id)?,
                    context: self.context(),
                },
                &self.permit,
            )
            .await?)
    }

    async fn modify(
        &self,
        prior: GoalId,
        request_id: &str,
        title: &str,
        text: &str,
    ) -> TestResult<GoalWriteOutcome> {
        Ok(self
            .pg
            .modify_goal_atomic(
                &ModifyGoalAtomicRequest {
                    owner: self.owner,
                    prior_goal_id: prior,
                    replacement: Self::payload(title, text),
                    wake: None,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new(request_id)?,
                    context: self.context(),
                    evidence: None,
                },
                &self.permit,
            )
            .await?)
    }

    async fn evidence(&self) -> TestResult<MemoryId> {
        Ok(self
            .pg
            .ingest_fact_atomic(&self.permit, &memory_draft("fact"), None)
            .await?
            .memory_id)
    }

    async fn achieve(&self, prior: GoalId, request_id: &str) -> TestResult<GoalWriteOutcome> {
        let evidence = self.evidence().await?;
        Ok(self
            .pg
            .achieve_goal_atomic(
                &AchieveGoalAtomicRequest {
                    owner: self.owner,
                    prior_goal_id: prior,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new(request_id)?,
                    context: self.context(),
                    evidence: vec![GoalEvidenceRef::new(evidence)],
                },
                &self.permit,
            )
            .await?)
    }

    /// The ids of the children, in the order given.
    async fn decompose(
        &self,
        parent: GoalId,
        prefix: &str,
        children: &[(&str, &str)],
    ) -> TestResult<Vec<GoalId>> {
        let children = children
            .iter()
            .enumerate()
            .map(|(index, (title, text))| {
                Ok(ChildGoalDraft {
                    payload: Self::payload(title, text),
                    evidence: Vec::new(),
                    wake: None,
                    request_id: IdempotencyKey::new(format!("{prefix}-{index}"))?,
                })
            })
            .collect::<TestResult<Vec<_>>>()?;
        let outcome = self
            .pg
            .decompose_goal_atomic(
                &DecomposeGoalAtomicRequest {
                    owner: self.owner,
                    parent_goal_id: parent,
                    authorship: GoalAuthorship::User,
                    context: self.context(),
                    topology: self.topology()?,
                    children,
                },
                &self.permit,
            )
            .await?;
        Ok(outcome
            .children
            .into_iter()
            .map(|child| child.outcome.goal_id)
            .collect())
    }

    /// One version, read by its id: the single read.
    async fn read(&self, id: GoalId) -> TestResult<GoalRow> {
        let mut req = QueryRequest::readable();
        req.entity_kind = Some(EntityKind::Goal);
        req.goal_ids = vec![id];
        req.limit = 1;
        let response = self
            .pg
            .query_memories(None, &[self.owner], &req, &[])
            .await?;
        response
            .goals
            .into_iter()
            .next()
            .ok_or_else(|| format!("goal {id:?} was not returned").into())
    }

    /// The current version of every series: the listing.
    async fn heads(&self) -> TestResult<Vec<GoalRow>> {
        let mut req = QueryRequest::readable();
        req.entity_kind = Some(EntityKind::Goal);
        let response = self
            .pg
            .query_memories(None, &[self.owner], &req, &[])
            .await?;
        Ok(response.goals)
    }

    fn user_id(&self) -> UserId {
        match self.owner {
            OwnerRef::Personal(user_id) => user_id,
            OwnerRef::Group(_) => unreachable!("the harness owns a personal owner"),
        }
    }

    /// Insert a one-version Goal series the way a Goal written before v0.0.13
    /// sits in the table: no declaration, no text.
    async fn insert_declarationless(&self, request_id: &str, state: &str) -> TestResult<GoalId> {
        let pool = self.pg.pool_for_tests();
        let handle = Uuid::now_v7();
        let t = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(handle)
        .bind(SimpleTextGoalV1::SCHEMA_ID)
        .bind(self.owner.stored_owner_id())
        .bind(t)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.goal
                 (handle, t, owner_id, title, state, request_id, assignment_t)
             VALUES ($1, $2, $3, 'written before v0.0.13', $4::proxima_core.goal_state, $5, $6)",
        )
        .bind(handle)
        .bind(t)
        .bind(self.owner.stored_owner_id())
        .bind(state)
        .bind(request_id)
        .bind(self.assignment.into_inner())
        .execute(pool)
        .await?;
        Ok(GoalId::new(t))
    }
}

macro_rules! goal_test {
    ($name:ident, |$h:ident| $body:expr) => {
        #[tokio::test]
        async fn $name() {
            let (db_name, pg) = fresh_pg().await;
            let result: TestResult = async {
                let $h = Harness::new(pg.clone()).await?;
                $body.await
            }
            .await;
            drop(pg);
            let _ = drop_db(&db_name).await;
            result.expect(stringify!($name));
        }
    };
}

goal_test!(
    create_then_read_returns_text_by_listing_and_by_id,
    |h| async {
        let created = h
            .create("create-read", "the title", "the prompt of this goal")
            .await?;

        let single = h.read(created.goal_id).await?;
        assert_eq!(single.text.as_deref(), Some("the prompt of this goal"));
        assert_eq!(
            single.set_at, single.changed_at,
            "a first version is both when the series was set and when it changed"
        );
        assert_eq!(single.set_at, minted_at(created.goal_id));

        let listing = h.heads().await?;
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].id, created.goal_id);
        assert_eq!(
            listing[0].text.as_deref(),
            Some("the prompt of this goal"),
            "the listing returns the full text, not only the single read"
        );
        assert_eq!(listing[0].set_at, single.set_at);
        assert_eq!(listing[0].changed_at, single.changed_at);
        Ok(())
    }
);

goal_test!(
    pause_then_resume_keep_the_text_and_the_set_time,
    |h| async {
        let first = h.create("pause-1", "pausable", "keep going").await?;
        tokio::time::sleep(TICK).await;
        let paused = h
            .transition(first.goal_id, GoalState::Paused, "pause-2")
            .await?;
        tokio::time::sleep(TICK).await;
        let resumed = h
            .transition(paused.goal_id, GoalState::Active, "pause-3")
            .await?;

        let mut previous_change = None;
        for (id, state) in [
            (first.goal_id, GoalState::Active),
            (paused.goal_id, GoalState::Paused),
            (resumed.goal_id, GoalState::Active),
        ] {
            let row = h.read(id).await?;
            assert_eq!(row.state, state);
            assert_eq!(
                row.text.as_deref(),
                Some("keep going"),
                "every version keeps the text: {state:?}"
            );
            assert_eq!(
                row.set_at,
                minted_at(first.goal_id),
                "the series was set once, by its first version"
            );
            assert_eq!(
                row.changed_at,
                minted_at(id),
                "the change time is this version's own"
            );
            if let Some(previous) = previous_change {
                assert!(
                    row.changed_at > previous,
                    "a later version changed later than the one before it"
                );
            }
            previous_change = Some(row.changed_at);
        }

        let listing = h.heads().await?;
        assert_eq!(listing.len(), 1, "one series, one head");
        assert_eq!(listing[0].id, resumed.goal_id);
        assert_eq!(listing[0].text.as_deref(), Some("keep going"));
        Ok(())
    }
);

goal_test!(
    modify_with_a_new_text_stores_it_and_keeps_the_set_time,
    |h| async {
        let first = h
            .create("modify-1", "first title", "the first text")
            .await?;
        tokio::time::sleep(TICK).await;
        let modified = h
            .modify(first.goal_id, "modify-2", "second title", "the second text")
            .await?;

        let before = h.read(first.goal_id).await?;
        let after = h.read(modified.goal_id).await?;
        assert_eq!(before.text.as_deref(), Some("the first text"));
        assert_eq!(after.text.as_deref(), Some("the second text"));
        assert_eq!(
            before.set_at, after.set_at,
            "modifying does not re-set a goal"
        );
        assert_eq!(after.set_at, minted_at(first.goal_id));
        assert!(after.changed_at > before.changed_at);
        assert_eq!(after.changed_at, minted_at(modified.goal_id));

        // A transition after the modify carries the NEW text, not the first one.
        tokio::time::sleep(TICK).await;
        let paused = h
            .transition(modified.goal_id, GoalState::Paused, "modify-3")
            .await?;
        assert_eq!(
            h.read(paused.goal_id).await?.text.as_deref(),
            Some("the second text")
        );
        Ok(())
    }
);

goal_test!(each_decomposed_child_carries_its_own_text, |h| async {
    let parent = h
        .create("decompose-parent", "parent", "the parent text")
        .await?;
    tokio::time::sleep(TICK).await;
    let children = h
        .decompose(
            parent.goal_id,
            "decompose-child",
            &[
                ("child one", "the first child text"),
                ("child two", "the second child text"),
            ],
        )
        .await?;
    assert_eq!(children.len(), 2);

    let first = h.read(children[0]).await?;
    let second = h.read(children[1]).await?;
    assert_eq!(first.text.as_deref(), Some("the first child text"));
    assert_eq!(second.text.as_deref(), Some("the second child text"));
    for child in [&first, &second] {
        assert_eq!(
            child.set_at, child.changed_at,
            "a child is the first version of its own series"
        );
    }
    let parent_row = h.read(parent.goal_id).await?;
    assert_eq!(
        parent_row.text.as_deref(),
        Some("the parent text"),
        "decomposing leaves the parent as it was"
    );
    assert!(
        first.set_at > parent_row.set_at,
        "a child is set after its parent"
    );
    Ok(())
});

goal_test!(achieve_and_abandon_keep_the_text, |h| async {
    let to_achieve = h.create("close-1", "achievable", "reach this").await?;
    let to_abandon = h.create("close-2", "abandonable", "drop this").await?;
    tokio::time::sleep(TICK).await;
    let achieved = h.achieve(to_achieve.goal_id, "close-1-achieve").await?;
    let abandoned = h
        .transition(to_abandon.goal_id, GoalState::Abandoned, "close-2-abandon")
        .await?;

    let achieved_row = h.read(achieved.goal_id).await?;
    assert_eq!(achieved_row.state, GoalState::Achieved);
    assert_eq!(achieved_row.text.as_deref(), Some("reach this"));
    assert_eq!(achieved_row.set_at, minted_at(to_achieve.goal_id));
    assert!(achieved_row.changed_at > achieved_row.set_at);

    let abandoned_row = h.read(abandoned.goal_id).await?;
    assert_eq!(abandoned_row.state, GoalState::Abandoned);
    assert_eq!(abandoned_row.text.as_deref(), Some("drop this"));
    assert_eq!(abandoned_row.set_at, minted_at(to_abandon.goal_id));
    assert!(abandoned_row.changed_at > abandoned_row.set_at);
    Ok(())
});

goal_test!(
    a_version_with_unknown_text_stays_unknown_when_it_moves_on,
    |h| async {
        // A Goal written before its declaration was recorded has no text anywhere.
        // Moving it on must not make one up, and must not store a blank.
        let old = h.insert_declarationless("old-1", "Active").await?;
        assert_eq!(h.read(old).await?.text, None);
        tokio::time::sleep(TICK).await;

        let paused = h.transition(old, GoalState::Paused, "old-2").await?;
        let resumed = h
            .transition(paused.goal_id, GoalState::Active, "old-3")
            .await?;
        for id in [paused.goal_id, resumed.goal_id] {
            let row = h.read(id).await?;
            assert_eq!(row.text, None, "unknown stays unknown");
            assert_eq!(row.set_at, minted_at(old));
        }

        // And a modify is the first version that can know its text.
        let modified = h
            .modify(resumed.goal_id, "old-4", "now known", "a text at last")
            .await?;
        assert_eq!(
            h.read(modified.goal_id).await?.text.as_deref(),
            Some("a text at last")
        );
        assert_eq!(h.read(old).await?.text, None, "history is not rewritten");
        Ok(())
    }
);

/// The Rust side of the migration's fixed place: this is the declaration
/// envelope the backfill reads (`replay.rs` `command_declaration`).
async fn declaration_of(pg: &PgStorage, id: GoalId) -> TestResult<serde_json::Value> {
    Ok(sqlx::query_scalar(
        "SELECT declaration FROM proxima_core.goal_replay_declaration WHERE goal_t = $1",
    )
    .bind(id.into_inner())
    .fetch_one(pg.pool_for_tests())
    .await?)
}

async fn stored_text(pg: &PgStorage, id: GoalId) -> TestResult<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT text FROM proxima_core.goal WHERE t = $1")
            .bind(id.into_inner())
            .fetch_one(pg.pool_for_tests())
            .await?,
    )
}

goal_test!(
    a_transition_recorded_before_the_change_still_replays,
    |h| async {
        let source = h.create("replay-source", "replayable", "replay me").await?;

        // The declaration a transition and an achievement record names the
        // request, never the Goal's text. It is what a v0.0.30 binary wrote, so a
        // request recorded then and retried now compares equal.
        let transition = h
            .transition(source.goal_id, GoalState::Paused, "replay-live")
            .await?;
        assert_eq!(
            declaration_of(&h.pg, transition.goal_id).await?,
            serde_json::json!({
                "version": 1,
                "verb": "transition",
                "prior_goal_id": source.goal_id.into_inner(),
                "next_state": "Paused",
                "authorship": "user",
            }),
            "the transition declaration is the one v0.0.30 recorded"
        );
        let live_retry = h
            .transition(source.goal_id, GoalState::Paused, "replay-live")
            .await?;
        assert!(
            live_retry.idempotent_replay,
            "a retry of a new write replays"
        );
        assert_eq!(live_retry.goal_id, transition.goal_id);
        let resumed = h
            .transition(transition.goal_id, GoalState::Active, "replay-live-resume")
            .await?;
        let achieved = h.achieve(resumed.goal_id, "replay-live-achieve").await?;
        let evidence: Vec<MemoryId> = sqlx::query_scalar::<_, Vec<Uuid>>(
            "SELECT evidence_t FROM proxima_core.goal WHERE t = $1",
        )
        .bind(achieved.goal_id.into_inner())
        .fetch_one(h.pg.pool_for_tests())
        .await?
        .into_iter()
        .map(MemoryId::new)
        .collect();
        assert_eq!(
            declaration_of(&h.pg, achieved.goal_id).await?,
            serde_json::json!({
                "version": 1,
                "verb": "achieve",
                "prior_goal_id": resumed.goal_id.into_inner(),
                "authorship": "user",
                "evidence": evidence
                    .iter()
                    .map(|id| serde_json::json!({"memory_id": id.into_inner()}))
                    .collect::<Vec<_>>(),
            }),
            "the achieve declaration is the one v0.0.30 recorded"
        );

        // A transition recorded by that binary: a Goal row with no text column
        // (so NULL) and the declaration above, written as it wrote them.
        let pool = h.pg.pool_for_tests();
        let source_two = h
            .create("replay-source-two", "replayable two", "also me")
            .await?;
        // The series stood in the table before the upgrade, so its first
        // version has no text either; the database refuses a version that
        // changes the state and not the text.
        as_written_before_the_column(&h.pg, source_two.goal_id).await?;
        let recorded = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.goal
             (handle, t, owner_id, title, state, request_id, assignment_t)
         SELECT handle, $1, owner_id, title, 'Paused', 'recorded-before', assignment_t
           FROM proxima_core.goal WHERE t = $2",
        )
        .bind(recorded)
        .bind(source_two.goal_id.into_inner())
        .execute(pool)
        .await?;
        sqlx::query(
            "UPDATE proxima_core.goal_head SET t = $1
          WHERE handle = (SELECT handle FROM proxima_core.goal WHERE t = $2)",
        )
        .bind(recorded)
        .bind(source_two.goal_id.into_inner())
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.goal_replay_declaration (goal_t, declaration, edge_count)
         VALUES ($1, $2, 1)",
        )
        .bind(recorded)
        .bind(serde_json::json!({
            "version": 1,
            "verb": "transition",
            "prior_goal_id": source_two.goal_id.into_inner(),
            "next_state": "Paused",
            "authorship": "user",
        }))
        .execute(pool)
        .await?;
        let before: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.goal")
            .fetch_one(pool)
            .await?;

        let retry = h
            .transition(source_two.goal_id, GoalState::Paused, "recorded-before")
            .await?;
        assert!(
            retry.idempotent_replay,
            "the retry replays, it does not conflict"
        );
        assert_eq!(retry.goal_id, GoalId::new(recorded));
        assert_eq!(retry.edge_count, 1);
        let after: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.goal")
            .fetch_one(pool)
            .await?;
        assert_eq!(after, before, "a replay writes no second version");
        assert_eq!(
            stored_text(&h.pg, GoalId::new(recorded)).await?,
            None,
            "a replay rewrites nothing: the recorded version stays as recorded"
        );

        // Now the upgrade runs over that recorded transition: the column goes
        // away and 0025 backfills it from the declarations. The recorded version
        // gets the text its series declared, and the retry still replays.
        rewind_to_0024(&h.pg).await?;
        apply_0025(&h.pg).await?;
        assert_eq!(
            stored_text(&h.pg, GoalId::new(recorded)).await?.as_deref(),
            Some("also me"),
            "the backfill gave the recorded transition its series' text"
        );
        let retry = h
            .transition(source_two.goal_id, GoalState::Paused, "recorded-before")
            .await?;
        assert!(
            retry.idempotent_replay,
            "a transition recorded before the migration still replays after it"
        );
        assert_eq!(retry.goal_id, GoalId::new(recorded));
        let after_upgrade: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.goal")
                .fetch_one(pool)
                .await?;
        assert_eq!(after_upgrade, before, "a replay writes no second version");

        // The storage-level resolver the engine probes first agrees.
        let request = TransitionGoalAtomicRequest {
            owner: h.owner,
            prior_goal_id: source_two.goal_id,
            next_state: GoalState::Paused,
            authorship: GoalAuthorship::User,
            request_id: IdempotencyKey::new("recorded-before")?,
            context: h.context(),
        };
        assert!(
            h.pg.resolve_goal_replay(GoalReplayRequest::Transition(&request), &h.permit)
                .await?
                .is_some()
        );

        // Identity still discriminates: another command under the same key is a
        // conflict, not a replay.
        let conflict = h
            .transition(source_two.goal_id, GoalState::Abandoned, "recorded-before")
            .await
            .expect_err("a different transition under a recorded key must conflict");
        assert!(
            conflict
                .downcast_ref::<StorageError>()
                .is_some_and(|error| matches!(error, StorageError::IdempotencyConflict { .. })),
            "{conflict}"
        );
        Ok(())
    }
);

/// Give one version the NULL text a row has when it was written before the
/// column existed. `goal` is append-only, so this switches the guard off for
/// the one statement, as the backfill of `0025` does.
async fn as_written_before_the_column(pg: &PgStorage, id: GoalId) -> TestResult {
    let mut tx = pg.pool_for_tests().begin().await?;
    sqlx::query("ALTER TABLE proxima_core.goal DISABLE TRIGGER goal_append_only")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE proxima_core.goal SET text = NULL WHERE t = $1")
        .bind(id.into_inner())
        .execute(&mut *tx)
        .await?;
    sqlx::query("ALTER TABLE proxima_core.goal ENABLE TRIGGER goal_append_only")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Re-apply `0025` to a database that already has it, as a restore of an
/// older database followed by the upgrade would: drop what it adds first.
async fn rewind_to_0024(pg: &PgStorage) -> TestResult {
    let pool = pg.pool_for_tests();
    sqlx::query("DROP TRIGGER goal_text_carried ON proxima_core.goal")
        .execute(pool)
        .await?;
    sqlx::query("DROP FUNCTION proxima_core.goal_text_carried()")
        .execute(pool)
        .await?;
    sqlx::query("ALTER TABLE proxima_core.goal DROP COLUMN text")
        .execute(pool)
        .await?;
    Ok(())
}

async fn apply_0025(pg: &PgStorage) -> TestResult {
    let migrator = proxima_storage_pg::core_migrator();
    let migration = migrator
        .iter()
        .find(|migration| migration.version == 25)
        .ok_or("0025 is embedded")?;
    sqlx::raw_sql(migration.sql.clone())
        .execute(pg.pool_for_tests())
        .await?;
    Ok(())
}

async fn all_stored_texts(pg: &PgStorage) -> TestResult<BTreeMap<Uuid, Option<String>>> {
    Ok(
        sqlx::query_as::<_, (Uuid, Option<String>)>("SELECT t, text FROM proxima_core.goal")
            .fetch_all(pg.pool_for_tests())
            .await?
            .into_iter()
            .collect(),
    )
}

goal_test!(
    the_backfill_follows_the_goals_the_write_paths_declared,
    |h| async {
        let mut expected: BTreeMap<Uuid, Option<String>> = BTreeMap::new();
        let mut expect = |id: GoalId, text: Option<&str>| {
            expected.insert(id.into_inner(), text.map(str::to_owned));
        };

        // A: create, pause, resume, modify, pause. The modify must not reach
        // back into the versions before it.
        let a1 = h.create("fill-a1", "series a", "alpha").await?;
        let a2 = h
            .transition(a1.goal_id, GoalState::Paused, "fill-a2")
            .await?;
        let a3 = h
            .transition(a2.goal_id, GoalState::Active, "fill-a3")
            .await?;
        let a4 = h.modify(a3.goal_id, "fill-a4", "series a", "beta").await?;
        let a5 = h
            .transition(a4.goal_id, GoalState::Paused, "fill-a5")
            .await?;
        for (id, text) in [
            (a1.goal_id, "alpha"),
            (a2.goal_id, "alpha"),
            (a3.goal_id, "alpha"),
            (a4.goal_id, "beta"),
            (a5.goal_id, "beta"),
        ] {
            expect(id, Some(text));
        }

        // B: a decomposed parent and its children, each child its own series.
        let b = h.create("fill-b", "series b", "parent text").await?;
        let children = h
            .decompose(
                b.goal_id,
                "fill-b-child",
                &[("child 1", "first child"), ("child 2", "second child")],
            )
            .await?;
        expect(b.goal_id, Some("parent text"));
        expect(children[0], Some("first child"));
        expect(children[1], Some("second child"));

        // C: a goal that was achieved.
        let c1 = h.create("fill-c1", "series c", "gamma").await?;
        let c2 = h.achieve(c1.goal_id, "fill-c2").await?;
        expect(c1.goal_id, Some("gamma"));
        expect(c2.goal_id, Some("gamma"));

        // D: written before declarations were recorded: no declaration at all.
        let d = h.insert_declarationless("fill-d", "Active").await?;
        expect(d, None);

        // E: a series whose first versions predate declarations, then a modify
        // that declares one. Nothing before the modify can know the text, and
        // nothing is guessed.
        let e1 = h.create("fill-e1", "series e", "delta").await?;
        let e2 = h
            .transition(e1.goal_id, GoalState::Paused, "fill-e2")
            .await?;
        let e3 = h
            .transition(e2.goal_id, GoalState::Active, "fill-e3")
            .await?;
        let e4 = h
            .modify(e3.goal_id, "fill-e4", "series e", "epsilon")
            .await?;
        let e5 = h
            .transition(e4.goal_id, GoalState::Paused, "fill-e5")
            .await?;
        for id in [e1.goal_id, e2.goal_id, e3.goal_id] {
            expect(id, Some("delta"));
        }
        expect(e4.goal_id, Some("epsilon"));
        expect(e5.goal_id, Some("epsilon"));

        // The live columns hold what the write paths gave, before the migration.
        assert_eq!(
            all_stored_texts(&h.pg).await?,
            expected,
            "the write paths store what the backfill must reproduce"
        );

        // Make the first three versions of E look like ones written before
        // declarations were recorded: no declaration to read a text from.
        sqlx::query("DELETE FROM proxima_core.goal_replay_declaration WHERE goal_t = ANY($1)")
            .bind(
                [e1.goal_id, e2.goal_id, e3.goal_id]
                    .map(GoalId::into_inner)
                    .to_vec(),
            )
            .execute(h.pg.pool_for_tests())
            .await?;
        for id in [e1.goal_id, e2.goal_id, e3.goal_id] {
            expected.insert(id.into_inner(), None);
        }

        // Stand the database back at the 0024 schema: no text column. Every
        // declaration the write paths recorded is still there.
        rewind_to_0024(&h.pg).await?;
        apply_0025(&h.pg).await?;
        assert_eq!(
            all_stored_texts(&h.pg).await?,
            expected,
            "the backfill gives each version the text the write paths gave it, \
         and the declaration-less versions stay NULL"
        );

        // Idempotent: a second run changes nothing.
        apply_0025(&h.pg).await?;
        assert_eq!(all_stored_texts(&h.pg).await?, expected);

        // The append-only guard is back on after the backfill.
        let refused = sqlx::query("UPDATE proxima_core.goal SET title = title")
            .execute(h.pg.pool_for_tests())
            .await
            .expect_err("the Goal table must stay append-only");
        assert!(refused.to_string().contains("append-only"), "{refused}");
        Ok(())
    }
);

/// The six ASCII whitespace characters: space, tab, line feed, carriage
/// return, vertical tab, form feed. The database counts a text made only of
/// these as blank; it has no other definition of blank.
const ASCII_WHITESPACE: [char; 6] = [' ', '\t', '\n', '\r', '\u{b}', '\u{c}'];

/// One Goal row written straight to the tables with `text` as given.
async fn insert_goal_row(
    h: &Harness,
    text: Option<&str>,
    request_id: &str,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
    let pool = h.pg.pool_for_tests();
    let handle = Uuid::now_v7();
    let t = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
         VALUES ($1, 'core/simple-text-v1', $2, $3)",
    )
    .bind(handle)
    .bind(h.owner.stored_owner_id())
    .bind(t)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.goal (handle, t, owner_id, title, text, state, request_id)
         VALUES ($1, $2, $3, 'check', $4, 'Active', $5)",
    )
    .bind(handle)
    .bind(t)
    .bind(h.owner.stored_owner_id())
    .bind(text)
    .bind(request_id)
    .execute(pool)
    .await
}

fn refused_by_the_text_check(error: &(dyn std::error::Error + 'static)) -> bool {
    error.to_string().contains("goal_text_nonblank_chk")
}

goal_test!(the_database_refuses_a_blank_text, |h| async {
    // Every one of the six alone, all of them together, and the empty string.
    let mut blanks: Vec<String> = ASCII_WHITESPACE.iter().map(char::to_string).collect();
    blanks.push(String::new());
    blanks.push(" \t\x0b\x0c\r\n".to_owned());
    blanks.push(ASCII_WHITESPACE.iter().collect::<String>().repeat(3));
    for (index, blank) in blanks.iter().enumerate() {
        let refused = insert_goal_row(&h, Some(blank), &format!("blank-{index}"))
            .await
            .expect_err("a blank text must be refused");
        assert!(refused_by_the_text_check(&refused), "{blank:?}: {refused}");
    }

    // One character that is not ASCII whitespace, anywhere, is enough.
    for (index, whitespace) in ASCII_WHITESPACE.into_iter().enumerate() {
        let padded = format!("{whitespace}x{whitespace}");
        insert_goal_row(&h, Some(&padded), &format!("padded-{index}")).await?;
    }
    for (index, text) in ["\u{a0}", "\u{2003}", "a text"].into_iter().enumerate() {
        insert_goal_row(&h, Some(text), &format!("admitted-{index}")).await?;
    }
    insert_goal_row(&h, None, "unknown").await?;

    // The low-level writer cannot get a blank past it either.
    let pool = h.pg.pool_for_tests();
    for (index, blank) in ["", "\t"].into_iter().enumerate() {
        let mut tx = pool.begin().await?;
        let refused = write_goal(
            &mut tx,
            &h.owner,
            &admitted_text_command(&h, blank, &format!("low-level-blank-{index}")),
        )
        .await
        .expect_err("a blank text must be refused");
        assert!(refused_by_the_text_check(&refused), "{blank:?}: {refused}");
    }

    // The Goal write verbs store the text they are given, so a hand-built
    // draft with a blank one is refused with a typed error and not turned into
    // an unknown text.
    let before = goal_count(&h.pg).await?;
    let parent = h.create("blank-parent", "parent", "a parent").await?;
    let refusals = [
        h.create("blank-create", "title", " \t ").await,
        h.modify(parent.goal_id, "blank-modify", "title", "\n")
            .await,
        h.decompose(parent.goal_id, "blank-child", &[("child", "\u{b}")])
            .await
            .map(|_| parent),
    ];
    for refused in refusals {
        let error = refused.expect_err("a blank text must be refused by the database");
        assert!(
            matches!(
                error.downcast_ref::<StorageError>(),
                Some(StorageError::ConstraintViolation(message))
                    if message.contains("goal_text_nonblank_chk")
            ),
            "{error}"
        );
    }
    assert_eq!(
        goal_count(&h.pg).await?,
        before + 1,
        "only the parent was written"
    );
    Ok(())
});

async fn goal_count(pg: &PgStorage) -> TestResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.goal")
            .fetch_one(pg.pool_for_tests())
            .await?,
    )
}

/// A first Goal version for the low-level writer, with `text` stored as is.
fn admitted_text_command(h: &Harness, text: &str, request_id: &str) -> GoalWriteCommand {
    version_command(h, None, GoalState::Active, Some(text), None, request_id)
}

/// One Goal version for the low-level writer: the first of a series when
/// `series` is `None`, else its next version. `text` is stored as given.
fn version_command(
    h: &Harness,
    series: Option<Uuid>,
    state: GoalState,
    text: Option<&str>,
    close_fact_t: Option<Uuid>,
    request_id: &str,
) -> GoalWriteCommand {
    GoalWriteCommand {
        handle: series,
        schema_id: SimpleTextGoalV1::SCHEMA_ID.into(),
        title: "low level".into(),
        text: text.map(str::to_owned),
        state,
        request_id: request_id.into(),
        close_fact_t,
        assignment_t: Some(h.assignment.into_inner()),
        dependency_t: vec![],
        evidence_t: vec![],
        wake_id: None,
        mint_write_act: false,
        write_act_t: None,
    }
}

/// The first version of a new series, through the public low-level writer,
/// committed. Returns the series handle.
async fn start_series(h: &Harness, text: Option<&str>, request_id: &str) -> TestResult<Uuid> {
    let mut tx = h.pg.pool_for_tests().begin().await?;
    let written = write_goal(
        &mut tx,
        &h.owner,
        &version_command(h, None, GoalState::Active, text, None, request_id),
    )
    .await?;
    tx.commit().await?;
    Ok(written.handle)
}

/// The next version of `series` through the public low-level writer. The
/// inner result is the write itself: a refused version is rolled back.
async fn append_version(
    h: &Harness,
    series: Uuid,
    state: GoalState,
    text: Option<&str>,
    request_id: &str,
) -> TestResult<Result<RowWritten, StorageError>> {
    let close_fact_t = match state {
        GoalState::Achieved | GoalState::Abandoned => Some(h.evidence().await?.into_inner()),
        _ => None,
    };
    let mut tx = h.pg.pool_for_tests().begin().await?;
    let written = write_goal(
        &mut tx,
        &h.owner,
        &version_command(h, Some(series), state, text, close_fact_t, request_id),
    )
    .await;
    if written.is_ok() {
        tx.commit().await?;
    }
    Ok(written)
}

/// The refusal the carry rule gives: a typed constraint violation that names
/// the rule.
fn carry_refused(error: &StorageError) -> bool {
    matches!(error, StorageError::ConstraintViolation(message)
        if message.contains("goal_text_carried"))
}

goal_test!(a_state_change_must_carry_the_predecessors_text, |h| async {
    // Pause, abandon and achieve each move a Goal that has a text. Every text
    // but the predecessor's own is refused, whatever it looks like.
    for (index, next) in [GoalState::Paused, GoalState::Abandoned, GoalState::Achieved]
        .into_iter()
        .enumerate()
    {
        let series = start_series(&h, Some("the prompt"), &format!("carry-{index}")).await?;
        let before = goal_count(&h.pg).await?;
        let others = [
            None,
            Some("another prompt"),
            Some("The prompt"),
            Some("the prompt "),
            Some("\u{a0}"),
        ];
        for (other, text) in others.into_iter().enumerate() {
            let refused = append_version(
                &h,
                series,
                next,
                text,
                &format!("carry-{index}-refused-{other}"),
            )
            .await?
            .expect_err("a state change must carry the predecessor's text");
            assert!(carry_refused(&refused), "{next:?} {text:?}: {refused}");
        }
        assert_eq!(goal_count(&h.pg).await?, before, "a refusal writes nothing");

        let carried = append_version(
            &h,
            series,
            next,
            Some("the prompt"),
            &format!("carry-{index}-same"),
        )
        .await??;
        assert_eq!(
            stored_text(&h.pg, GoalId::new(carried.t)).await?.as_deref(),
            Some("the prompt")
        );
    }

    // Resuming is a state change too.
    let series = start_series(&h, Some("the prompt"), "carry-resume").await?;
    append_version(
        &h,
        series,
        GoalState::Paused,
        Some("the prompt"),
        "carry-resume-pause",
    )
    .await??;
    for (other, text) in [None, Some("another prompt")].into_iter().enumerate() {
        let refused = append_version(
            &h,
            series,
            GoalState::Active,
            text,
            &format!("carry-resume-refused-{other}"),
        )
        .await?
        .expect_err("resuming must carry the text");
        assert!(carry_refused(&refused), "{text:?}: {refused}");
    }
    append_version(
        &h,
        series,
        GoalState::Active,
        Some("the prompt"),
        "carry-resume-same",
    )
    .await??;
    Ok(())
});

goal_test!(a_version_joins_its_series_only_at_the_end, |h| async {
    // `Active "A"`, then `Paused "A"`. An `Active "B"` slipped between them
    // passes the carry rule against the first version and leaves the paused
    // one carrying a text its predecessor no longer has; one slipped before
    // the first has no predecessor at all. Both are refused.
    let series = start_series(&h, Some("A"), "order").await?;
    let paused = append_version(&h, series, GoalState::Paused, Some("A"), "order-pause").await??;
    let first: Uuid =
        sqlx::query_scalar("SELECT t FROM proxima_core.goal WHERE handle = $1 ORDER BY t LIMIT 1")
            .bind(series)
            .fetch_one(h.pg.pool_for_tests())
            .await?;
    let between = Uuid::from_u128(first.as_u128() + 1);
    let before_first = Uuid::from_u128(first.as_u128() - 1);
    assert!(
        first < between && between < paused.t,
        "the fixture slips between the two versions"
    );
    let count = goal_count(&h.pg).await?;
    for (index, t) in [between, before_first].into_iter().enumerate() {
        let refused = sqlx::query(
            "INSERT INTO proxima_core.goal (handle, t, owner_id, title, text, state, request_id)
             VALUES ($1, $2, $3, 'slipped', 'B', 'Active', $4)",
        )
        .bind(series)
        .bind(t)
        .bind(h.owner.stored_owner_id())
        .bind(format!("order-slipped-{index}"))
        .execute(h.pg.pool_for_tests())
        .await
        .expect_err("a version joins its series after the last version");
        assert!(
            refused
                .to_string()
                .contains("a version joins its series after the last version"),
            "{t}: {refused}"
        );
    }
    // The last version's own `t` is a duplicate, and the primary key says so.
    let duplicate = sqlx::query(
        "INSERT INTO proxima_core.goal (handle, t, owner_id, title, text, state, request_id)
         VALUES ($1, $2, $3, 'slipped', 'A', 'Paused', 'order-duplicate')",
    )
    .bind(series)
    .bind(paused.t)
    .bind(h.owner.stored_owner_id())
    .execute(h.pg.pool_for_tests())
    .await
    .expect_err("a version's t is unique");
    assert_eq!(
        duplicate
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("23505"),
        "{duplicate}"
    );
    assert_eq!(goal_count(&h.pg).await?, count, "a refusal writes nothing");
    Ok(())
});

goal_test!(two_concurrent_inserts_on_one_series_take_turns, |h| async {
    // `Active "A"` is the last version. One transaction inserts `Active "B"`
    // and holds it open; a second inserts `Paused "A"`, which only the first
    // version would admit. The series lock makes the second wait, and once the
    // first commits it is judged against `Active "B"` and refused.
    let series = start_series(&h, Some("A"), "turns").await?;
    let pool = h.pg.pool_for_tests().clone();
    let owner_id = h.owner.stored_owner_id();
    let t_b = Uuid::now_v7();
    let t_paused = Uuid::now_v7();
    assert!(t_b < t_paused, "the fixture orders the two inserts");

    let mut first = pool.begin().await?;
    sqlx::query(
        "INSERT INTO proxima_core.goal (handle, t, owner_id, title, text, state, request_id)
         VALUES ($1, $2, $3, 'turns', 'B', 'Active', 'turns-b')",
    )
    .bind(series)
    .bind(t_b)
    .bind(owner_id)
    .execute(&mut *first)
    .await?;

    let second = tokio::spawn({
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO proxima_core.goal (handle, t, owner_id, title, text, state, request_id)
                 VALUES ($1, $2, $3, 'turns', 'A', 'Paused', 'turns-paused')",
            )
            .bind(series)
            .bind(t_paused)
            .bind(owner_id)
            .execute(&pool)
            .await
        }
    });
    let waiting = async {
        loop {
            if second.is_finished() {
                return Err("the second insert did not wait for the series lock");
            }
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM pg_stat_activity
                  WHERE datname = current_database() AND wait_event_type = 'Lock'",
            )
            .fetch_one(&pool)
            .await
            .map_err(|_| "pg_stat_activity is readable")?;
            if waiting > 0 {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .map_err(|_| "the second insert never waited for the series lock")??;

    first.commit().await?;
    let refused = second
        .await?
        .expect_err("the second insert is judged against the version committed first");
    assert!(
        refused
            .to_string()
            .contains("keeps the predecessor text exactly"),
        "{refused}"
    );
    let last: Option<String> = sqlx::query_scalar(
        "SELECT text FROM proxima_core.goal WHERE handle = $1 ORDER BY t DESC LIMIT 1",
    )
    .bind(series)
    .fetch_one(&pool)
    .await?;
    assert_eq!(last.as_deref(), Some("B"));
    Ok(())
});

goal_test!(a_stale_snapshot_cannot_judge_a_state_change, |h| async {
    // `Active "A"` is the last version. A REPEATABLE READ transaction takes its
    // snapshot; another inserts `Active "B"` (without advancing the head, as a
    // raw writer may) and commits. The first then inserts `Paused "A"`, which
    // its snapshot would admit: the head the other insert wrote refuses it with
    // a serialization failure instead.
    let series = start_series(&h, Some("A"), "snapshot").await?;
    let pool = h.pg.pool_for_tests().clone();
    let owner_id = h.owner.stored_owner_id();
    let insert = |text: &'static str, state: &'static str, request: &'static str| {
        sqlx::query(
            "INSERT INTO proxima_core.goal (handle, t, owner_id, title, text, state, request_id)
             VALUES ($1, $2, $3, 'snapshot', $4, $5::proxima_core.goal_state, $6)",
        )
        .bind(series)
        .bind(Uuid::now_v7())
        .bind(owner_id)
        .bind(text)
        .bind(state)
        .bind(request)
    };

    let mut stale = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *stale)
        .await?;
    let seen: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.goal WHERE handle = $1")
            .bind(series)
            .fetch_one(&mut *stale)
            .await?;
    assert_eq!(seen, 1, "the snapshot holds the first version only");

    insert("B", "Active", "snapshot-b").execute(&pool).await?;

    let refused = insert("A", "Paused", "snapshot-paused")
        .execute(&mut *stale)
        .await
        .expect_err("a stale snapshot must not judge the state change");
    assert_eq!(
        refused
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("40001"),
        "{refused}"
    );
    stale.rollback().await?;
    Ok(())
});

goal_test!(
    an_unknown_text_stays_unknown_across_a_state_change,
    |h| async {
        let series = start_series(&h, None, "unknown-0").await?;
        for (index, next) in [GoalState::Paused, GoalState::Active, GoalState::Abandoned]
            .into_iter()
            .enumerate()
        {
            let refused = append_version(
                &h,
                series,
                next,
                Some("a text out of nowhere"),
                &format!("unknown-refused-{index}"),
            )
            .await?
            .expect_err("a state change cannot make an unknown text known");
            assert!(carry_refused(&refused), "{next:?}: {refused}");

            let carried =
                append_version(&h, series, next, None, &format!("unknown-same-{index}")).await??;
            assert_eq!(stored_text(&h.pg, GoalId::new(carried.t)).await?, None);
        }
        Ok(())
    }
);

goal_test!(
    a_modify_may_change_the_text_and_the_next_state_change_carries_that_one,
    |h| async {
        let series = start_series(&h, Some("one"), "modify-0").await?;
        let modified =
            append_version(&h, series, GoalState::Active, Some("two"), "modify-1").await??;
        assert_eq!(
            stored_text(&h.pg, GoalId::new(modified.t))
                .await?
                .as_deref(),
            Some("two"),
            "Active to Active may change the text"
        );

        // The predecessor is the latest version, not the first.
        let refused = append_version(&h, series, GoalState::Paused, Some("one"), "modify-2")
            .await?
            .expect_err("the text to carry is the modify's");
        assert!(carry_refused(&refused), "{refused}");
        let paused =
            append_version(&h, series, GoalState::Paused, Some("two"), "modify-3").await??;
        assert_eq!(
            stored_text(&h.pg, GoalId::new(paused.t)).await?.as_deref(),
            Some("two")
        );
        Ok(())
    }
);

goal_test!(
    a_version_written_by_the_v0_0_26_statement_cannot_move_a_goal_with_a_text,
    |h| async {
        // v0.0.26's `insert_goal_revision` names no `text` column, so a
        // process still running that release writes every version with a NULL
        // text. This is its statement, column list and all.
        const V0_0_26_INSERT: &str = "INSERT INTO proxima_core.goal
            (handle, t, owner_id, title, state, request_id, close_fact_t,
             assignment_t, dependency_t, evidence_t, wake_id, write_act_t)
         VALUES ($1, uuidv7(), $2, $3, $4::proxima_core.goal_state, $5, NULL,
                 NULL, '{}', '{}', NULL, NULL)";
        let pool = h.pg.pool_for_tests();
        let created = h.create("old-0", "old process", "a prompt").await?;
        let handle: Uuid = sqlx::query_scalar("SELECT handle FROM proxima_core.goal WHERE t = $1")
            .bind(created.goal_id.into_inner())
            .fetch_one(pool)
            .await?;
        let write = |state: &'static str, request_id: &'static str| {
            sqlx::query(V0_0_26_INSERT)
                .bind(handle)
                .bind(h.owner.stored_owner_id())
                .bind("old process")
                .bind(state)
                .bind(request_id)
                .execute(pool)
        };

        // Pause, which is what a v0.0.26 pause of this Goal would write.
        let refused = write("Paused", "old-1")
            .await
            .expect_err("a NULL text cannot move a Goal that has a text");
        assert!(
            refused.to_string().contains("goal_text_carried"),
            "{refused}"
        );
        assert_eq!(
            stored_text(&h.pg, created.goal_id).await?.as_deref(),
            Some("a prompt")
        );

        // A modify keeps the state, so the database lets it through: the text
        // is lost, and every later version carries the NULL.
        write("Active", "old-2").await?;
        write("Paused", "old-3").await?;
        let texts: Vec<Option<String>> =
            sqlx::query_scalar("SELECT text FROM proxima_core.goal WHERE handle = $1 ORDER BY t")
                .bind(handle)
                .fetch_all(pool)
                .await?;
        assert_eq!(texts, vec![Some("a prompt".to_owned()), None, None]);
        Ok(())
    }
);

goal_test!(
    a_text_the_database_admits_is_carried_forward_byte_for_byte,
    |h| async {
        // Texts the database admits but the Rust spring would call blank or
        // trim: they are in the table, and moving the Goal on must not judge
        // them again.
        let texts = [
            "\u{a0}",
            "\u{2003}",
            " padded\t",
            "\u{a0}\t",
            "\n\u{3000}\n",
        ];
        let pool = h.pg.pool_for_tests();
        for (index, text) in texts.into_iter().enumerate() {
            for closing in [GoalState::Abandoned, GoalState::Achieved] {
                let request = format!("admitted-{index}-{closing:?}");
                let mut tx = pool.begin().await?;
                let written = write_goal(
                    &mut tx,
                    &h.owner,
                    &admitted_text_command(&h, text, &request),
                )
                .await?;
                tx.commit().await?;
                let first = GoalId::new(written.t);

                let paused = h
                    .transition(first, GoalState::Paused, &format!("{request}-pause"))
                    .await?;
                let resumed = h
                    .transition(
                        paused.goal_id,
                        GoalState::Active,
                        &format!("{request}-resume"),
                    )
                    .await?;
                let closed = if closing == GoalState::Achieved {
                    h.achieve(resumed.goal_id, &format!("{request}-close"))
                        .await?
                } else {
                    h.transition(resumed.goal_id, closing, &format!("{request}-close"))
                        .await?
                };
                for id in [first, paused.goal_id, resumed.goal_id, closed.goal_id] {
                    assert_eq!(
                        stored_text(&h.pg, id).await?.as_deref(),
                        Some(text),
                        "{text:?} must come through {closing:?} byte for byte"
                    );
                    assert_eq!(h.read(id).await?.text.as_deref(), Some(text));
                }
            }
        }
        Ok(())
    }
);

goal_test!(
    export_carries_the_text_and_erase_takes_it_with_the_row,
    |h| async {
        let surfaces = OwnerSurfaces::for_registry(&h.registry);
        let known = h
            .create("export-1", "exported", "an exportable prompt")
            .await?;
        let paused = h
            .transition(known.goal_id, GoalState::Paused, "export-2")
            .await?;
        let unknown = h.insert_declarationless("export-old", "Active").await?;

        let auth = ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner {
            user_id: h.user_id(),
        });
        let bundle = h.pg.export_owner_bundle(&auth, &surfaces).await?;
        let exported: BTreeMap<String, serde_json::Value> = bundle
            .table("proxima_core.goal")
            .iter()
            .map(|row| {
                (
                    row["t"].as_str().unwrap_or_default().to_owned(),
                    row["text"].clone(),
                )
            })
            .collect();
        assert_eq!(
            exported,
            BTreeMap::from([
                (
                    known.goal_id.into_inner().to_string(),
                    serde_json::json!("an exportable prompt")
                ),
                (
                    paused.goal_id.into_inner().to_string(),
                    serde_json::json!("an exportable prompt")
                ),
                (unknown.into_inner().to_string(), serde_json::Value::Null),
            ]),
            "the bundle carries each version's text, null where it is unknown"
        );

        let erase = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: h.user_id(),
            drop_event_id: "goal-text-erase".into(),
        });
        let outcome =
            h.pg.erase_personal_owner(&erase, h.user_id(), &surfaces)
                .await?;
        assert!(
            matches!(outcome, OwnerEraseOutcome::Completed { .. }),
            "{outcome:?}"
        );
        let left: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.goal
          WHERE owner_id = $1 OR text = 'an exportable prompt'",
        )
        .bind(h.owner.stored_owner_id())
        .fetch_one(h.pg.pool_for_tests())
        .await?;
        assert_eq!(left, 0, "the text is erased with its row");
        Ok(())
    }
);

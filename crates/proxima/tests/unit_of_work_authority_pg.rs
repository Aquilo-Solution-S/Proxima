//! Current-subject authority reads share an owner unit's transaction and fence.
#![cfg(feature = "testkit")]
#![allow(clippy::too_many_lines)]

#[path = "fixtures/host_state/mod.rs"]
#[allow(
    dead_code,
    reason = "this suite reuses only the host fixture's app and tables"
)]
mod host_state_fixture;
#[path = "fixtures/split_core_db.rs"]
mod split_core_db;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use host_state_fixture::{AUXILIARY_TABLE, HostFixtureApp, PARTICIPANT};
use proxima::host::{
    AgreedCommand, HostStateCommand, HostStateHandler, HostStatePayloadOwners,
    HostStateWritePermit, PgCommandDispatcher, StateSurfaceName,
};
use proxima::testkit::SplitRoleDb;
use proxima::{
    AuthPath, AuthzContext, BuiltProxima, ErrorCode, HostStateOutcome, PgHostStateSession, Proxima,
    ServedSurface,
};
use proxima_core::verbs::goal_write::{
    GoalAssignmentTarget, GoalAuthorship, GoalCreateRequest, GoalEvidenceRef, GoalTopologyWrite,
    IdempotencyKey,
};
use proxima_core::{
    AgentNoteV1, DerivedMemory, GroupId, InterpretationSubjectKind, InterpretationV1, MemoryId,
    MemoryTarget, OwnerId, Relation, Role, SeriesHandle, SimpleTextGoalV1, StorageError, UserId,
};
use proxima_pg_testkit::db_url;
use proxima_storage_pg::{PgPlatformScope, begin_owner_transaction};
use split_core_db::clone_split_core_db;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
const TABLES: &[StateSurfaceName] = &[StateSurfaceName::new(AUXILIARY_TABLE)];

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    subject: UserId,
    roles: BTreeMap<Uuid, Role>,
}

enum AuthorityAction {
    Read,
    RecordGoal {
        goal: Uuid,
        fail_after_insert: bool,
    },
    ProbeBoundaries {
        foreign_subject: UserId,
        group: GroupId,
        foreign_memory: MemoryId,
    },
}

struct AuthorityCommand {
    owner: OwnerId,
    action: AuthorityAction,
}

impl AuthorityCommand {
    fn read(subject: UserId) -> Self {
        Self {
            owner: subject.into(),
            action: AuthorityAction::Read,
        }
    }
}

impl HostStateCommand for AuthorityCommand {
    const PARTICIPANT_ID: proxima::HostStateParticipantId = PARTICIPANT;
    const TABLES: &'static [StateSurfaceName] = TABLES;
    type Outcome = Snapshot;

    fn owner(&self) -> OwnerId {
        self.owner
    }
}

impl HostStatePayloadOwners for AuthorityCommand {
    fn payload_owners(&self) -> Vec<OwnerId> {
        Vec::new()
    }
}

struct AuthorityHandler;

#[async_trait]
impl HostStateHandler<AuthorityCommand> for AuthorityHandler {
    async fn handle(
        &self,
        session: &mut PgHostStateSession<'_, '_>,
        permit: &HostStateWritePermit,
        command: AgreedCommand<AuthorityCommand>,
    ) -> Result<HostStateOutcome<Snapshot>, StorageError> {
        let memberships = session.authority_memberships().await?;
        let snapshot = Snapshot {
            subject: memberships.subject(),
            roles: memberships
                .memberships()
                .iter()
                .map(|row| (row.group.into_inner(), row.role))
                .collect(),
        };
        let tx = session.transaction();
        match command.into_inner().action {
            AuthorityAction::Read => {}
            AuthorityAction::RecordGoal {
                goal,
                fail_after_insert,
            } => {
                let visible: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM proxima_core.goal WHERE t = $1 AND owner_id = $2)",
                ).bind(goal).bind(permit.owner().into_inner())
                    .fetch_one(&mut **tx).await.map_err(proxima_storage_pg::map_err)?;
                assert!(visible, "the participant sees the unit's pending Goal");
                sqlx::query(
                    "INSERT INTO host_fixture.auxiliary (invocation_id, owner_id) VALUES ($1, $2)",
                )
                .bind(goal)
                .bind(permit.owner().into_inner())
                .execute(&mut **tx)
                .await
                .map_err(proxima_storage_pg::map_err)?;
                if fail_after_insert {
                    return Err(StorageError::Internal(
                        "injected failure after authority decision and host write".into(),
                    ));
                }
            }
            AuthorityAction::ProbeBoundaries {
                foreign_subject,
                group,
                foreign_memory,
            } => {
                let (foreign_rows, group_rows, managed, authority_subject): (
                    i64,
                    i64,
                    String,
                    String,
                ) = sqlx::query_as(
                    "SELECT (SELECT count(*) FROM proxima_core.memory WHERE t = $1),
                            (SELECT count(*) FROM proxima_core.group_memberships),
                            current_setting('app.manage_owner'),
                            COALESCE(current_setting('app.authority_subject', true), '')",
                )
                .bind(foreign_memory.into_inner())
                .fetch_one(&mut **tx)
                .await
                .map_err(proxima_storage_pg::map_err)?;
                assert_eq!(foreign_rows, 0, "authority reads grant no memory reach");
                assert_eq!(group_rows, 0, "raw membership rows remain owner-scoped");
                assert_eq!(managed, "{}", "a personal writer manages no groups");
                assert!(
                    authority_subject.is_empty(),
                    "temporary projection scope is restored"
                );
                refused_sql(
                    tx,
                    sqlx::query("SELECT * FROM proxima_core.read_subject_authority($1)"),
                    foreign_subject.into_inner(),
                    "42501",
                )
                .await?;
                refused_sql(tx,
                    sqlx::query("INSERT INTO proxima_core.group_memberships (group_id, member_user_id, relation)
                     VALUES ($1, current_setting('app.subject')::uuid, 'admin')"),
                    group.into_inner(), "42501").await?;

                sqlx::query("SAVEPOINT unrelated_scope")
                    .execute(&mut **tx)
                    .await
                    .map_err(proxima_storage_pg::map_err)?;
                sqlx::query("SELECT set_config('app.proxima_scope', 'platform', true)")
                    .execute(&mut **tx)
                    .await
                    .map_err(proxima_storage_pg::map_err)?;
                refused_sql(
                    tx,
                    sqlx::query("SELECT * FROM proxima_core.read_subject_authority($1)"),
                    snapshot.subject.into_inner(),
                    "42501",
                )
                .await?;
                sqlx::query("ROLLBACK TO SAVEPOINT unrelated_scope")
                    .execute(&mut **tx)
                    .await
                    .map_err(proxima_storage_pg::map_err)?;

                // Even an explicitly requested projection scope cannot expose another subject.
                sqlx::query("SELECT set_config('app.authority_subject', $1::uuid::text, true)")
                    .bind(foreign_subject.into_inner())
                    .execute(&mut **tx)
                    .await
                    .map_err(proxima_storage_pg::map_err)?;
                let others: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM proxima_core.group_role_projection WHERE member_user_id = $1",
                ).bind(foreign_subject.into_inner()).fetch_one(&mut **tx).await
                    .map_err(proxima_storage_pg::map_err)?;
                assert_eq!(others, 0);
                sqlx::query("SELECT set_config('app.authority_subject', '', true)")
                    .execute(&mut **tx)
                    .await
                    .map_err(proxima_storage_pg::map_err)?;
            }
        }
        Ok(HostStateOutcome::Permitted(snapshot))
    }
}

async fn refused_sql(
    tx: &mut Transaction<'_, Postgres>,
    statement: sqlx::query::Query<'static, Postgres, sqlx::postgres::PgArguments>,
    argument: Uuid,
    expected_code: &str,
) -> Result<(), StorageError> {
    sqlx::query("SAVEPOINT authority_refusal")
        .execute(&mut **tx)
        .await
        .map_err(proxima_storage_pg::map_err)?;
    let error = statement
        .bind(argument)
        .execute(&mut **tx)
        .await
        .expect_err("the database boundary refuses this operation");
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some(expected_code),
        "{error}"
    );
    sqlx::query("ROLLBACK TO SAVEPOINT authority_refusal")
        .execute(&mut **tx)
        .await
        .map_err(proxima_storage_pg::map_err)?;
    Ok(())
}

struct Harness {
    db: SplitRoleDb,
    built: BuiltProxima,
    admin: PgPool,
    platform: PgPlatformScope,
    subject: UserId,
    authz: AuthzContext,
}

impl Harness {
    async fn new() -> TestResult<Self> {
        let db = clone_split_core_db("proxima_uow_authority").await?;
        let subject = UserId::new(Uuid::now_v7());
        let built = Proxima::<HostFixtureApp>::app()
            .database_url(db.runtime_url())
            .platform_database_url(db.platform_url())
            .store_number(0)
            .owner(subject.into())
            .allow_insecure_single_owner()
            .served_surface(ServedSurface::All)
            .host_state_participant(Arc::new(
                PgCommandDispatcher::new(PARTICIPANT, TABLES)
                    .register::<AuthorityCommand, _>(AuthorityHandler)?,
            ))
            .build()
            .await?;
        let runtime = PgPool::connect(db.runtime_url()).await?;
        let flags: (bool, bool) = sqlx::query_as(
            "SELECT rolsuper, rolbypassrls FROM pg_roles WHERE rolname = current_user",
        )
        .fetch_one(&runtime)
        .await?;
        assert_eq!(flags, (false, false), "the host runtime must obey RLS");
        runtime.close().await;
        let admin = PgPool::connect(&db_url(db.name())).await?;
        let platform = PgPlatformScope::new(
            PgPool::connect(db.platform_url()).await?,
            &["proxima_core", "host_fixture"],
        )
        .await?;
        let authz = personal_authz(subject);
        assert!(
            !authz
                .role_for_owner(&subject.into())
                .expect("personal role")
                .manages()
        );
        Ok(Self {
            db,
            built,
            admin,
            platform,
            subject,
            authz,
        })
    }

    async fn group(&self) -> TestResult<GroupId> {
        let group = GroupId::new(Uuid::now_v7());
        let mut tx = self.platform.begin().await?;
        proxima::register_group(&mut tx, group).await?;
        tx.commit().await?;
        Ok(group)
    }

    async fn add(&self, group: GroupId, member: OwnerId, relation: Relation) -> TestResult {
        let mut tx = self.platform.begin().await?;
        proxima::add_group_member(&mut tx, group, member, relation).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn snapshot(&self) -> TestResult<Snapshot> {
        let mut unit = self.built.host().engine().unit_of_work(&self.authz).await?;
        let snapshot = permitted(
            unit.apply_host_state(AuthorityCommand::read(self.subject))
                .await?,
        );
        unit.commit().await?;
        Ok(snapshot)
    }

    async fn close(self) {
        self.built.shutdown().await;
        self.admin.close().await;
        drop(self.db);
    }
}

fn personal_authz(subject: UserId) -> AuthzContext {
    proxima_core::test_fixtures::authenticated_context(
        AuthzContext::for_subject(subject, AuthPath::HostBearer)
            .narrowed_to_owner(subject.into())
            .expect("personal owner"),
    )
}

fn permitted(result: HostStateOutcome<Snapshot>) -> Snapshot {
    match result {
        HostStateOutcome::Permitted(snapshot) => snapshot,
        HostStateOutcome::AlreadyApplied(snapshot) => {
            panic!("authority read unexpectedly reported a replay: {snapshot:?}")
        }
        HostStateOutcome::Refused(snapshot) => {
            panic!("authority read was refused: {snapshot:?}")
        }
    }
}

fn note(title: &str) -> AgentNoteV1 {
    AgentNoteV1 {
        note_id: Uuid::now_v7(),
        title: title.into(),
        body: title.into(),
        tags: Vec::new(),
        idempotency_key: Some(title.into()),
    }
}

fn goal_request(
    owner: OwnerId,
    assignment: MemoryId,
    evidence: MemoryId,
    request: &str,
) -> GoalCreateRequest<SimpleTextGoalV1> {
    GoalCreateRequest {
        owner,
        topology: GoalTopologyWrite::new(
            Some(GoalAssignmentTarget::perspective(assignment)),
            Vec::new(),
            vec![GoalEvidenceRef::new(evidence)],
        )
        .expect("valid fixture topology"),
        title: "authority decision".into(),
        text: "authority decision".into(),
        payload: SimpleTextGoalV1 {},
        request_id: IdempotencyKey::new(request).expect("fixture key"),
        authorship: GoalAuthorship::User,
        author_self_perspective_id: None,
    }
}

async fn wait_for_fence(pool: &PgPool, label: &str, pid: Option<i32>) -> TestResult {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "WITH k AS (SELECT hashtextextended($1, 0) AS key)
                 SELECT EXISTS (SELECT 1 FROM pg_locks l
                     JOIN pg_stat_activity a USING (pid) CROSS JOIN k
                    WHERE l.locktype = 'advisory' AND NOT l.granted
                      AND a.wait_event_type = 'Lock' AND ($2::integer IS NULL OR l.pid = $2)
                      AND l.classid::bigint = ((k.key >> 32) & 4294967295)
                      AND l.objid::bigint = (k.key & 4294967295))",
            )
            .bind(label)
            .bind(pid)
            .fetch_one(pool)
            .await?;
            if waiting {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn personal_writer_reads_complete_direct_and_inherited_authority_without_widening_owner_scope()
-> TestResult {
    let h = Harness::new().await?;
    let child = h.group().await?;
    let parent = h.group().await?;
    let direct = h.group().await?;
    let foreign_group = h.group().await?;
    let foreign = UserId::new(Uuid::now_v7());
    h.add(child, h.subject.into(), Relation::Editor).await?;
    h.add(parent, child.into(), Relation::Viewer).await?;
    h.add(direct, h.subject.into(), Relation::Ingest).await?;
    h.add(foreign_group, foreign.into(), Relation::Admin)
        .await?;
    let engine = h.built.host().engine();
    let group_fact = engine
        .ingest_fact(
            &proxima::testkit::scoped_authz(child.into()),
            proxima::FactWrite::new(child.into(), "group-authority", &note("group")),
        )
        .await?;
    let mut unit = engine.unit_of_work(&h.authz).await?;
    let snapshot = permitted(
        unit.apply_host_state(AuthorityCommand {
            owner: h.subject.into(),
            action: AuthorityAction::ProbeBoundaries {
                foreign_subject: foreign,
                group: child,
                foreign_memory: group_fact.memory_id,
            },
        })
        .await?,
    );
    assert_eq!(
        snapshot,
        Snapshot {
            subject: h.subject,
            roles: BTreeMap::from([
                (child.into_inner(), Role::editor()),
                (parent.into_inner(), Role::viewer()),
                (direct.into_inner(), Role::ingest()),
            ])
        }
    );
    let denied = unit
        .ingest_fact(proxima::FactWrite::new(
            child.into(),
            "outside-personal-owner",
            &note("outside"),
        ))
        .await
        .expect_err("membership answers do not authorize another memory owner");
    assert_eq!(denied.code, ErrorCode::Forbidden);
    drop(unit);
    h.close().await;
    Ok(())
}

#[tokio::test]
async fn goal_and_host_authority_decision_commit_and_roll_back_together() -> TestResult {
    let h = Harness::new().await?;
    let group = h.group().await?;
    h.add(group, h.subject.into(), Relation::Viewer).await?;
    let owner = h.subject.into();
    let engine = h.built.host().engine();
    let fact = engine
        .ingest_fact(
            &h.authz,
            proxima::FactWrite::new(owner, "authority-evidence", &note("evidence")),
        )
        .await?;
    let assignment = engine
        .derive_memory(
            &h.authz,
            DerivedMemory::interpretation(
                MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                owner,
                "authority assignment",
                InterpretationV1 {
                    claim: "assignment".into(),
                    confidence: 90,
                    subject_memory_ids: vec![fact.memory_id.into_inner()],
                    subject_kinds: vec![InterpretationSubjectKind::Fact],
                    model_id: "test".into(),
                    client_name: "test".into(),
                    client_version: "1".into(),
                },
            ),
        )
        .await?;
    for (request, fail_after_insert) in [("authority-commit", false), ("authority-rollback", true)]
    {
        let mut unit = engine.unit_of_work(&h.authz).await?;
        let goal = unit
            .create_goal(goal_request(
                owner,
                assignment.memory_id,
                fact.memory_id,
                request,
            ))
            .await?;
        let outcome = unit
            .apply_host_state(AuthorityCommand {
                owner,
                action: AuthorityAction::RecordGoal {
                    goal: goal.goal_id.into_inner(),
                    fail_after_insert,
                },
            })
            .await;
        let commit = unit.commit().await;
        if fail_after_insert {
            assert!(outcome.is_err(), "injected failure reaches the caller");
            assert!(commit.is_err(), "participant failure poisons the unit");
        } else {
            let snapshot = permitted(outcome?);
            assert_eq!(
                snapshot.roles,
                BTreeMap::from([(group.into_inner(), Role::viewer())])
            );
            commit?;
        }
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM proxima_core.goal WHERE t = $1),
                    (SELECT count(*) FROM host_fixture.auxiliary WHERE invocation_id = $1)",
        )
        .bind(goal.goal_id.into_inner())
        .fetch_one(&h.admin)
        .await?;
        let expected = i64::from(!fail_after_insert);
        assert_eq!(counts, (expected, expected));
    }
    h.close().await;
    Ok(())
}

#[tokio::test]
async fn reader_fences_direct_removal_and_first_membership_until_commit() -> TestResult {
    let h = Harness::new().await?;
    let unrelated_group = h.group().await?;
    for existing in [false, true] {
        let group = h.group().await?;
        if existing {
            h.add(group, h.subject.into(), Relation::Viewer).await?;
        }
        let mut reader = h.built.host().engine().unit_of_work(&h.authz).await?;
        let before = permitted(
            reader
                .apply_host_state(AuthorityCommand::read(h.subject))
                .await?,
        );
        if !existing {
            assert!(
                before.roles.is_empty(),
                "the first membership has no projection row to lock"
            );
        }
        assert_eq!(before.roles.contains_key(&group.into_inner()), existing);
        let mut write = h.platform.begin().await?;
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *write)
            .await?;
        let subject = h.subject;
        let writer = tokio::spawn(async move {
            if existing {
                proxima::remove_group_member(&mut write, group, subject.into()).await?;
            } else {
                proxima::add_group_member(&mut write, group, subject.into(), Relation::Editor)
                    .await?;
            }
            write.commit().await.map_err(proxima_storage_pg::map_err)
        });
        wait_for_fence(
            &h.admin,
            &format!("proxima-owner-kind:{}", h.subject.into_inner()),
            Some(pid),
        )
        .await?;
        let unrelated = UserId::new(Uuid::now_v7());
        tokio::time::timeout(
            Duration::from_secs(5),
            h.add(unrelated_group, unrelated.into(), Relation::Viewer),
        )
        .await??;
        assert!(
            !writer.is_finished(),
            "membership waits for the command's transaction"
        );
        reader.commit().await?;
        tokio::time::timeout(Duration::from_secs(5), writer).await???;
        let after = h.snapshot().await?;
        assert_eq!(
            after.roles.get(&group.into_inner()).copied(),
            if existing { None } else { Some(Role::editor()) }
        );
    }
    h.close().await;
    Ok(())
}

#[tokio::test]
async fn reader_fences_inherited_membership_changes_until_rollback() -> TestResult {
    let h = Harness::new().await?;
    let parent = h.group().await?;
    let child = h.group().await?;
    h.add(child, h.subject.into(), Relation::Editor).await?;
    let mut reader = h.built.host().engine().unit_of_work(&h.authz).await?;
    let before = permitted(
        reader
            .apply_host_state(AuthorityCommand::read(h.subject))
            .await?,
    );
    assert!(!before.roles.contains_key(&parent.into_inner()));
    let mut write = h.platform.begin().await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *write)
        .await?;
    let writer = tokio::spawn(async move {
        proxima::add_group_member(&mut write, parent, child.into(), Relation::Viewer).await?;
        write.commit().await.map_err(proxima_storage_pg::map_err)
    });
    wait_for_fence(
        &h.admin,
        &format!("proxima-group-projection:{}", h.subject.into_inner()),
        Some(pid),
    )
    .await?;
    assert!(!writer.is_finished());
    drop(reader);
    tokio::time::timeout(Duration::from_secs(5), writer).await???;
    assert_eq!(
        h.snapshot().await?.roles.get(&parent.into_inner()),
        Some(&Role::viewer())
    );
    h.close().await;
    Ok(())
}

#[tokio::test]
async fn writer_first_role_replacement_is_visible_after_the_authority_read_waits() -> TestResult {
    let h = Harness::new().await?;
    let group = h.group().await?;
    h.add(group, h.subject.into(), Relation::Viewer).await?;
    let mut write = h.platform.begin().await?;
    proxima::replace_group_member_relation(
        &mut write,
        group,
        h.subject.into(),
        Relation::Viewer,
        Relation::Editor,
    )
    .await?;
    let engine = h.built.host().engine().clone();
    let authz = h.authz.clone();
    let subject = h.subject;
    let reader = tokio::spawn(async move {
        let mut unit = engine.unit_of_work(&authz).await?;
        let result = unit
            .apply_host_state(AuthorityCommand::read(subject))
            .await?;
        unit.commit().await?;
        Ok::<_, proxima::ProtocolError>(permitted(result))
    });
    wait_for_fence(
        &h.admin,
        &format!("proxima-owner-kind:{}", h.subject.into_inner()),
        None,
    )
    .await?;
    write.commit().await?;
    let after = tokio::time::timeout(Duration::from_secs(5), reader).await???;
    assert_eq!(
        after.roles,
        BTreeMap::from([(group.into_inner(), Role::editor())])
    );
    h.close().await;
    Ok(())
}

#[tokio::test]
async fn maintenance_and_fixed_snapshot_transactions_do_not_gain_subject_authority() -> TestResult {
    let h = Harness::new().await?;
    let authority = h
        .built
        .host_state_maintenance_authority()
        .expect("registered participant");
    let mut maintenance = h
        .built
        .host()
        .engine()
        .host_state_unit_of_work(authority, h.subject.into())?;
    let error = maintenance
        .apply_host_state(AuthorityCommand::read(h.subject))
        .await
        .expect_err("maintenance has no authenticated subject scope");
    assert!(
        error.message.contains("authenticated owner scope"),
        "{error}"
    );
    assert!(maintenance.commit().await.is_err());

    let runtime = PgPool::connect(h.db.runtime_url()).await?;
    for isolation in [
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ"),
        sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE"),
    ] {
        let mut tx = runtime.begin().await?;
        isolation.execute(&mut *tx).await?;
        // Binding the real scope runs SELECTs, so select isolation first.
        sqlx::query("SELECT set_config('app.proxima_scope', 'owner', true), set_config('app.subject', $1::uuid::text, true)")
            .bind(h.subject.into_inner()).execute(&mut *tx).await?;
        refused_sql(
            &mut tx,
            sqlx::query("SELECT * FROM proxima_core.read_subject_authority($1)"),
            h.subject.into_inner(),
            "25001",
        )
        .await?;
        tx.rollback().await?;
    }
    let group_subject = h.group().await?;
    let group_authz = personal_authz(UserId::new(group_subject.into_inner()));
    let mut tx = begin_owner_transaction(
        &runtime,
        group_authz.owner_scope().expect("sealed fixture context"),
    )
    .await?;
    refused_sql(
        &mut tx,
        sqlx::query("SELECT * FROM proxima_core.read_subject_authority($1)"),
        group_subject.into_inner(),
        "42501",
    )
    .await?;
    tx.rollback().await?;
    runtime.close().await;
    h.close().await;
    Ok(())
}

#[tokio::test]
async fn existing_memberships_survive_definition_upgrade_and_reapplication() -> TestResult {
    let h = Harness::new().await?;
    let group = h.group().await?;
    h.add(group, h.subject.into(), Relation::Editor).await?;
    // Restore the pre-port policy and remove the new helper on an existing,
    // populated database. The released migration ledger stays untouched.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        "DROP FUNCTION proxima_core.read_subject_authority(uuid);
         ALTER POLICY proxima_owner_read ON proxima_core.group_role_projection
           USING (group_id = ANY(COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[])));
         ALTER POLICY proxima_owner_read ON proxima_core.owners
           USING (owner_id = ANY(COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[])));
         CREATE OR REPLACE FUNCTION proxima_core.definitions_applied()
           RETURNS TABLE(checksum text, release text) LANGUAGE sql IMMUTABLE
           AS $$ SELECT 'previous-definitions'::text, '0.0.0'::text $$;",
    )).execute(&h.admin).await?;
    let ledger: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version, checksum FROM public._sqlx_migrations ORDER BY version")
            .fetch_all(&h.admin)
            .await?;
    let runtime = PgPool::connect(h.db.runtime_url()).await?;
    let mut before = begin_owner_transaction(
        &runtime,
        h.authz.owner_scope().expect("sealed personal scope"),
    )
    .await?;
    let hidden: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.group_role_projection")
        .fetch_one(&mut *before)
        .await?;
    assert_eq!(
        hidden, 0,
        "the previous policy does not expose personal authority"
    );
    before.rollback().await?;
    for pass in 0..2 {
        if pass > 0 {
            // Force the definitions pass again after the successful upgrade.
            sqlx::query(
                "CREATE OR REPLACE FUNCTION proxima_core.definitions_applied()
                 RETURNS TABLE(checksum text, release text) LANGUAGE sql IMMUTABLE
                 AS $$ SELECT 'reapply'::text, '0.0.0'::text $$",
            )
            .execute(&h.admin)
            .await?;
        }
        let migration = proxima_storage_pg::PgMigrationStorage::connect_with_config(
            h.db.platform_url(),
            proxima_storage_pg::PgPoolConfig::default(),
            proxima_storage_pg::PgTuning::default(),
        )
        .await?;
        migration.run_migrations().await?;
        migration.clone_pool_for_backend().close().await;
        assert_eq!(
            h.snapshot().await?,
            Snapshot {
                subject: h.subject,
                roles: BTreeMap::from([(group.into_inner(), Role::editor())])
            }
        );
        let current_ledger: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT version, checksum FROM public._sqlx_migrations ORDER BY version",
        )
        .fetch_all(&h.admin)
        .await?;
        assert_eq!(
            current_ledger, ledger,
            "the upgrade changes definitions only"
        );
    }
    runtime.close().await;
    h.close().await;
    Ok(())
}

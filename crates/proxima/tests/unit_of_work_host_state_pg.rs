//! Real-Postgres host-state participation on the existing [`proxima::UnitOfWork`].
#![allow(clippy::too_many_lines)]

#[path = "fixtures/host_state/mod.rs"]
mod host_state_fixture;
#[path = "fixtures/split_core_db.rs"]
mod split_core_db;

use host_state_fixture::{
    AuxiliaryHostCommand, CoreStateSurfaceCommand, DuplicateDescriptorParticipant,
    DuplicateTablesCommand, EmptyDescriptorParticipant, EmptyTablesCommand, FixtureHostCommand,
    FixtureHostResult, HostFixtureApp, HostFixtureParticipant, InvalidBindingCommand,
    UndeclaredDescriptorParticipant, UnknownParticipantCommand, invocation_lock_key,
};
use proxima::{
    AuthPath, AuthzContext, ErrorCode, HostStateOutcome, PgHostStateParticipant, Proxima, Role,
    ToolScope, company_owner,
};
use proxima_core::{AgentNoteV1, GroupId, Owner, UserId};
use proxima_pg_testkit::{db_url, drop_db, split_role_urls, unique_db_name};
use split_core_db::create_split_core_db;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

fn note(title: &str) -> AgentNoteV1 {
    AgentNoteV1 {
        note_id: Uuid::now_v7(),
        title: title.into(),
        body: title.into(),
        tags: Vec::new(),
        idempotency_key: Some(title.into()),
    }
}

fn admin_authz_for(owner: Owner) -> AuthzContext {
    let context = if matches!(owner, Owner::Personal(_)) {
        AuthzContext::single_owner(&owner, AuthPath::HostBearer)
    } else {
        AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(owner, Role::admin())],
            AuthPath::HostBearer,
        )
        .narrowed_to_owner(owner)
        .expect("an admin on exactly this owner narrows to it")
    };
    proxima_core::test_fixtures::authenticated_context(context)
}

async fn boot_fixture(
    db_url: &str,
    platform_url: &str,
    owner: Owner,
    participant: Option<Arc<dyn PgHostStateParticipant>>,
) -> Result<proxima::BuiltProxima, Box<dyn std::error::Error>> {
    let mut app = Proxima::<HostFixtureApp>::app()
        .database_url(db_url)
        .platform_database_url(platform_url)
        .owner(owner)
        .allow_insecure_single_owner()
        .tool_scope(ToolScope::All);
    if let Some(participant) = participant {
        app = app.host_state_participant(participant);
    }
    Ok(app.build().await?)
}

async fn count_memory(
    pool: &PgPool,
    memory_id: proxima_core::MemoryId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
        .bind(memory_id.into_inner())
        .fetch_one(pool)
        .await
}

async fn admin_pool(database: &str) -> Result<PgPool, sqlx::Error> {
    PgPool::connect(&db_url(database)).await
}

async fn count_execution(
    pool: &PgPool,
    memory_id: proxima_core::MemoryId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint FROM host_fixture.execution WHERE invocation_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_one(pool)
    .await
}

async fn execution_status(
    pool: &PgPool,
    memory_id: proxima_core::MemoryId,
) -> Result<Option<(String, i32)>, sqlx::Error> {
    sqlx::query_as::<_, (String, i32)>(
        "SELECT status, version FROM host_fixture.execution WHERE invocation_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_optional(pool)
    .await
}

async fn execution_provenance(
    pool: &PgPool,
    memory_id: proxima_core::MemoryId,
) -> Result<
    Option<(
        String,
        Uuid,
        Option<String>,
        Option<Uuid>,
        bool,
        Option<String>,
        Option<Uuid>,
    )>,
    sqlx::Error,
> {
    sqlx::query_as::<
        _,
        (
            String,
            Uuid,
            Option<String>,
            Option<Uuid>,
            bool,
            Option<String>,
            Option<Uuid>,
        ),
    >(
        "SELECT owner_kind::text, owner_id, principal_kind::text, principal_id,
                maintenance_origin, payload_saved_by_kind::text, payload_saved_by_id
         FROM host_fixture.execution WHERE invocation_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_optional(pool)
    .await
}

fn owner_fence_label(owner: Owner) -> String {
    let kind = match owner {
        Owner::Personal(_) => "personal",
        Owner::Group(_) => "group",
    };
    format!("proxima-owner-fence:{kind}:{}", owner.stored_owner_id())
}

async fn wait_for_owner_fence_wait(
    pool: &PgPool,
    owner: Owner,
) -> Result<(), Box<dyn std::error::Error>> {
    let label = owner_fence_label(owner);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "WITH lock_key AS (SELECT hashtextextended($1, 0) AS key)
                 SELECT EXISTS (
                     SELECT 1
                       FROM pg_locks l
                       CROSS JOIN lock_key k
                      WHERE l.locktype = 'advisory'
                        AND NOT l.granted
                        AND l.classid::bigint = ((k.key >> 32) & 4294967295)
                        AND l.objid::bigint = (k.key & 4294967295)
                 )",
            )
            .bind(label.as_str())
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

async fn advisory_wait_reports_lock_event(pool: &PgPool, label: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "WITH lock_key AS (SELECT hashtextextended($1, 0) AS key)
         SELECT EXISTS (
             SELECT 1
               FROM pg_locks l
               JOIN pg_stat_activity a USING (pid)
               CROSS JOIN lock_key k
              WHERE l.locktype = 'advisory' AND NOT l.granted
                AND a.wait_event_type = 'Lock'
                AND l.classid::bigint = ((k.key >> 32) & 4294967295)
                AND l.objid::bigint = (k.key & 4294967295)
         )",
    )
    .bind(label)
    .fetch_one(pool)
    .await
}

async fn advisory_integer_wait_reports_lock_event(
    pool: &PgPool,
    key: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_locks l
               JOIN pg_stat_activity a USING (pid)
              WHERE l.locktype = 'advisory' AND NOT l.granted
                AND a.wait_event_type = 'Lock'
                AND l.classid::bigint = (($1::bigint >> 32) & 4294967295)
                AND l.objid::bigint = ($1::bigint & 4294967295)
         )",
    )
    .bind(key)
    .fetch_one(pool)
    .await
}

async fn wait_for_erase_holding_owner_fence_and_waiting_on_test_lock(
    pool: &PgPool,
    owner: Owner,
    test_lock_key: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    let label = owner_fence_label(owner);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let observed: bool = sqlx::query_scalar(
                "WITH owner_key AS (SELECT hashtextextended($1, 0) AS key),
                      test_key AS (SELECT $2::bigint AS key)
                 SELECT EXISTS (
                     SELECT 1 FROM pg_locks l CROSS JOIN owner_key k
                      WHERE l.locktype = 'advisory' AND l.granted
                        AND l.mode = 'ExclusiveLock'
                        AND l.classid::bigint = ((k.key >> 32) & 4294967295)
                        AND l.objid::bigint = (k.key & 4294967295)
                 ) AND EXISTS (
                     SELECT 1
                       FROM pg_locks l
                       CROSS JOIN test_key k
                      WHERE l.locktype = 'advisory' AND NOT l.granted
                        AND l.classid::bigint = ((k.key >> 32) & 4294967295)
                        AND l.objid::bigint = (k.key & 4294967295)
                 )",
            )
            .bind(label.as_str())
            .bind(test_lock_key)
            .fetch_one(pool)
            .await?;
            if observed {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn injected_failures_leave_no_partial_commit() {
    let db_name = unique_db_name("proxima_uow_hs_fail");
    create_split_core_db(&db_name).await.expect("PG required");
    let (url, platform_url) = split_role_urls(&db_name).await.expect("split roles");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, &platform_url, owner, Some(participant.clone())).await?;
        let other = PgPool::connect(&db_url(&db_name)).await?;
        let authz = admin_authz_for(owner);
        let engine = built.engine();

        participant.arm_fail_before_sql();
        let mut before = engine.unit_of_work(&authz).await?;
        let fact_before = before
            .ingest_fact(proxima::FactWrite::new(
                owner,
                "test/hs-fail-before",
                &note("before"),
            ))
            .await?;
        let err = before
            .apply_host_state(FixtureHostCommand::Create {
                owner,
                invocation_id: fact_before.memory_id,
            })
            .await
            .expect_err("injected failure before SQL");
        assert_eq!(err.code, ErrorCode::Internal, "{err:?}");
        let commit_err = before
            .commit()
            .await
            .expect_err("poisoned unit cannot commit");
        assert_eq!(commit_err.code, ErrorCode::Internal, "{commit_err:?}");
        assert_eq!(count_memory(&other, fact_before.memory_id).await?, 0);
        assert_eq!(count_execution(&other, fact_before.memory_id).await?, 0);

        participant.arm_fail_after_sql();
        let mut after = engine.unit_of_work(&authz).await?;
        let fact_after = after
            .ingest_fact(proxima::FactWrite::new(
                owner,
                "test/hs-fail-after",
                &note("after"),
            ))
            .await?;
        let err = after
            .apply_host_state(FixtureHostCommand::Create {
                owner,
                invocation_id: fact_after.memory_id,
            })
            .await
            .expect_err("injected failure after SQL");
        assert_eq!(err.code, ErrorCode::Internal, "{err:?}");
        let commit_err = after
            .commit()
            .await
            .expect_err("poisoned unit cannot commit after SQL");
        assert_eq!(commit_err.code, ErrorCode::Internal, "{commit_err:?}");
        assert_eq!(count_memory(&other, fact_after.memory_id).await?, 0);
        assert_eq!(count_execution(&other, fact_after.memory_id).await?, 0);

        let mut setup = engine.unit_of_work(&authz).await?;
        let created = setup
            .ingest_fact(proxima::FactWrite::new(
                owner,
                "test/hs-fail-update-setup",
                &note("update-setup"),
            ))
            .await?;
        setup
            .apply_host_state(FixtureHostCommand::Create {
                owner,
                invocation_id: created.memory_id,
            })
            .await?;
        setup.commit().await?;

        participant.arm_fail_after_sql();
        let mut update = engine.unit_of_work(&authz).await?;
        let update_fact = update
            .ingest_fact(proxima::FactWrite::new(
                owner,
                "test/hs-fail-update",
                &note("update-fail"),
            ))
            .await?;
        let err = update
            .apply_host_state(FixtureHostCommand::Finalize {
                owner,
                invocation_id: created.memory_id,
            })
            .await
            .expect_err("injected failure after UPDATE");
        assert_eq!(err.code, ErrorCode::Internal, "{err:?}");
        let commit_err = update
            .commit()
            .await
            .expect_err("poisoned finalize cannot commit");
        assert_eq!(commit_err.code, ErrorCode::Internal, "{commit_err:?}");
        assert_eq!(count_memory(&other, update_fact.memory_id).await?, 0);
        assert_eq!(
            execution_status(&other, created.memory_id).await?,
            Some(("created".into(), 1))
        );

        built.shutdown();
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop fixture");
    result.expect("injected failures");
}

#[tokio::test]
async fn unauthorized_unregistered_and_invalid_binding_refuse_before_mutation() {
    let db_name = unique_db_name("proxima_uow_hs_authz");
    create_split_core_db(&db_name).await.expect("PG required");
    let (url, platform_url) = split_role_urls(&db_name).await.expect("split roles");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, &platform_url, owner, Some(participant)).await?;
        let engine = built.engine();
        let other = PgPool::connect(&db_url(&db_name)).await?;
        let invocation = proxima_core::MemoryId::new(Uuid::now_v7());

        let denied = AuthzContext::denied_for_owner(&owner);
        let mut unauthorized = engine.unit_of_work(&denied).await?;
        let err = unauthorized
            .apply_host_state(FixtureHostCommand::Create {
                owner,
                invocation_id: invocation,
            })
            .await
            .expect_err("denied authz");
        assert_eq!(err.code, ErrorCode::Forbidden, "{err:?}");
        drop(unauthorized);
        assert_eq!(count_execution(&other, invocation).await?, 0);

        let authz = admin_authz_for(owner);
        let mut invalid = engine.unit_of_work(&authz).await?;
        let err = invalid
            .apply_host_state(InvalidBindingCommand { owner })
            .await
            .expect_err("memory is not a state surface");
        assert_eq!(err.code, ErrorCode::InvalidArgument, "{err:?}");
        assert!(err.message.contains("state_surfaces"), "{err:?}");
        drop(invalid);
        assert_eq!(count_execution(&other, invocation).await?, 0);

        let mut core_table = engine.unit_of_work(&authz).await?;
        let err = core_table
            .apply_host_state(CoreStateSurfaceCommand { owner })
            .await
            .expect_err("core Goal table is not a host-state binding");
        assert_eq!(err.code, ErrorCode::InvalidArgument, "{err:?}");
        drop(core_table);

        let mut unknown = engine.unit_of_work(&authz).await?;
        let err = unknown
            .apply_host_state(UnknownParticipantCommand { owner })
            .await
            .expect_err("unknown participant");
        assert_eq!(err.code, ErrorCode::InvalidArgument, "{err:?}");
        drop(unknown);
        assert_eq!(count_execution(&other, invocation).await?, 0);

        built.shutdown();

        let unregistered = boot_fixture(&url, &platform_url, owner, None).await?;
        let authz = admin_authz_for(owner);
        let engine = unregistered.engine();
        let mut uow = engine.unit_of_work(&authz).await?;
        let err = uow
            .apply_host_state(FixtureHostCommand::Create {
                owner,
                invocation_id: invocation,
            })
            .await
            .expect_err("no participant registered");
        assert_eq!(err.code, ErrorCode::InvalidArgument, "{err:?}");
        drop(uow);
        assert_eq!(count_execution(&other, invocation).await?, 0);
        unregistered.shutdown();
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop fixture");
    result.expect("refusals");
}

#[tokio::test]
async fn concurrent_finalize_commits_exactly_one_transition() {
    let db_name = unique_db_name("proxima_uow_hs_conc");
    create_split_core_db(&db_name).await.expect("PG required");
    let (url, platform_url) = split_role_urls(&db_name).await.expect("split roles");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let other_owner = company_owner(Uuid::now_v7());
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, &platform_url, owner, Some(participant)).await?;
        let authz = admin_authz_for(owner);
        let other_authz = admin_authz_for(other_owner);
        let engine = built.engine();
        let pool = admin_pool(&db_name).await?;

        let mut setup = engine.unit_of_work(&authz).await?;
        let invocation = setup
            .ingest_fact(proxima::FactWrite::new(
                owner,
                "test/hs-conc",
                &note("invoke"),
            ))
            .await?;
        setup
            .apply_host_state(FixtureHostCommand::Create {
                owner,
                invocation_id: invocation.memory_id,
            })
            .await?;
        setup.commit().await?;

        let key = invocation_lock_key(invocation.memory_id);
        let engine_a = engine.clone();
        let engine_b = engine.clone();
        let authz_a = authz.clone();
        let authz_b = authz.clone();
        let invocation_id = invocation.memory_id;
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let barrier_a = Arc::clone(&barrier);
        let barrier_b = Arc::clone(&barrier);

        let left = tokio::spawn(async move {
            let mut uow = engine_a.unit_of_work(&authz_a).await.unwrap();
            barrier_a.wait().await;
            uow.advisory_xact_lock(key).await.unwrap();
            let read = uow
                .apply_host_state(FixtureHostCommand::Read {
                    owner,
                    invocation_id,
                })
                .await
                .unwrap();
            let wrote_fact = if matches!(
                &read,
                HostStateOutcome::Permitted(FixtureHostResult::Row(Some(row)))
                    if row.status == "created"
            ) {
                uow.ingest_fact(proxima::FactWrite::new(
                    owner,
                    "test/hs-conc-a",
                    &note("winner-or-loser-a"),
                ))
                .await
                .unwrap();
                let outcome = uow
                    .apply_host_state(FixtureHostCommand::Finalize {
                        owner,
                        invocation_id,
                    })
                    .await
                    .unwrap();
                outcome.is_permitted()
            } else {
                false
            };
            uow.commit().await.unwrap();
            wrote_fact
        });
        let right = tokio::spawn(async move {
            let mut uow = engine_b.unit_of_work(&authz_b).await.unwrap();
            barrier_b.wait().await;
            uow.advisory_xact_lock(key).await.unwrap();
            let read = uow
                .apply_host_state(FixtureHostCommand::Read {
                    owner,
                    invocation_id,
                })
                .await
                .unwrap();
            let wrote_fact = if matches!(
                &read,
                HostStateOutcome::Permitted(FixtureHostResult::Row(Some(row)))
                    if row.status == "created"
            ) {
                uow.ingest_fact(proxima::FactWrite::new(
                    owner,
                    "test/hs-conc-b",
                    &note("winner-or-loser-b"),
                ))
                .await
                .unwrap();
                let outcome = uow
                    .apply_host_state(FixtureHostCommand::Finalize {
                        owner,
                        invocation_id,
                    })
                    .await
                    .unwrap();
                outcome.is_permitted()
            } else {
                false
            };
            uow.commit().await.unwrap();
            wrote_fact
        });

        let left_won = left.await?;
        let right_won = right.await?;
        assert_ne!(left_won, right_won, "exactly one permitted finalize");
        assert!(left_won || right_won);

        let facts: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory")
            .fetch_one(&pool)
            .await?;
        assert_eq!(facts, 2, "setup Fact plus exactly one finalize Fact");
        assert_eq!(
            execution_status(&pool, invocation.memory_id).await?,
            Some(("finalized".into(), 2))
        );

        let mut steal = engine.unit_of_work(&other_authz).await?;
        let stolen = steal
            .apply_host_state(FixtureHostCommand::Finalize {
                owner: other_owner,
                invocation_id: invocation.memory_id,
            })
            .await?;
        assert!(
            stolen.is_refused(),
            "other owner cannot take the row: {stolen:?}"
        );
        steal.commit().await?;
        assert_eq!(
            execution_status(&pool, invocation.memory_id).await?,
            Some(("finalized".into(), 2))
        );

        built.shutdown();
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop fixture");
    result.expect("concurrency");
}

#[tokio::test]
async fn refused_finalize_of_missing_row_writes_nothing() {
    let db_name = unique_db_name("proxima_uow_hs_refuse");
    create_split_core_db(&db_name).await.expect("PG required");
    let (url, platform_url) = split_role_urls(&db_name).await.expect("split roles");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, &platform_url, owner, Some(participant)).await?;
        let authz = admin_authz_for(owner);
        let missing = proxima_core::MemoryId::new(Uuid::now_v7());
        let engine = built.engine();
        let mut uow = engine.unit_of_work(&authz).await?;
        let outcome = uow
            .apply_host_state(FixtureHostCommand::Finalize {
                owner,
                invocation_id: missing,
            })
            .await?;
        assert!(
            matches!(
                outcome,
                HostStateOutcome::Refused(FixtureHostResult::Missing)
            ),
            "{outcome:?}"
        );
        uow.commit().await?;
        assert_eq!(
            count_execution(&admin_pool(&db_name).await?, missing).await?,
            0
        );
        built.shutdown();
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop fixture");
    result.expect("refused missing");
}

#[tokio::test]
async fn cancelled_host_op_after_sql_cannot_commit() {
    let db_name = unique_db_name("proxima_uow_hs_cancel");
    create_split_core_db(&db_name).await.expect("PG required");
    let (url, platform_url) = split_role_urls(&db_name).await.expect("split roles");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, &platform_url, owner, Some(participant.clone())).await?;
        let other = PgPool::connect(&db_url(&db_name)).await?;
        let authz = admin_authz_for(owner);
        let engine = built.engine();

        participant.arm_hang_after_sql();
        let mut uow = engine.unit_of_work(&authz).await?;
        let fact = uow
            .ingest_fact(proxima::FactWrite::new(
                owner,
                "test/hs-cancel",
                &note("cancel"),
            ))
            .await?;
        let sql_completed = participant.sql_completed();
        tokio::select! {
            biased;
            () = sql_completed => {}
            result = uow.apply_host_state(FixtureHostCommand::Create {
                owner,
                invocation_id: fact.memory_id,
            }) => {
                panic!("hanging host command completed: {result:?}");
            }
        }
        let err = uow
            .commit()
            .await
            .expect_err("cancelled host op poisons the unit");
        assert_eq!(err.code, ErrorCode::Internal, "{err:?}");
        assert_eq!(count_memory(&other, fact.memory_id).await?, 0);
        assert_eq!(count_execution(&other, fact.memory_id).await?, 0);
        built.shutdown();
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop fixture");
    result.expect("cancel after SQL");
}

#[tokio::test]
async fn host_only_authority_is_engine_bound_owner_fixed_and_works_for_personal_and_group() {
    let db_name = unique_db_name("proxima_uow_hs_authority");
    create_split_core_db(&db_name).await.expect("PG required");
    let (url, platform_url) = split_role_urls(&db_name).await.expect("split roles");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let group = Owner::Group(GroupId::new(Uuid::now_v7()));
        let personal = Owner::Personal(UserId::new(Uuid::now_v7()));
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, &platform_url, group, Some(participant.clone())).await?;
        let authority = built
            .host_state_maintenance_authority()
            .expect("registered participant mints host-only authority");
        let engine = built.engine();
        let observer = PgPool::connect(&db_url(&db_name)).await?;

        for (owner, key) in [
            (group, "host-authority-group"),
            (personal, "host-authority-personal"),
        ] {
            let authz = admin_authz_for(owner);
            let mut setup = engine.unit_of_work(&authz).await?;
            let fact = setup
                .ingest_fact(proxima::FactWrite::new(owner, key, &note(key)))
                .await?;
            setup
                .apply_host_state(FixtureHostCommand::Create {
                    owner,
                    invocation_id: fact.memory_id,
                })
                .await?;
            setup.commit().await?;

            // No AuthzContext is accepted by this one-owner maintenance API.
            let mut maintenance = engine.host_state_unit_of_work(authority, owner)?;
            let result = maintenance
                .apply_host_state(FixtureHostCommand::Read {
                    owner,
                    invocation_id: fact.memory_id,
                })
                .await?;
            assert!(matches!(
                result,
                HostStateOutcome::Permitted(FixtureHostResult::Row(Some(ref row)))
                    if row.status == "created" && row.version == 1
            ));

            let finalized = maintenance
                .apply_host_state(FixtureHostCommand::Finalize {
                    owner,
                    invocation_id: fact.memory_id,
                })
                .await?;
            assert!(matches!(
                finalized,
                HostStateOutcome::Permitted(FixtureHostResult::Finalized {
                    invocation_id,
                    version: 2,
                }) if invocation_id == fact.memory_id
            ));
            maintenance.commit().await?;

            assert_eq!(
                execution_status(&observer, fact.memory_id).await?,
                Some(("finalized".to_owned(), 2)),
                "host-only mutation must commit for {owner:?} and be visible on another connection"
            );
            let (target_kind, target_id) = owner.columns();
            assert_eq!(
                execution_provenance(&observer, fact.memory_id).await?,
                Some((
                    target_kind.as_str().to_owned(),
                    target_id,
                    None,
                    None,
                    true,
                    None,
                    None,
                )),
                "maintenance stamps no ordinary principal and preserves the target owner"
            );
        }

        let calls_before_rejections = participant.callback_calls();
        let mut wrong_owner = engine.host_state_unit_of_work(authority, group)?;
        let error = wrong_owner
            .apply_host_state(FixtureHostCommand::Read {
                owner: personal,
                invocation_id: proxima_core::MemoryId::new(Uuid::now_v7()),
            })
            .await
            .expect_err("fixed unit owner rejects another command owner");
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{error:?}");
        drop(wrong_owner);

        let mut wrong_participant = engine.host_state_unit_of_work(authority, group)?;
        let error = wrong_participant
            .apply_host_state(UnknownParticipantCommand { owner: group })
            .await
            .expect_err("command participant must match actual registration");
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{error:?}");
        drop(wrong_participant);

        let mut wrong_table = engine.host_state_unit_of_work(authority, group)?;
        let error = wrong_table
            .apply_host_state(AuxiliaryHostCommand { owner: group })
            .await
            .expect_err("unregistered participant table must fail before dispatch");
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{error:?}");
        drop(wrong_table);

        let mut duplicate = engine.host_state_unit_of_work(authority, group)?;
        let error = duplicate
            .apply_host_state(DuplicateTablesCommand { owner: group })
            .await
            .expect_err("duplicate request tables are rejected");
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{error:?}");
        drop(duplicate);

        let empty_invocation = proxima_core::MemoryId::new(Uuid::now_v7());
        let mut empty = engine.host_state_unit_of_work(authority, group)?;
        let error = empty
            .apply_host_state(EmptyTablesCommand { owner: group })
            .await
            .expect_err("empty request tables are rejected before opening a session");
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{error:?}");
        assert!(error.message.contains("no state surfaces"), "{error:?}");
        drop(empty);
        assert_eq!(participant.callback_calls(), calls_before_rejections);
        assert_eq!(
            count_execution(&admin_pool(&db_name).await?, empty_invocation).await?,
            0
        );

        // A capability minted by one engine cannot be paired with another
        // boot, even when both engines captured identical metadata.
        let second_participant = Arc::new(HostFixtureParticipant::default());
        let second =
            boot_fixture(&url, &platform_url, group, Some(second_participant.clone())).await?;
        let second_engine = second.engine();
        let error = second_engine
            .host_state_unit_of_work(authority, group)
            .expect_err("capability belongs to the first engine");
        assert_eq!(error.code, ErrorCode::Forbidden, "{error:?}");
        assert_eq!(second_participant.callback_calls(), 0);

        second.shutdown();
        built.shutdown();
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop fixture");
    result.expect("host authority invariants");
}

#[tokio::test]
async fn frozen_descriptor_rejects_full_invalid_registration_and_cannot_widen_after_boot() {
    let db_name = unique_db_name("proxima_uow_hs_descriptor");
    create_split_core_db(&db_name).await.expect("PG required");
    let (url, platform_url) = split_role_urls(&db_name).await.expect("split roles");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        for (participant, expected_message) in [
            (
                Arc::new(DuplicateDescriptorParticipant) as Arc<dyn PgHostStateParticipant>,
                "more than once",
            ),
            (
                Arc::new(UndeclaredDescriptorParticipant) as Arc<dyn PgHostStateParticipant>,
                "state_surfaces",
            ),
            (
                Arc::new(EmptyDescriptorParticipant) as Arc<dyn PgHostStateParticipant>,
                "no state surfaces",
            ),
        ] {
            let error = boot_fixture(&url, &platform_url, owner, Some(participant))
                .await
                .expect_err("invalid full participant registration must fail boot");
            assert!(error.to_string().contains(expected_message), "{error}");
        }

        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, &platform_url, owner, Some(participant.clone())).await?;
        assert_eq!(
            participant.metadata_reads(),
            2,
            "id and tables captured once"
        );
        participant.arm_widen_descriptor_after_capture();

        let authority = built
            .host_state_maintenance_authority()
            .expect("valid participant capability");
        let engine = built.engine();
        let mut widened = engine.host_state_unit_of_work(authority, owner)?;
        let error = widened
            .apply_host_state(AuxiliaryHostCommand { owner })
            .await
            .expect_err("metadata getter change cannot widen the boot descriptor");
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{error:?}");
        drop(widened);
        assert_eq!(participant.callback_calls(), 0);

        let mut valid = engine.host_state_unit_of_work(authority, owner)?;
        let result = valid
            .apply_host_state(FixtureHostCommand::Read {
                owner,
                invocation_id: proxima_core::MemoryId::new(Uuid::now_v7()),
            })
            .await?;
        assert!(matches!(
            result,
            HostStateOutcome::Permitted(FixtureHostResult::Row(None))
        ));
        valid.commit().await?;
        assert_eq!(
            participant.metadata_reads(),
            2,
            "dispatch never re-reads getters"
        );
        assert_eq!(participant.callback_calls(), 1);
        built.shutdown();
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop fixture");
    result.expect("frozen registration descriptor");
}

#[tokio::test]
async fn ordinary_group_editor_provenance_is_not_target_or_command_metadata() {
    let db_name = unique_db_name("proxima_uow_hs_origin");
    create_split_core_db(&db_name).await.expect("PG required");
    let (url, platform_url) = split_role_urls(&db_name).await.expect("split roles");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let group = company_owner(Uuid::now_v7());
        let editor = UserId::new(Uuid::now_v7());
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, &platform_url, group, Some(participant.clone())).await?;
        let authz = proxima_core::test_fixtures::authenticated_context(
            AuthzContext::for_subject_with_role(
                editor,
                [(group, Role::editor())],
                AuthPath::HostBearer,
            )
            .narrowed_to_owner(group)
            .expect("group editor is narrowed to its target owner"),
        );
        assert_eq!(authz.principal(), Owner::Personal(editor));
        assert_ne!(authz.principal(), group);

        let engine = built.engine();
        let mut unit = engine.unit_of_work(&authz).await?;
        let fact = unit
            .ingest_fact(proxima::FactWrite::new(
                group,
                "test/hs-group-editor-origin",
                &note("group-editor-origin"),
            ))
            .await?;
        unit.apply_host_state(FixtureHostCommand::CreateWithPayloadSavedBy {
            owner: group,
            saved_by: group,
            invocation_id: fact.memory_id,
        })
        .await?;
        unit.commit().await?;

        let (target_kind, target_id) = group.columns();
        assert_eq!(
            execution_provenance(&admin_pool(&db_name).await?, fact.memory_id).await?,
            Some((
                target_kind.as_str().to_owned(),
                target_id,
                Some("personal".to_owned()),
                Some(editor.into_inner()),
                false,
                Some("group".to_owned()),
                Some(group.stored_owner_id()),
            )),
            "the committed fixture row distinguishes target, caller stamp and fake payload"
        );
        assert_eq!(participant.callback_calls(), 1);
        built.shutdown();
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop fixture");
    result.expect("ordinary caller provenance");
}

#[tokio::test]
async fn subjectless_denied_ordinary_host_write_never_dispatches() {
    let db_name = unique_db_name("proxima_uow_hs_denied_origin");
    create_split_core_db(&db_name).await.expect("PG required");
    let (url, platform_url) = split_role_urls(&db_name).await.expect("split roles");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, &platform_url, owner, Some(participant.clone())).await?;
        let authz = AuthzContext::denied_for_owner(&owner);
        assert_eq!(authz.subject(), None);
        let engine = built.engine();
        let mut unit = engine.unit_of_work(&authz).await?;
        let invocation_id = proxima_core::MemoryId::new(Uuid::now_v7());
        let error = unit
            .apply_host_state(FixtureHostCommand::Read {
                owner,
                invocation_id,
            })
            .await
            .expect_err("subjectless denied context has no ordinary host-write authority");
        assert_eq!(error.code, ErrorCode::Forbidden, "{error:?}");
        drop(unit);
        assert_eq!(participant.callback_calls(), 0);
        assert_eq!(
            count_execution(&admin_pool(&db_name).await?, invocation_id).await?,
            0
        );
        built.shutdown();
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop fixture");
    result.expect("denied ordinary host state");
}

#[tokio::test]
async fn opaque_payload_owner_mismatch_is_checked_inside_participant_before_sql() {
    let db_name = unique_db_name("proxima_uow_hs_payload_owner");
    create_split_core_db(&db_name).await.expect("PG required");
    let (url, platform_url) = split_role_urls(&db_name).await.expect("split roles");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let foreign = Owner::Personal(UserId::new(Uuid::now_v7()));
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, &platform_url, owner, Some(participant.clone())).await?;
        let authority = built
            .host_state_maintenance_authority()
            .expect("host authority");
        let invocation = proxima_core::MemoryId::new(Uuid::now_v7());
        let engine = built.engine();
        let mut maintenance = engine.host_state_unit_of_work(authority, owner)?;
        let error = maintenance
            .apply_host_state(FixtureHostCommand::CreateWithPayloadOwner {
                owner,
                payload_owner: foreign,
                invocation_id: invocation,
            })
            .await
            .expect_err("opaque payload's owner is participant-checked");
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{error:?}");
        assert_eq!(
            participant.callback_calls(),
            1,
            "callback owns payload check"
        );
        drop(maintenance);
        assert_eq!(
            count_execution(&admin_pool(&db_name).await?, invocation).await?,
            0
        );
        built.shutdown();
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop fixture");
    result.expect("payload owner boundary");
}

#[tokio::test]
async fn deferred_fk_commit_failure_rolls_back_fact_and_host_state_rows() {
    let db_name = unique_db_name("proxima_uow_hs_deferred");
    create_split_core_db(&db_name).await.expect("PG required");
    let (url, platform_url) = split_role_urls(&db_name).await.expect("split roles");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, &platform_url, owner, Some(participant)).await?;
        let other = PgPool::connect(&db_url(&db_name)).await?;
        let authz = admin_authz_for(owner);
        let engine = built.engine();
        let mut unit = engine.unit_of_work(&authz).await?;
        let fact = unit
            .ingest_fact(proxima::FactWrite::new(
                owner,
                "test/host-state-deferred-fk",
                &note("deferred-fk"),
            ))
            .await?;
        unit.apply_host_state(FixtureHostCommand::Create {
            owner,
            invocation_id: fact.memory_id,
        })
        .await?;
        unit.apply_host_state(FixtureHostCommand::InsertDeferredInvalid {
            owner,
            invocation_id: fact.memory_id,
        })
        .await?;
        let error = unit
            .commit()
            .await
            .expect_err("deferred constraint fails at actual COMMIT");
        assert_eq!(error.code, ErrorCode::Internal, "{error:?}");
        assert_eq!(count_memory(&other, fact.memory_id).await?, 0);
        assert_eq!(count_execution(&other, fact.memory_id).await?, 0);
        let deferred_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM host_fixture.deferred_reference WHERE invocation_id = $1",
        )
        .bind(fact.memory_id.into_inner())
        .fetch_one(&other)
        .await?;
        assert_eq!(deferred_rows, 0);
        built.shutdown();
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop fixture");
    result.expect("deferred COMMIT rollback");
}

#[tokio::test]
async fn host_state_and_whole_owner_erase_wait_on_the_same_owner_fence_both_ways() {
    const TRIGGER_LOCK_KEY: i64 = 8_719_872_200_019;

    let db_name = unique_db_name("proxima_uow_hs_owner_fence");
    create_split_core_db(&db_name).await.expect("PG required");
    let (url, platform_url) = split_role_urls(&db_name).await.expect("split roles");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = Owner::Group(GroupId::new(Uuid::now_v7()));
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, &platform_url, owner, Some(participant.clone())).await?;
        let engine = built.engine();
        let pool = admin_pool(&db_name).await?;
        let authz = admin_authz_for(owner);

        let mut setup = engine.unit_of_work(&authz).await?;
        let fact = setup
            .ingest_fact(proxima::FactWrite::new(
                owner,
                "test/host-state-owner-fence",
                &note("owner-fence"),
            ))
            .await?;
        setup
            .apply_host_state(FixtureHostCommand::Create {
                owner,
                invocation_id: fact.memory_id,
            })
            .await?;
        setup.commit().await?;

        // A host participant holds the production shared fence until its
        // write session is dropped. Whole-owner erase must wait for it.
        participant.arm_hang_after_sql();
        let completion = participant.sql_completed();
        let maintenance_engine = engine.clone();
        let maintenance_authority = engine
            .host_state_maintenance_authority(built.system_authority())?
            .expect("host participant authority");
        let maintenance = tokio::spawn(async move {
            let mut unit =
                maintenance_engine.host_state_unit_of_work(&maintenance_authority, owner)?;
            unit.apply_host_state(FixtureHostCommand::Read {
                owner,
                invocation_id: fact.memory_id,
            })
            .await
        });
        tokio::time::timeout(Duration::from_secs(10), completion)
            .await
            .map_err(|error| {
                std::io::Error::other(format!("participant callback wait: {error}"))
            })?;

        let erase_engine = engine.clone();
        let erase_authz = AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::System);
        let erase = tokio::spawn(async move {
            erase_engine
                .erase_group_owner(&erase_authz, GroupId::new(owner.stored_owner_id()))
                .await
        });
        wait_for_owner_fence_wait(&pool, owner)
            .await
            .map_err(|error| std::io::Error::other(format!("erase fence wait: {error}")))?;
        assert!(advisory_wait_reports_lock_event(&pool, &owner_fence_label(owner)).await?);
        maintenance.abort();
        assert!(
            maintenance
                .await
                .expect_err("cancelled maintenance")
                .is_cancelled()
        );
        let erased = erase.await??;
        assert!(matches!(
            erased,
            proxima::OwnerEraseOutcome::Completed { .. }
        ));
        assert_eq!(count_execution(&pool, fact.memory_id).await?, 0);

        // Hold an independent test advisory lock inside a trigger. The real
        // erase path first acquires its exclusive owner fence, then waits in
        // the trigger. A host-state call started behind it must show a
        // Lock wait on that same owner fence before the trigger lock releases.
        let trigger_key = TRIGGER_LOCK_KEY;
        let mut blocker = pool.acquire().await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(trigger_key)
            .execute(&mut *blocker)
            .await?;
        sqlx::query(
            "CREATE FUNCTION host_fixture.block_execution_erase() RETURNS trigger \
             LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(8719872200019); \
             RETURN OLD; END $$",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "CREATE TRIGGER block_execution_erase BEFORE DELETE ON host_fixture.execution \
             FOR EACH ROW EXECUTE FUNCTION host_fixture.block_execution_erase()",
        )
        .execute(&pool)
        .await?;

        let mut setup_again = engine.unit_of_work(&authz).await?;
        let seeded_fact = setup_again
            .ingest_fact(proxima::FactWrite::new(
                owner,
                "test/host-state-owner-fence-second",
                &note("owner-fence-second"),
            ))
            .await?;
        setup_again
            .apply_host_state(FixtureHostCommand::Create {
                owner,
                invocation_id: seeded_fact.memory_id,
            })
            .await?;
        setup_again.commit().await?;

        let erase_engine = engine.clone();
        let erase_authz = AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::System);
        let erase = tokio::spawn(async move {
            erase_engine
                .erase_group_owner(&erase_authz, GroupId::new(owner.stored_owner_id()))
                .await
        });
        wait_for_erase_holding_owner_fence_and_waiting_on_test_lock(&pool, owner, trigger_key)
            .await
            .map_err(|error| std::io::Error::other(format!("erase trigger lock wait: {error}")))?;
        assert!(advisory_integer_wait_reports_lock_event(&pool, trigger_key).await?);

        let callback_count = participant.callback_calls();
        let waiting_engine = engine.clone();
        let waiting_authority = engine
            .host_state_maintenance_authority(built.system_authority())?
            .expect("host participant authority");
        let waiting = tokio::spawn(async move {
            let mut unit = waiting_engine.host_state_unit_of_work(&waiting_authority, owner)?;
            let result = unit
                .apply_host_state(FixtureHostCommand::Read {
                    owner,
                    invocation_id: seeded_fact.memory_id,
                })
                .await?;
            unit.commit().await?;
            Ok::<_, proxima::ProtocolError>(result)
        });
        wait_for_owner_fence_wait(&pool, owner)
            .await
            .map_err(|error| std::io::Error::other(format!("maintenance fence wait: {error}")))?;
        assert!(advisory_wait_reports_lock_event(&pool, &owner_fence_label(owner)).await?);
        assert_eq!(
            participant.callback_calls(),
            callback_count,
            "the callback waits until erase releases the exclusive owner fence"
        );

        let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
            .bind(trigger_key)
            .fetch_one(&mut *blocker)
            .await?;
        assert!(unlocked, "test releases trigger blocker");
        assert!(matches!(
            erase.await??,
            proxima::OwnerEraseOutcome::Completed { .. }
        ));
        let after_erase = waiting.await??;
        assert!(matches!(
            after_erase,
            HostStateOutcome::Permitted(FixtureHostResult::Row(None))
        ));
        assert_eq!(count_execution(&pool, seeded_fact.memory_id).await?, 0);
        built.shutdown();
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop fixture");
    result.expect("owner-fence serialization");
}

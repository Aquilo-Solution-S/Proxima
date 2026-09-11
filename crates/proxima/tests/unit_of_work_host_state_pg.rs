//! Real-Postgres host-state participation on the existing [`proxima::UnitOfWork`].
#![allow(clippy::too_many_lines)]

#[path = "fixtures/host_state/mod.rs"]
mod host_state_fixture;

use host_state_fixture::{
    CoreStateSurfaceCommand, FixtureHostCommand, FixtureHostResult, HostFixtureApp,
    HostFixtureParticipant, InvalidBindingCommand, UnknownParticipantCommand, invocation_lock_key,
};
use proxima::flavor::{FlavorBundle, NamedMigrator};
use proxima::{
    AppInfo, AuthPath, AuthzContext, ErrorCode, FlavorApp, HostStateOutcome, Proxima, Role,
    ToolScope, company_owner,
};
use proxima_core::{AgentNoteV1, Owner, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

struct EmptyApp;

impl FlavorBundle for EmptyApp {
    fn register(
        _: &mut proxima_core::FlavorRegistry,
    ) -> Result<(), proxima_core::FlavorRegistryError> {
        Ok(())
    }
    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for EmptyApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "uow-host-state-empty",
            title: "uow-host-state-empty",
            version: "0",
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

fn admin_authz_for(owner: Owner) -> AuthzContext {
    AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(owner, Role::admin())],
        AuthPath::HostBearer,
    )
    .narrowed_to_owner(owner)
    .expect("an admin on exactly this owner narrows to it")
}

async fn boot_fixture(
    db_url: &str,
    owner: Owner,
    participant: Option<Arc<HostFixtureParticipant>>,
) -> Result<proxima::BuiltProxima, Box<dyn std::error::Error>> {
    let mut app = Proxima::<HostFixtureApp>::app()
        .database_url(db_url)
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

#[tokio::test]
async fn create_commits_fact_and_host_row_together() {
    let db_name = unique_db_name("proxima_uow_hs_create");
    create_db(&db_name).await.expect("PG required");
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, owner, Some(participant)).await?;
        let other = PgPool::connect(&url).await?;
        let authz = built.single_owner_authz().expect("single owner");
        let engine = built.engine();

        let mut uow = engine.unit_of_work(&authz).await?;
        let fact = uow
            .ingest_fact(proxima::FactWrite::new(
                owner,
                "test/hs-create",
                &note("create"),
            ))
            .await?;
        let created = uow
            .apply_host_state(FixtureHostCommand::Create {
                owner,
                invocation_id: fact.memory_id,
            })
            .await?;
        assert!(
            matches!(
                created,
                HostStateOutcome::Permitted(FixtureHostResult::Created { version: 1, .. })
            ),
            "{created:?}"
        );

        assert_eq!(count_memory(&other, fact.memory_id).await?, 0);
        assert_eq!(count_execution(&other, fact.memory_id).await?, 0);

        let seen = uow
            .apply_host_state(FixtureHostCommand::Read {
                owner,
                invocation_id: fact.memory_id,
            })
            .await?;
        assert!(
            matches!(
                seen,
                HostStateOutcome::Permitted(FixtureHostResult::Row(Some(_)))
            ),
            "same unit must see its uncommitted host row: {seen:?}"
        );

        uow.commit().await?;
        assert_eq!(count_memory(&other, fact.memory_id).await?, 1);
        assert_eq!(count_execution(&other, fact.memory_id).await?, 1);
        built.shutdown();
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop fixture");
    result.expect("create atomicity");
}

#[tokio::test]
async fn finalize_is_conditional_and_already_applied_writes_no_duplicate() {
    let db_name = unique_db_name("proxima_uow_hs_fin");
    create_db(&db_name).await.expect("PG required");
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, owner, Some(participant)).await?;
        let authz = built.single_owner_authz().expect("single owner");
        let engine = built.engine();
        let pool = built.pool_for_tests();

        let mut create = engine.unit_of_work(&authz).await?;
        let invocation = create
            .ingest_fact(proxima::FactWrite::new(
                owner,
                "test/hs-fin",
                &note("invoke"),
            ))
            .await?;
        create
            .apply_host_state(FixtureHostCommand::Create {
                owner,
                invocation_id: invocation.memory_id,
            })
            .await?;
        create.commit().await?;

        let mut first = engine.unit_of_work(&authz).await?;
        first
            .advisory_xact_lock(invocation_lock_key(invocation.memory_id))
            .await?;
        let read = first
            .apply_host_state(FixtureHostCommand::Read {
                owner,
                invocation_id: invocation.memory_id,
            })
            .await?;
        assert!(
            matches!(
                read,
                HostStateOutcome::Permitted(FixtureHostResult::Row(Some(ref row)))
                    if row.status == "created"
            ),
            "{read:?}"
        );
        let done = first
            .ingest_fact(proxima::FactWrite::new(
                owner,
                "test/hs-fin-done",
                &note("finalized"),
            ))
            .await?;
        let transition = first
            .apply_host_state(FixtureHostCommand::Finalize {
                owner,
                invocation_id: invocation.memory_id,
            })
            .await?;
        assert!(
            matches!(
                transition,
                HostStateOutcome::Permitted(FixtureHostResult::Finalized { version: 2, .. })
            ),
            "{transition:?}"
        );
        first.commit().await?;

        let mut second = engine.unit_of_work(&authz).await?;
        second
            .advisory_xact_lock(invocation_lock_key(invocation.memory_id))
            .await?;
        let replay = second
            .apply_host_state(FixtureHostCommand::Finalize {
                owner,
                invocation_id: invocation.memory_id,
            })
            .await?;
        assert!(
            matches!(
                replay,
                HostStateOutcome::AlreadyApplied(FixtureHostResult::Finalized { version: 2, .. })
            ),
            "{replay:?}"
        );
        second.commit().await?;

        assert_eq!(count_memory(pool, invocation.memory_id).await?, 1);
        assert_eq!(count_memory(pool, done.memory_id).await?, 1);
        let facts: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory")
            .fetch_one(pool)
            .await?;
        assert_eq!(facts, 2, "already-applied must not append a second Fact");
        assert_eq!(
            execution_status(pool, invocation.memory_id).await?,
            Some(("finalized".into(), 2))
        );
        built.shutdown();
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop fixture");
    result.expect("finalize");
}

#[tokio::test]
async fn drop_without_commit_rolls_fact_and_host_row_back() {
    let db_name = unique_db_name("proxima_uow_hs_drop");
    create_db(&db_name).await.expect("PG required");
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, owner, Some(participant)).await?;
        let other = PgPool::connect(&url).await?;
        let authz = built.single_owner_authz().expect("single owner");
        let engine = built.engine();

        let memory_id;
        {
            let mut uow = engine.unit_of_work(&authz).await?;
            let fact = uow
                .ingest_fact(proxima::FactWrite::new(
                    owner,
                    "test/hs-drop",
                    &note("drop"),
                ))
                .await?;
            memory_id = fact.memory_id;
            uow.apply_host_state(FixtureHostCommand::Create {
                owner,
                invocation_id: memory_id,
            })
            .await?;
        }
        assert_eq!(count_memory(&other, memory_id).await?, 0);
        assert_eq!(count_execution(&other, memory_id).await?, 0);
        built.shutdown();
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop fixture");
    result.expect("drop rollback");
}

#[tokio::test]
async fn injected_failures_leave_no_partial_commit() {
    let db_name = unique_db_name("proxima_uow_hs_fail");
    create_db(&db_name).await.expect("PG required");
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, owner, Some(Arc::clone(&participant))).await?;
        let other = PgPool::connect(&url).await?;
        let authz = built.single_owner_authz().expect("single owner");
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
    create_db(&db_name).await.expect("PG required");
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, owner, Some(participant)).await?;
        let engine = built.engine();
        let other = PgPool::connect(&url).await?;
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

        let authz = built.single_owner_authz().expect("single owner");
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

        let unregistered = boot_fixture(&url, owner, None).await?;
        let authz = unregistered.single_owner_authz().expect("single owner");
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
    create_db(&db_name).await.expect("PG required");
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let other_owner = company_owner(Uuid::now_v7());
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, owner, Some(participant)).await?;
        let authz = admin_authz_for(owner);
        let other_authz = admin_authz_for(other_owner);
        let engine = built.engine();
        let pool = built.pool_for_tests();

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
            .fetch_one(pool)
            .await?;
        assert_eq!(facts, 2, "setup Fact plus exactly one finalize Fact");
        assert_eq!(
            execution_status(pool, invocation.memory_id).await?,
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
            execution_status(pool, invocation.memory_id).await?,
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
async fn host_without_participant_still_ingests_facts() {
    let db_name = unique_db_name("proxima_uow_hs_compat");
    create_db(&db_name).await.expect("PG required");
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(&url)
            .owner(owner)
            .allow_insecure_single_owner()
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let authz = built.single_owner_authz().expect("single owner");
        let engine = built.engine();
        let mut uow = engine.unit_of_work(&authz).await?;
        let fact = uow
            .ingest_fact(proxima::FactWrite::new(
                owner,
                "test/hs-compat",
                &note("compat"),
            ))
            .await?;
        uow.commit().await?;
        assert_eq!(
            count_memory(built.pool_for_tests(), fact.memory_id).await?,
            1
        );
        built.shutdown();
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop fixture");
    result.expect("compatibility");
}

#[tokio::test]
async fn refused_finalize_of_missing_row_writes_nothing() {
    let db_name = unique_db_name("proxima_uow_hs_refuse");
    create_db(&db_name).await.expect("PG required");
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, owner, Some(participant)).await?;
        let authz = built.single_owner_authz().expect("single owner");
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
        assert_eq!(count_execution(built.pool_for_tests(), missing).await?, 0);
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
    create_db(&db_name).await.expect("PG required");
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let participant = Arc::new(HostFixtureParticipant::default());
        let built = boot_fixture(&url, owner, Some(Arc::clone(&participant))).await?;
        let other = PgPool::connect(&url).await?;
        let authz = built.single_owner_authz().expect("single owner");
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

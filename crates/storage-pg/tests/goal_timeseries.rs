//! GoalWrite + write-act Fact.
#![allow(
    clippy::missing_errors_doc,
    clippy::doc_markdown,
    clippy::too_many_lines
)]

use proxima_core::storage_ports::{MemoryReadPort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::query::{EntityKind, QueryRequest};
use proxima_core::{AccessKind, MemoryId, OwnerRef, SchemaId, SchemaVersion, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use proxima_storage_pg::verbs::goal_timeseries::{GoalWriteCommand, WRITE_ACT_SCHEMA, write_goal};
use uuid::Uuid;

fn fact_draft() -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new("core/test-fact-v1".to_string()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        citation: None,
        derived_from: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    }
}

#[tokio::test]
async fn goal_write_replay_terminal_and_write_act() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let pool = pg.pool_for_tests();

        let mut tx = pool.begin().await?;
        let created = write_goal(
            &mut tx,
            &owner,
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-v1".into(),
                title: "ship v008".into(),
                state: GoalState::Active,
                request_id: "req-1".into(),
                close_fact_t: None,
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![],
                wake_id: None,
                mint_write_act: true,
                write_act_t: None,
            },
        )
        .await?;
        tx.commit().await?;
        assert!(!created.replay);
        assert!(created.write_act_t.is_some());

        let schema: String = sqlx::query_scalar(
            "SELECT m.schema_id FROM proxima_core.memory m
             WHERE m.t = $1",
        )
        .bind(created.write_act_t)
        .fetch_one(pool)
        .await?;
        assert_eq!(schema, WRITE_ACT_SCHEMA);

        let mut produced = fact_draft();
        produced.refs = vec![created.write_act_t.expect("write-act")];
        let produced_out = ingest_fact_atomic(pool, &permit, &produced, None).await?;
        let refs: Vec<Uuid> =
            sqlx::query_scalar("SELECT refs FROM proxima_core.memory WHERE t = $1")
                .bind(produced_out.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(refs, vec![created.write_act_t.unwrap()]);

        let mut tx = pool.begin().await?;
        let replay = write_goal(
            &mut tx,
            &owner,
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-v1".into(),
                title: "ignored".into(),
                state: GoalState::Active,
                request_id: "req-1".into(),
                close_fact_t: None,
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![],
                wake_id: None,
                mint_write_act: true,
                write_act_t: None,
            },
        )
        .await?;
        tx.commit().await?;
        assert!(replay.replay);
        assert_eq!(replay.t, created.t);
        assert_eq!(replay.handle, created.handle);

        let close = ingest_fact_atomic(pool, &permit, &fact_draft(), None).await?;
        let mut tx = pool.begin().await?;
        let closed = write_goal(
            &mut tx,
            &owner,
            &GoalWriteCommand {
                handle: Some(created.handle),
                schema_id: "core/task-v1".into(),
                title: "ship v008".into(),
                state: GoalState::Achieved,
                request_id: "req-close".into(),
                close_fact_t: Some(close.memory_id.into_inner()),
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![],
                wake_id: None,
                mint_write_act: false,
                write_act_t: None,
            },
        )
        .await?;
        tx.commit().await?;
        assert_eq!(closed.handle, created.handle);
        assert_ne!(closed.t, created.t);

        let mut tx = pool.begin().await?;
        let err = write_goal(
            &mut tx,
            &owner,
            &GoalWriteCommand {
                handle: Some(created.handle),
                schema_id: "core/task-v1".into(),
                title: "again".into(),
                state: GoalState::Active,
                request_id: "req-after-term".into(),
                close_fact_t: None,
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![],
                wake_id: None,
                mint_write_act: false,
                write_act_t: None,
            },
        )
        .await
        .expect_err("terminal admits no later t");
        assert!(
            err.to_string().contains("terminal") || err.to_string().contains("23514"),
            "got: {err}"
        );

        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("goal timeseries test failed");
}

#[tokio::test]
async fn goal_query_projects_assignment_and_evidence_filters() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let pool = pg.pool_for_tests();

        let assignment = ingest_fact_atomic(pool, &permit, &fact_draft(), None).await?;
        let evidence = ingest_fact_atomic(pool, &permit, &fact_draft(), None).await?;

        let mut tx = pool.begin().await?;
        let created = write_goal(
            &mut tx,
            &owner,
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-v1".into(),
                title: "assigned".into(),
                state: GoalState::Active,
                request_id: "req-assign".into(),
                close_fact_t: None,
                assignment_t: Some(assignment.memory_id.into_inner()),
                dependency_t: vec![],
                evidence_t: vec![evidence.memory_id.into_inner()],
                wake_id: None,
                mint_write_act: false,
                write_act_t: None,
            },
        )
        .await?;
        tx.commit().await?;

        let mut by_assignment = QueryRequest::for_owner(owner);
        by_assignment.entity_kind = Some(EntityKind::Goal);
        by_assignment.goal_state = Some(GoalState::Active);
        by_assignment.assignment = Some(assignment.memory_id);
        let assigned = pg.query_memories(&by_assignment, &[]).await?;
        assert_eq!(assigned.goals.len(), 1);
        assert_eq!(assigned.goals[0].id.into_inner(), created.t);
        assert_eq!(assigned.goals[0].assignment, Some(assignment.memory_id));
        assert_eq!(assigned.goals[0].evidence, vec![evidence.memory_id]);

        let mut by_evidence = QueryRequest::for_owner(owner);
        by_evidence.entity_kind = Some(EntityKind::Goal);
        by_evidence.evidence_contains = Some(evidence.memory_id);
        let evidenced = pg.query_memories(&by_evidence, &[]).await?;
        assert_eq!(evidenced.goals.len(), 1);
        assert_eq!(evidenced.goals[0].id.into_inner(), created.t);

        let mut miss = QueryRequest::for_owner(owner);
        miss.entity_kind = Some(EntityKind::Goal);
        miss.assignment = Some(MemoryId::new(Uuid::now_v7()));
        let empty = pg.query_memories(&miss, &[]).await?;
        assert!(empty.goals.is_empty());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("goal query assignment/evidence failed");
}

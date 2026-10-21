//! GoalWrite + write-act Fact.
#![allow(
    clippy::missing_errors_doc,
    clippy::doc_markdown,
    clippy::too_many_lines
)]

use std::collections::BTreeSet;

use proxima_core::storage_ports::FactIngestPort;
use proxima_core::storage_ports::{MemoryReadPort, OwnerWritePermit};
use proxima_core::test_fixtures::owner_scope_for_tests;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::query::{EntityKind, GoalAssignmentFilter, QueryRequest};
use proxima_core::{AccessKind, EdgeEndpoint, MemoryId, OwnerId, SchemaId, SchemaVersion, UserId};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::test_fixtures::create_core_db;
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
        occurred_at: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: proxima_core::MemoryKind::Fact,
        request: None,
    }
}

#[tokio::test]
async fn goal_write_replay_terminal_and_write_act() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let pool = pg.pool_for_tests();
        let first_writer = proxima_core::WriterAttribution {
            by: Uuid::now_v7(),
            via: Some(Uuid::now_v7()),
        };
        let next_writer = proxima_core::WriterAttribution {
            by: Uuid::now_v7(),
            via: None,
        };

        let mut tx = pool.begin().await?;
        let created = write_goal(
            &mut tx,
            &owner,
            first_writer,
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-v1".into(),
                title: "ship v008".into(),
                text: None,
                state: GoalState::Active,
                request_id: "req-1".into(),
                close_fact_t: None,
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![],
                mint_write_act: true,
                write_act_t: None,
                parent_handle: None,
            },
            1,
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
        let act_writer: (Uuid, Option<Uuid>) =
            sqlx::query_as("SELECT \"by\", via FROM proxima_core.memory WHERE t = $1")
                .bind(created.write_act_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(act_writer, (first_writer.by, first_writer.via));

        let mut produced = fact_draft();
        produced.refs = vec![created.write_act_t.expect("write-act")];
        let produced_out = pg.ingest_fact_atomic(&permit, &produced, None).await?;
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
            next_writer,
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-v1".into(),
                title: "ignored".into(),
                text: None,
                state: GoalState::Active,
                request_id: "req-1".into(),
                close_fact_t: None,
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![],
                mint_write_act: true,
                write_act_t: None,
                parent_handle: None,
            },
            1,
        )
        .await?;
        tx.commit().await?;
        assert!(replay.replay);
        assert_eq!(replay.t, created.t);
        assert_eq!(replay.handle, created.handle);

        let close = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let mut tx = pool.begin().await?;
        let closed = write_goal(
            &mut tx,
            &owner,
            next_writer,
            &GoalWriteCommand {
                handle: Some(created.handle),
                schema_id: "core/task-v1".into(),
                title: "ship v008".into(),
                text: None,
                state: GoalState::Achieved,
                request_id: "req-close".into(),
                close_fact_t: Some(close.memory_id.into_inner()),
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![],
                mint_write_act: false,
                write_act_t: None,
                parent_handle: None,
            },
            1,
        )
        .await?;
        tx.commit().await?;
        assert_eq!(closed.handle, created.handle);
        assert_ne!(closed.t, created.t);
        for (id, expected) in [(created.t, first_writer), (closed.t, next_writer)] {
            let stored: (Uuid, Option<Uuid>) =
                sqlx::query_as("SELECT \"by\", via FROM proxima_core.goal WHERE t = $1")
                    .bind(id)
                    .fetch_one(pool)
                    .await?;
            assert_eq!(stored, (expected.by, expected.via));
        }
        for query in [
            sqlx::query("UPDATE proxima_core.goal SET \"by\" = $2 WHERE t = $1"),
            sqlx::query("UPDATE proxima_core.goal SET via = $2 WHERE t = $1"),
        ] {
            let error = query
                .bind(created.t)
                .bind(Uuid::now_v7())
                .execute(pool)
                .await
                .expect_err("Goal writer is immutable");
            assert!(error.to_string().contains("append-only"), "{error}");
        }

        let mut tx = pool.begin().await?;
        let err = write_goal(
            &mut tx,
            &owner,
            next_writer,
            &GoalWriteCommand {
                handle: Some(created.handle),
                schema_id: "core/task-v1".into(),
                title: "again".into(),
                text: None,
                state: GoalState::Active,
                request_id: "req-after-term".into(),
                close_fact_t: None,
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![],
                mint_write_act: false,
                write_act_t: None,
                parent_handle: None,
            },
            1,
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
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let pool = pg.pool_for_tests();

        let base = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let mut abstraction = fact_draft();
        abstraction.kind = proxima_core::MemoryKind::Abstraction;
        abstraction.additional_references =
            vec![EdgeEndpoint::memory(EntityKind::Fact, base.memory_id)];
        let abstraction = pg.ingest_fact_atomic(&permit, &abstraction, None).await?;
        let mut perspective = fact_draft();
        perspective.kind = proxima_core::MemoryKind::Perspective;
        perspective.additional_references = vec![EdgeEndpoint::memory(
            EntityKind::Abstraction,
            abstraction.memory_id,
        )];
        let assignment = pg.ingest_fact_atomic(&permit, &perspective, None).await?;
        let evidence = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;

        let mut tx = pool.begin().await?;
        let created = write_goal(
            &mut tx,
            &owner,
            proxima_storage_pg::test_fixtures::writer(),
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-v1".into(),
                title: "assigned".into(),
                text: None,
                state: GoalState::Active,
                request_id: "req-assign".into(),
                close_fact_t: None,
                assignment_t: Some(assignment.memory_id.into_inner()),
                dependency_t: vec![],
                evidence_t: vec![evidence.memory_id.into_inner()],
                mint_write_act: false,
                write_act_t: None,
                parent_handle: None,
            },
            1,
        )
        .await?;
        tx.commit().await?;

        let mut by_assignment = QueryRequest::readable();
        by_assignment.entity_kind = Some(EntityKind::Goal);
        by_assignment.goal_states = BTreeSet::from([GoalState::Active]);
        by_assignment.assignment = GoalAssignmentFilter::AssignedTo(vec![assignment.memory_id]);
        let assigned = pg
            .query_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &[owner],
                &by_assignment,
                &[],
            )
            .await?;
        assert_eq!(assigned.goals.len(), 1);
        assert_eq!(assigned.goals[0].id.into_inner(), created.t);
        assert_eq!(
            assigned.goals[0].writer,
            proxima_storage_pg::test_fixtures::writer()
        );
        assert_eq!(assigned.goals[0].assignment, Some(assignment.memory_id));
        assert_eq!(assigned.goals[0].evidence, vec![evidence.memory_id]);

        let mut by_evidence = QueryRequest::readable();
        by_evidence.entity_kind = Some(EntityKind::Goal);
        by_evidence.evidence_contains = Some(evidence.memory_id);
        let evidenced = pg
            .query_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &[owner],
                &by_evidence,
                &[],
            )
            .await?;
        assert_eq!(evidenced.goals.len(), 1);
        assert_eq!(evidenced.goals[0].id.into_inner(), created.t);

        // Several Perspectives select the goals assigned to any of them.
        let mut second = fact_draft();
        second.kind = proxima_core::MemoryKind::Perspective;
        second.additional_references = vec![EdgeEndpoint::memory(
            EntityKind::Abstraction,
            abstraction.memory_id,
        )];
        let second = pg.ingest_fact_atomic(&permit, &second, None).await?;
        let mut tx = pool.begin().await?;
        let second_goal = write_goal(
            &mut tx,
            &owner,
            proxima_storage_pg::test_fixtures::writer(),
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-v1".into(),
                title: "assigned elsewhere".into(),
                text: None,
                state: GoalState::Active,
                request_id: "req-assign-second".into(),
                close_fact_t: None,
                assignment_t: Some(second.memory_id.into_inner()),
                dependency_t: vec![],
                evidence_t: vec![],
                mint_write_act: false,
                write_act_t: None,
                parent_handle: None,
            },
            1,
        )
        .await?;
        tx.commit().await?;
        let mut by_either = QueryRequest::readable();
        by_either.entity_kind = Some(EntityKind::Goal);
        by_either.assignment =
            GoalAssignmentFilter::AssignedTo(vec![assignment.memory_id, second.memory_id]);
        let either = pg
            .query_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &[owner],
                &by_either,
                &[],
            )
            .await?;
        let mut found: Vec<Uuid> = either.goals.iter().map(|g| g.id.into_inner()).collect();
        found.sort_unstable();
        let mut expected = vec![created.t, second_goal.t];
        expected.sort_unstable();
        assert_eq!(found, expected);

        let mut miss = QueryRequest::readable();
        miss.entity_kind = Some(EntityKind::Goal);
        miss.assignment = GoalAssignmentFilter::AssignedTo(vec![MemoryId::new(Uuid::now_v7())]);
        let empty = pg
            .query_memories(Some(&owner_scope_for_tests(&[owner])), &[owner], &miss, &[])
            .await?;
        assert!(empty.goals.is_empty());

        // A goal with no assignment is what `Unassigned` keeps; an empty
        // `AssignedTo` keeps none, and `Any` keeps all.
        let mut tx = pool.begin().await?;
        let loose = write_goal(
            &mut tx,
            &owner,
            proxima_storage_pg::test_fixtures::writer(),
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-v1".into(),
                title: "unassigned".into(),
                text: None,
                state: GoalState::Active,
                request_id: "req-unassigned".into(),
                close_fact_t: None,
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![],
                mint_write_act: false,
                write_act_t: None,
                parent_handle: None,
            },
            1,
        )
        .await?;
        tx.commit().await?;
        let goals_under = |filter: GoalAssignmentFilter| {
            let mut req = QueryRequest::readable();
            req.entity_kind = Some(EntityKind::Goal);
            req.assignment = filter;
            let pg = &pg;
            async move {
                let page = pg
                    .query_memories(Some(&owner_scope_for_tests(&[owner])), &[owner], &req, &[])
                    .await?;
                let mut ids: Vec<Uuid> = page.goals.iter().map(|g| g.id.into_inner()).collect();
                ids.sort_unstable();
                Ok::<_, proxima_core::StorageError>(ids)
            }
        };
        assert_eq!(
            goals_under(GoalAssignmentFilter::Unassigned).await?,
            [loose.t]
        );
        assert!(
            goals_under(GoalAssignmentFilter::AssignedTo(Vec::new()))
                .await?
                .is_empty()
        );
        let mut all = vec![created.t, second_goal.t, loose.t];
        all.sort_unstable();
        assert_eq!(goals_under(GoalAssignmentFilter::Any).await?, all);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("goal query assignment/evidence failed");
}

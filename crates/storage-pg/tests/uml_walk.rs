//! UML §10 walk + query / ChangeHistory / publish.
#![allow(
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::used_underscore_binding
)]

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{AccessKind, EntityKind, OwnerRef, SchemaId, SchemaVersion, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use proxima_storage_pg::verbs::goal_timeseries::{GoalWriteCommand, write_goal};
use proxima_storage_pg::verbs::query_timeseries::{change_history, publish_head, query_heads};
use proxima_storage_pg::verbs::wake_timeseries::{
    WakeConfigDraft, WakeTriggerKind, insert_wake_config,
};
use uuid::Uuid;

fn fact(schema: &str, refs: Vec<Uuid>, origins: Vec<Uuid>, kind: &str) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new(schema.to_string()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        citation: None,
        derived_from: origins
            .into_iter()
            .map(|t| {
                proxima_core::EdgeEndpoint::memory(EntityKind::Fact, proxima_core::MemoryId::new(t))
            })
            .collect(),
        refs,
        blob_id: None,
        kind: kind.into(),
    }
}

#[tokio::test]
async fn uml_section_10_walk_query_history_publish() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();

        let visit =
            ingest_fact_atomic(pool, &permit, &fact("visit", vec![], vec![], "fact"), None).await?;
        let file = ingest_fact_atomic(
            pool,
            &permit,
            &fact("file", vec![visit.memory_id.into_inner()], vec![], "fact"),
            None,
        )
        .await?;
        let c1 = ingest_fact_atomic(
            pool,
            &permit,
            &fact(
                "chunk",
                vec![visit.memory_id.into_inner(), file.memory_id.into_inner()],
                vec![],
                "fact",
            ),
            None,
        )
        .await?;
        let c2 = ingest_fact_atomic(
            pool,
            &permit,
            &fact(
                "chunk",
                vec![visit.memory_id.into_inner(), file.memory_id.into_inner()],
                vec![],
                "fact",
            ),
            None,
        )
        .await?;
        let sum_draft = fact(
            "sum",
            vec![],
            vec![c1.memory_id.into_inner(), c2.memory_id.into_inner()],
            "abstraction",
        );
        let sum = ingest_fact_atomic(pool, &permit, &sum_draft, None).await?;
        let self_p = ingest_fact_atomic(
            pool,
            &permit,
            &fact(
                "self",
                vec![sum.memory_id.into_inner()],
                vec![],
                "perspective",
            ),
            None,
        )
        .await?;

        let mut tx = pool.begin().await?;
        let wake = insert_wake_config(
            &mut tx,
            &owner,
            &WakeConfigDraft {
                trigger_kind: WakeTriggerKind::FactSchema,
                trigger_schema_id: Some("visit".into()),
                trigger_t: None,
                tool_ids: vec!["core.remember".into()],
                prompt: "on visit".into(),
                hard_memory_t: vec![],
            },
        )
        .await?;
        let _g = write_goal(
            &mut tx,
            &owner,
            &GoalWriteCommand {
                handle: None,
                schema_id: "task".into(),
                title: "review".into(),
                state: GoalState::Active,
                request_id: "walk-g".into(),
                close_fact_t: None,
                assignment_t: Some(self_p.memory_id.into_inner()),
                dependency_t: vec![],
                evidence_t: vec![sum.memory_id.into_inner()],
                wake_id: Some(wake),
                mint_write_act: true,
            },
        )
        .await?;
        tx.commit().await?;

        let chunks = query_heads(pool, owner.stored_owner_id(), "chunk").await?;
        assert_eq!(chunks.len(), 2);

        let hist = change_history(pool, owner.stored_owner_id(), None, 50).await?;
        assert!(
            hist.iter()
                .any(|r| r.op == "append" && r.entity == "memory")
        );
        assert!(hist.iter().any(|r| r.entity == "goal"));

        let mut tx = pool.begin().await?;
        let (world_h, world_t) = publish_head(&mut tx, file.handle).await?;
        tx.commit().await?;
        assert_ne!(world_h, file.handle);
        let world_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(world_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(world_owner, Uuid::from_u128(1));
        let old_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(file.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(old_owner, owner.stored_owner_id());

        for dead in ["edges", "fact_entities", "change_event", "fact_receipts"] {
            let n: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM information_schema.tables
                  WHERE table_schema = 'proxima_core' AND table_name = $1",
            )
            .bind(dead)
            .fetch_one(pool)
            .await?;
            assert_eq!(n, 0, "{dead} must be absent");
        }

        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("UML §10 walk failed");
}

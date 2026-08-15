//! Slice 5: WakeConfig share / RESTRICT / match / fire.

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{AccessKind, OwnerRef, SchemaId, SchemaVersion, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use proxima_storage_pg::verbs::wake_timeseries::{
    WakeConfigDraft, WakeTriggerKind, fire_wake, insert_wake_config, matching_wake_ids,
    update_wake_prompt, write_armed_goal,
};
use uuid::Uuid;

fn fact_draft(schema: &str) -> FactWriteCommand {
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
        derived_from: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    }
}

#[tokio::test]
async fn goal_wake_share_restrict_match_fire() {
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

        let mut tx = pool.begin().await?;
        let wake_id = insert_wake_config(
            &mut tx,
            &owner,
            &WakeConfigDraft {
                trigger_kind: WakeTriggerKind::FactSchema,
                trigger_schema_id: Some("core/visit-v1".into()),
                trigger_t: None,
                tool_ids: vec!["core.remember".into()],
                prompt: "on visit".into(),
                hard_memory_t: vec![],
            },
        )
        .await?;
        let g1 = write_armed_goal(&mut tx, &owner, "one", "g1", wake_id).await?;
        let g2 = write_armed_goal(&mut tx, &owner, "two", "g2", wake_id).await?;
        tx.commit().await?;
        assert_ne!(g1, g2);

        let n: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.goal WHERE wake_id = $1",
        )
        .bind(wake_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(n, 2);

        let err = sqlx::query("DELETE FROM proxima_core.wake_config WHERE wake_id = $1")
            .bind(wake_id)
            .execute(pool)
            .await
            .expect_err("RESTRICT while goals name it");
        assert!(
            err.to_string().contains("restrict")
                || err.to_string().contains("violates")
                || err.to_string().contains("23503"),
            "got: {err}"
        );

        update_wake_prompt(pool, wake_id, "updated").await?;
        let prompt: String =
            sqlx::query_scalar("SELECT prompt FROM proxima_core.wake_config WHERE wake_id = $1")
                .bind(wake_id)
                .fetch_one(pool)
                .await?;
        assert_eq!(prompt, "updated");

        let visit = ingest_fact_atomic(pool, &permit, &fact_draft("core/visit-v1"), None).await?;
        let other = ingest_fact_atomic(pool, &permit, &fact_draft("core/other-v1"), None).await?;
        let matched = matching_wake_ids(pool, visit.memory_id.into_inner()).await?;
        assert_eq!(matched, vec![wake_id]);
        let unmatched = matching_wake_ids(pool, other.memory_id.into_inner()).await?;
        assert!(unmatched.is_empty());

        let mut tx = pool.begin().await?;
        let tr = fire_wake(&mut tx, &owner).await?;
        tx.commit().await?;
        let schema: String = sqlx::query_scalar(
            "SELECT h.schema_id FROM proxima_core.memory m
              JOIN proxima_core.memory_head h ON h.handle = m.handle
             WHERE m.t = $1",
        )
        .bind(tr)
        .fetch_one(pool)
        .await?;
        assert_eq!(schema, "core/write-act-v1");

        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("wake test failed");
}

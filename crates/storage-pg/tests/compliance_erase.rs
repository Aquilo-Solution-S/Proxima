//! Compliance owner erase against blank `0001_v008.sql`.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_core::compliance::{ComplianceEraseOutcome, ComplianceEraseTarget, EraseAuthorization};
use proxima_core::storage_ports::{ComplianceErasePort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{AccessKind, OwnerRef, SchemaId, SchemaVersion, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use uuid::Uuid;

fn draft(source: Option<(&str, &str)>) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new("core/test-fact-v1".to_string()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: source.map(|(s, _)| s.to_owned()),
        ingest_key: source.map(|(_, k)| k.to_owned()),
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

fn embed_literal() -> String {
    format!(
        "[{}]",
        std::iter::once("1")
            .chain(std::iter::repeat_n("0", 1023))
            .collect::<Vec<_>>()
            .join(",")
    )
}

#[tokio::test]
async fn erase_personal_owner_drops_memory_keys_and_embeddings() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();

        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let written = ingest_fact_atomic(pool, &permit, &draft(Some(("src", "k1"))), None).await?;
        let t = written.memory_id.into_inner();
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_id, model_id, embedding_version, vec, owner_id)
             VALUES ($1, 'test-embed', 1, $2::vector, $3)",
        )
        .bind(t)
        .bind(embed_literal())
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.embedding_heads
                (entity_id, model_id, embedding_version, owner_id)
             VALUES ($1, 'test-embed', 1, $2)",
        )
        .bind(t)
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;

        let other = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let other_permit = OwnerWritePermit::new_for_tests(other, AccessKind::Fact);
        let other_written =
            ingest_fact_atomic(pool, &other_permit, &draft(Some(("src", "k-other"))), None).await?;

        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop".into(),
        });
        let outcome = pg
            .erase_personal_owner_if_drop_verified(&auth, user, false, &[], &[], &[], &[])
            .await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert_eq!(counts.memories, 1);
        assert_eq!(counts.embeddings, 2);
        assert_eq!(counts.suppressed_keys, 0);
        assert_eq!(counts.receipts, 0);
        assert_eq!(counts.source_batches, 0);

        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(remaining, 0);
        let keys: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE ingest_key = 'k1'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(keys, 0);
        let embeddings: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.embeddings WHERE entity_id = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(embeddings, 0);

        let other_remaining: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(other_written.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(other_remaining, 1);
        let other_keys: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE ingest_key = 'k-other'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(other_keys, 1);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("compliance erase failed");
}

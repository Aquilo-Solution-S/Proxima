//! Integration tests for the `persist_mcp_call` verb.

use proxima_core::verbs::persist_mcp_call::{
    MCP_CALL_FACT_SCHEMA, MCP_CALL_IO_SCHEMA, McpCallLogInput,
};
use proxima_core::{Owner, Storage};
use proxima_storage_pg::verbs::persist_mcp_call::persist_mcp_call_atomic;

#[tokio::test]
async fn persist_writes_fact_inline_io_citation_and_change_event() {
    let Some((pg, db_name)) = crate::common::fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;

        let owner = crate::common::owner_fixture();
        let io_body = br#"{"input":{"q":"x"},"output":{"ok":true},"error":null}"#.to_vec();
        let input = sample_input(&owner, io_body.clone(), false, None);

        let outcome = persist_mcp_call_atomic(pg.pool(), &input).await?;

        assert!(!outcome.idempotent_replay);

        let n_facts: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM proxima_core.memories WHERE schema_id = $1")
                .bind(MCP_CALL_FACT_SCHEMA)
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(n_facts.0, 1);

        let fact: (String, String, bool, i32, Vec<u8>) = sqlx::query_as(
            "SELECT tool_name, actor_upn, ok, latency_ms, io_content_hash \
             FROM proxima_core.mcp_call_logged_v1 WHERE memory_id = $1",
        )
        .bind(outcome.fact_memory_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(fact.0, "core/search_memories");
        assert_eq!(fact.1, "agent@example.com");
        assert!(fact.2);
        assert_eq!(fact.3, 42);
        assert_eq!(fact.4.as_slice(), input.io_content_hash());

        let cited: (Vec<u8>, i64, bool) = sqlx::query_as(
            "SELECT body, byte_len, truncated FROM proxima_core.cited_mcp_call_io_v1 \
             WHERE cited_object_id = $1",
        )
        .bind(outcome.cited_object_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(cited.0, io_body);
        assert_eq!(usize::try_from(cited.1).unwrap(), input.io_body.len());
        assert!(!cited.2);

        let cm: (uuid::Uuid, uuid::Uuid) = sqlx::query_as(
            "SELECT memory_id, cited_object_id FROM proxima_core.citation_mappings \
             WHERE citation_mapping_id = $1",
        )
        .bind(outcome.citation_mapping_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(cm.0, outcome.fact_memory_id.into_inner());
        assert_eq!(cm.1, outcome.cited_object_id);

        let citation_sidecar: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM proxima_core.citation_mcp_call_io_v1 \
             WHERE citation_mapping_id = $1",
        )
        .bind(outcome.citation_mapping_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(citation_sidecar.0, 1);

        let change_event: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM proxima_core.change_event \
             WHERE entity_memory_id = $1 AND entity_kind = 'Fact'",
        )
        .bind(outcome.fact_memory_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(change_event.0, 1);

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("persist MCP call writes required rows");
}

#[tokio::test]
async fn distinct_calls_share_one_cited_object() {
    let Some((pg, db_name)) = crate::common::fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;

        let owner = crate::common::owner_fixture();
        let now = time::OffsetDateTime::now_utc();
        let io_body = br#"{"input":{"q":"same"},"output":{"ok":true},"error":null}"#.to_vec();
        let mut first_input = sample_input(&owner, io_body.clone(), false, None);
        first_input.observed_at = now;
        first_input.occurred_at = now;
        let mut second_input = sample_input(&owner, io_body, false, None);
        second_input.observed_at = now + time::Duration::milliseconds(5);
        second_input.occurred_at = now + time::Duration::milliseconds(5);

        let first = persist_mcp_call_atomic(pg.pool(), &first_input).await?;
        let second = persist_mcp_call_atomic(pg.pool(), &second_input).await?;

        assert!(!first.idempotent_replay);
        assert!(!second.idempotent_replay);
        assert_ne!(first.fact_memory_id, second.fact_memory_id);
        assert_eq!(first.cited_object_id, second.cited_object_id);

        let n_cited: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM proxima_core.cited_objects WHERE schema_id = $1")
                .bind(MCP_CALL_IO_SCHEMA)
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(n_cited.0, 1);

        let n_facts: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM proxima_core.memories WHERE schema_id = $1")
                .bind(MCP_CALL_FACT_SCHEMA)
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(n_facts.0, 2);

        let n_bodies: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM proxima_core.cited_mcp_call_io_v1")
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(n_bodies.0, 1);

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("distinct calls share one cited object");
}

#[tokio::test]
async fn identical_event_replays_idempotently() {
    let Some((pg, db_name)) = crate::common::fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;

        let owner = crate::common::owner_fixture();
        let input = sample_input(
            &owner,
            br#"{"input":{"q":"same"},"output":{"ok":true},"error":null}"#.to_vec(),
            false,
            None,
        );

        let first = persist_mcp_call_atomic(pg.pool(), &input).await?;
        let second = persist_mcp_call_atomic(pg.pool(), &input).await?;

        assert!(!first.idempotent_replay);
        assert!(second.idempotent_replay);
        assert_eq!(first.fact_memory_id, second.fact_memory_id);
        assert_eq!(first.cited_object_id, second.cited_object_id);
        assert_eq!(first.citation_mapping_id, second.citation_mapping_id);

        let n_facts: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM proxima_core.memories WHERE schema_id = $1")
                .bind(MCP_CALL_FACT_SCHEMA)
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(n_facts.0, 1);

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("identical event replays idempotently");
}

#[tokio::test]
async fn truncated_io_round_trips_original_byte_len() {
    let Some((pg, db_name)) = crate::common::fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;

        let owner = crate::common::owner_fixture();
        let io_body = br#"{"input":"large","output":"truncated"}"#.to_vec();
        let original_len = u64::try_from(io_body.len()).unwrap() + 4096;
        let input = sample_input(&owner, io_body.clone(), true, Some(original_len));

        let outcome = persist_mcp_call_atomic(pg.pool(), &input).await?;

        let cited: (Vec<u8>, i64, bool) = sqlx::query_as(
            "SELECT body, byte_len, truncated FROM proxima_core.cited_mcp_call_io_v1 \
             WHERE cited_object_id = $1",
        )
        .bind(outcome.cited_object_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(cited.0, io_body);
        assert_eq!(u64::try_from(cited.1).unwrap(), original_len);
        assert!(cited.2);

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("truncated IO metadata round-trips");
}

#[tokio::test]
async fn storage_trait_exposes_mcp_call_persist() {
    let Some((pg, db_name)) = crate::common::fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;

        let owner = crate::common::owner_fixture();
        let input = sample_input(
            &owner,
            br#"{"input":{},"output":{"via":"trait"},"error":null}"#.to_vec(),
            false,
            None,
        );

        let outcome = pg.persist_mcp_call_atomic(&input).await?;

        assert!(!outcome.idempotent_replay);
        assert_ne!(outcome.fact_memory_id.into_inner(), uuid::Uuid::nil());

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("Storage trait exposes MCP call persist");
}

fn sample_input(
    owner: &Owner,
    io_body: Vec<u8>,
    io_truncated: bool,
    original_len: Option<u64>,
) -> McpCallLogInput {
    let now = time::OffsetDateTime::now_utc();
    let io_byte_len_original =
        original_len.unwrap_or_else(|| u64::try_from(io_body.len()).unwrap());
    McpCallLogInput {
        owner: owner.clone(),
        actor_oid: "00000000-0000-0000-0000-000000000001".into(),
        actor_upn: "agent@example.com".into(),
        tool_name: "core/search_memories".into(),
        ok: true,
        error: None,
        latency_ms: 42,
        io_body,
        io_byte_len_original,
        io_truncated,
        observed_at: now,
        occurred_at: now,
    }
}

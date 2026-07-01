//! Integration tests for the `read_mcp_call_history` verb.

use proxima_core::McpCallReadPort;
use proxima_core::verbs::mcp_call_history::McpCallHistoryRequest;
use proxima_core::verbs::persist_mcp_call::McpCallLogInput;
use proxima_core::{Owner, OwnerRef, UserId};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::persist_mcp_call::persist_mcp_call_atomic;
use uuid::Uuid;

#[tokio::test]
async fn read_mcp_call_history_returns_owner_scoped_newest_first() {
    let (pg, db_name) = crate::common::fresh_pg().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;

        let owner1 = crate::common::owner_fixture();
        let owner2 = OwnerRef::Personal(UserId::new(Uuid::now_v7()));

        seed_history_fixture(&pg, &owner1, &owner2).await?;

        let oid_a = pg
            .read_mcp_call_history(&history_req(&owner1, Some("oidA"), 999))
            .await?;
        assert_eq!(oid_a.calls.len(), 2);
        assert_eq!(oid_a.calls[0].tool_name, "core/a-new");
        assert_eq!(oid_a.calls[1].tool_name, "core/a-old");
        assert!(oid_a.calls[0].at >= oid_a.calls[1].at);
        let latest_body = oid_a.calls[0]
            .io_body
            .as_ref()
            .expect("inline MCP I/O body is present");
        assert!(
            latest_body
                .windows(b"roundtrip-token".len())
                .any(|window| { window == b"roundtrip-token" })
        );

        let all_owner1 = pg
            .read_mcp_call_history(&history_req(&owner1, None, 999))
            .await?;
        assert_eq!(all_owner1.calls.len(), 3);
        assert!(
            all_owner1
                .calls
                .iter()
                .all(|call| call.tool_name != "core/foreign")
        );

        let limited = pg
            .read_mcp_call_history(&history_req(&owner1, None, 1))
            .await?;
        assert_eq!(limited.calls.len(), 1);
        assert_eq!(limited.calls[0].tool_name, "core/a-new");

        let all_owner2 = pg
            .read_mcp_call_history(&history_req(&owner2, None, 999))
            .await?;
        assert_eq!(all_owner2.calls.len(), 1);
        assert_eq!(all_owner2.calls[0].tool_name, "core/foreign");

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("read_mcp_call_history_returns_owner_scoped_newest_first failed");
}

async fn seed_history_fixture(
    pg: &PgStorage,
    owner1: &Owner,
    owner2: &Owner,
) -> Result<(), proxima_core::StorageError> {
    let base = time::OffsetDateTime::now_utc();
    let seeds = [
        (
            owner1,
            "oidA",
            "core/a-old",
            br#"{"input":{"token":"oidA-old"},"output":{"ok":true}}"#.as_slice(),
            0,
        ),
        (
            owner1,
            "oidB",
            "core/b",
            br#"{"input":{"token":"oidB"},"output":{"ok":true}}"#.as_slice(),
            1,
        ),
        (
            owner1,
            "oidA",
            "core/a-new",
            br#"{"input":{"token":"roundtrip-token"},"output":{"ok":true}}"#.as_slice(),
            2,
        ),
        (
            owner2,
            "oidA",
            "core/foreign",
            br#"{"input":{"token":"foreign"},"output":{"ok":true}}"#.as_slice(),
            3,
        ),
    ];

    for (owner, actor_oid, tool_name, io_body, offset_ms) in seeds {
        seed_call(
            pg.pool_for_tests(),
            owner,
            actor_oid,
            tool_name,
            io_body.to_vec(),
            base + time::Duration::milliseconds(offset_ms),
        )
        .await?;
    }
    Ok(())
}

fn history_req(owner: &Owner, actor_oid: Option<&str>, limit: u32) -> McpCallHistoryRequest {
    McpCallHistoryRequest {
        owner: *owner,
        actor_oid: actor_oid.map(str::to_string),
        limit,
    }
}

async fn seed_call(
    pool: &sqlx::PgPool,
    owner: &Owner,
    actor_oid: &str,
    tool_name: &str,
    io_body: Vec<u8>,
    observed_at: time::OffsetDateTime,
) -> Result<(), proxima_core::StorageError> {
    let input = McpCallLogInput {
        owner: *owner,
        actor_oid: actor_oid.into(),
        actor_upn: format!("{actor_oid}@example.com"),
        tool_name: tool_name.into(),
        ok: true,
        error: None,
        latency_ms: 42,
        io_byte_len_original: u64::try_from(io_body.len()).unwrap_or(u64::MAX),
        io_body,
        io_truncated: false,
        observed_at,
        occurred_at: observed_at,
    };
    persist_mcp_call_atomic(pool, &input).await?;
    Ok(())
}

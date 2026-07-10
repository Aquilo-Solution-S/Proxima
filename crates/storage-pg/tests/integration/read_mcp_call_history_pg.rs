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

#[tokio::test]
async fn include_body_false_omits_body_and_true_hydrates() {
    let (pg, db_name) = crate::common::fresh_pg().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = crate::common::owner_fixture();
        seed_call(
            pg.pool_for_tests(),
            &owner,
            "oidA",
            "core/only",
            br#"{"input":{"token":"body-token"},"output":{"ok":true}}"#.to_vec(),
            time::OffsetDateTime::now_utc(),
        )
        .await?;

        let omitted = pg
            .read_mcp_call_history(&McpCallHistoryRequest {
                owner,
                actor_oid: None,
                limit: 10,
                include_body: false,
                before: None,
            })
            .await?;
        assert_eq!(omitted.calls.len(), 1);
        assert_eq!(omitted.calls[0].tool_name, "core/only");
        assert!(
            omitted.calls[0].io_body.is_none(),
            "include_body=false must omit the inline I/O body (default)"
        );

        let hydrated = pg
            .read_mcp_call_history(&McpCallHistoryRequest {
                owner,
                actor_oid: None,
                limit: 10,
                include_body: true,
                before: None,
            })
            .await?;
        assert_eq!(hydrated.calls.len(), 1);
        let body = hydrated.calls[0]
            .io_body
            .as_ref()
            .expect("include_body=true hydrates the inline I/O body");
        assert!(
            body.windows(b"body-token".len())
                .any(|window| window == b"body-token"),
            "hydrated body carries the seeded I/O bytes"
        );

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("include_body gating test failed");
}

#[tokio::test]
async fn cursor_pages_strictly_older_rows() {
    let (pg, db_name) = crate::common::fresh_pg().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = crate::common::owner_fixture();
        let base = time::OffsetDateTime::now_utc();
        // Increasing timestamps: c0 (oldest) .. c2 (newest).
        for (name, offset_ms) in [("core/c0", 0i64), ("core/c1", 1i64), ("core/c2", 2i64)] {
            seed_call(
                pg.pool_for_tests(),
                &owner,
                "oidA",
                name,
                br#"{"input":{},"output":{}}"#.to_vec(),
                base + time::Duration::milliseconds(offset_ms),
            )
            .await?;
        }

        // Page 1: newest two.
        let page1 = pg
            .read_mcp_call_history(&McpCallHistoryRequest {
                owner,
                actor_oid: None,
                limit: 2,
                include_body: false,
                before: None,
            })
            .await?;
        assert_eq!(page1.calls.len(), 2);
        assert_eq!(page1.calls[0].tool_name, "core/c2");
        assert_eq!(page1.calls[1].tool_name, "core/c1");

        // Page 2: strictly older than page 1's last row => only c0, no repeats.
        let cursor = &page1.calls[1];
        let page2 = pg
            .read_mcp_call_history(&McpCallHistoryRequest {
                owner,
                actor_oid: None,
                limit: 2,
                include_body: false,
                before: Some((cursor.at, cursor.memory_id.into_inner())),
            })
            .await?;
        assert_eq!(page2.calls.len(), 1, "cursor pages the remaining older row");
        assert_eq!(page2.calls[0].tool_name, "core/c0");
        assert!(
            page2
                .calls
                .iter()
                .all(|call| call.tool_name != "core/c1" && call.tool_name != "core/c2"),
            "cursor must not repeat page-1 rows"
        );

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("cursor paging test failed");
}

async fn seed_history_fixture(
    pg: &PgStorage,
    owner1: &Owner,
    owner2: &Owner,
) -> Result<(), Box<dyn std::error::Error>> {
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

// `include_body: true` here mirrors the prior default (body always
// hydrated); the new default is `false`, so this helper opts back in to keep
// the body-present assertions above meaningful. Body-omission and cursor
// paging get dedicated tests below.
fn history_req(owner: &Owner, actor_oid: Option<&str>, limit: u32) -> McpCallHistoryRequest {
    McpCallHistoryRequest {
        owner: *owner,
        actor_oid: actor_oid.map(str::to_string),
        limit,
        include_body: true,
        before: None,
    }
}

async fn seed_call(
    pool: &sqlx::PgPool,
    owner: &Owner,
    actor_oid: &str,
    tool_name: &str,
    io_body: Vec<u8>,
    observed_at: time::OffsetDateTime,
) -> Result<(), Box<dyn std::error::Error>> {
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
    let permit = crate::common::owner_write_permit(owner, proxima_core::AccessKind::Fact).await?;
    persist_mcp_call_atomic(pool, &permit, &input).await?;
    Ok(())
}

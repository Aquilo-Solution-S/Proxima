//! `persist_mcp_call` lands a call the history read can see.
//!
//! The write is the governed typed-Fact path, so the admission row declares
//! `proxima_core.mcp_call_logged_v1`, the typed row lands through the frozen
//! sidecar registry, and `read_mcp_call_history` — which reads only that
//! table — answers. A private storage verb used to land the memory row with
//! an empty `sidecar_tables` stamp and no typed row, which left every call
//! it logged invisible to the read that exists for it.

use proxima::flavor::{FlavorBundle, NamedMigrator};
use proxima::{AppInfo, AuthPath, AuthzContext, FlavorApp, Proxima, ToolScope, company_owner};
use proxima_core::verbs::mcp_call_history::{McpCallHistoryRequest, McpCallRecord};
use proxima_core::{Engine, McpCallLogInput, Owner, ProtocolError, Role, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::core_pg_sidecars;
use uuid::Uuid;

const LOGGED_TABLE: &str = "proxima_core.mcp_call_logged_v1";
const TOOL: &str = "core_remember";

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
            id: "mcp-call-log-test",
            title: "mcp-call-log-test",
            version: "0",
        }
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

fn call(owner: &Owner, at: time::OffsetDateTime) -> McpCallLogInput {
    McpCallLogInput {
        owner: *owner,
        actor_oid: "oid-1".into(),
        actor_upn: "agent@example.test".into(),
        tool_name: TOOL.into(),
        ok: true,
        error: None,
        latency_ms: 42,
        io_body: br#"{"ok":true}"#.to_vec(),
        io_byte_len_original: 11,
        io_truncated: false,
        observed_at: at,
        occurred_at: at,
    }
}

async fn history(
    engine: &Engine,
    authz: &AuthzContext,
    owner: Owner,
) -> Result<Vec<McpCallRecord>, ProtocolError> {
    let response = proxima::read_mcp_call_history(
        engine,
        authz,
        &McpCallHistoryRequest {
            owner,
            actor_oid: None,
            limit: 10,
            include_body: false,
            before: None,
        },
    )
    .await?;
    Ok(response.calls)
}

/// Three claims. The call is readable through the history read; the same
/// call replays as a no-op while the same call at a later time is a new
/// row; and the rows it left pass the declaration integrity check.
#[tokio::test]
async fn a_persisted_mcp_call_is_readable_through_the_history_read() {
    let db_name = unique_db_name("proxima_mcp_call_log");
    create_db(&db_name).await.expect("PG required");
    let db_url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(db_url)
            .owner(owner)
            .allow_insecure_single_owner()
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let engine = built.engine();
        let authz = admin_authz_for(owner);
        let t0 = time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(20_000);

        let first = proxima::log_mcp_call(&engine, &authz, call(&owner, t0)).await?;
        assert!(!first.idempotent_replay, "{first:?}");
        assert!(
            first.cited_object_id.is_some(),
            "the I/O bytes land as a cited object: {first:?}"
        );

        // The read that exists for this write sees it.
        let calls = history(&engine, &authz, owner).await?;
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].memory_id, first.fact_memory_id);
        assert_eq!(calls[0].tool_name, TOOL);
        assert!(calls[0].ok);

        // Because the admission row declares the sidecar it carries: this
        // stamp was `{}` on the path this replaced.
        let stamp: Vec<String> =
            sqlx::query_scalar("SELECT sidecar_tables FROM proxima_core.memory WHERE t = $1")
                .bind(first.fact_memory_id.into_inner())
                .fetch_one(built.pool_for_tests())
                .await?;
        assert!(
            stamp.iter().any(|table| table == LOGGED_TABLE),
            "the admission row declares the logged-call sidecar: {stamp:?}"
        );

        // Whole-verb replay: the same call is the same Fact.
        let again = proxima::log_mcp_call(&engine, &authz, call(&owner, t0)).await?;
        assert!(again.idempotent_replay, "{again:?}");
        assert_eq!(again.fact_memory_id, first.fact_memory_id);

        // The same call at a later time is a new Fact — the timestamps are
        // part of the receipt — sharing the content-addressed I/O object.
        let later = proxima::log_mcp_call(
            &engine,
            &authz,
            call(&owner, t0 + time::Duration::seconds(1)),
        )
        .await?;
        assert!(!later.idempotent_replay, "{later:?}");
        assert_ne!(later.fact_memory_id, first.fact_memory_id);
        assert_eq!(
            later.cited_object_id, first.cited_object_id,
            "identical I/O bytes under one owner share one cited object"
        );
        let calls = history(&engine, &authz, owner).await?;
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[0].memory_id, later.fact_memory_id, "newest first");

        // A stranger to this owner reads nothing.
        let stranger = admin_authz_for(company_owner(Uuid::now_v7()));
        let err = history(&engine, &stranger, owner)
            .await
            .expect_err("a foreign owner's history is not served");
        assert_eq!(err.code, proxima_core::ErrorCode::Forbidden, "{err:?}");

        // Nothing this path wrote is undeclared or unprojected.
        core_pg_sidecars()
            .integrity_check(built.pool_for_tests())
            .await
            .map_err(|err| format!("logging left declaration drift: {err}"))?;

        built.shutdown();
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("mcp call log pg test failed");
}

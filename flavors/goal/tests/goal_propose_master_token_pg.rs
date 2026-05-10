//! M0 acceptance: a master-token-driven `goal_propose` creates a
//! core/inspires edge from the new Goal to the per-token shell-author's
//! Self-Perspective. This is the v0.1.0 acceptance criterion for M0
//! per docs/superpowers/specs/2026-05-10-spinning-wheel-proof-roadmap.md
//! §M0.

#[allow(dead_code)]
mod common;

use common::{drop_db, migrated, owner_fixture};
use proxima_core::auth::NoAuth;
use proxima_core::mcp::{HandleTable, McpAuthorContext, McpToolCtx};
use proxima_core::storage::Storage;
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{Engine, FlavorRegistry, McpTool};
use proxima_flavor_goal::tools::propose::{ProposeArgs, ProposeTool};
use proxima_flavor_goal::tools::util::{GoalPayloadInput, SimpleTextGoalBody};
use proxima_mcp_server::{DevMcpServer, McpAuthContext, McpAuthStore};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

async fn mcp_server_for_owner(
    pg: &proxima_storage_pg::PgStorage,
    owner: &proxima_core::Owner,
) -> Result<(DevMcpServer, McpAuthContext, Uuid), Box<dyn std::error::Error>> {
    let mut registry = FlavorRegistry::new();
    proxima_flavor_goal::register(&mut registry);
    let server_registry = Arc::new(registry.freeze());

    let mut engine_registry = FlavorRegistry::new();
    proxima_flavor_goal::register(&mut engine_registry);
    let auth = NoAuth::new(owner.principal.clone(), owner.clone());
    let engine = Arc::new(
        Engine::new(engine_registry.freeze(), MemoryStore::new(), Box::new(auth))
            .with_storage(Arc::new(pg.clone())),
    );
    let server = DevMcpServer::from_pool(pg.pool().clone(), owner.clone(), server_registry)
        .with_engine(engine);
    let auth_store = McpAuthStore::new(Arc::new(
        proxima_core::wake::token_store::WakeTokenStore::new(Duration::from_mins(5)),
    ));
    let token = Uuid::now_v7();
    auth_store
        .replace_local_master_token(token, owner.clone())
        .await;
    let auth_ctx = auth_store.resolve(token).await.expect("token resolves");
    Ok((server, auth_ctx, token))
}

async fn propose_accept_via_mcp(
    server: &DevMcpServer,
    auth_ctx: &McpAuthContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let author = McpAuthorContext {
        model_id: "test-model".into(),
        client_name: "test".into(),
        client_version: "1".into(),
        caller_self_perspective: None,
    };
    let proposed = server
        .call_tool(
            "proxima-goal/goal_propose",
            serde_json::json!({
                "payload": {
                    "schema_id": "proxima-goal/simple-text-v1",
                    "body": {
                        "title": "mcp lifecycle",
                        "text": "mcp lifecycle"
                    }
                },
                "evidence": [],
                "idempotency_key": "mcp-lifecycle-propose"
            }),
            author.clone(),
            Some(auth_ctx.clone()),
        )
        .await?;
    let proposal_handle = proposed
        .get("handle")
        .and_then(serde_json::Value::as_str)
        .expect("proposal handle")
        .to_string();

    server
        .call_tool(
            "proxima-goal/goal_accept",
            serde_json::json!({
                "proposal": proposal_handle,
                "idempotency_key": "mcp-lifecycle-accept"
            }),
            author,
            Some(auth_ctx.clone()),
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn master_token_propose_creates_inspires_edge_to_per_token_self_perspective()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let owner = owner_fixture();
        let token = Uuid::now_v7();

        // Mint the per-token shell-author identity directly via the
        // storage verb. In production this is done by the MCP server's
        // call_tool ensure step (Task 6); here we simulate the
        // post-ensure state by populating caller_self_perspective with
        // the resulting Self-Perspective.
        let identity = pg.ensure_master_token_personality(&owner, token).await?;

        // Build an McpToolCtx mirroring the post-ensure state for a
        // master-token call. The ProposeTool reads caller_self_perspective
        // and uses it as the target of the core/inspires edge — see
        // flavors/goal/src/tools/propose.rs.
        //
        // We can't reuse the existing common::ctx() because it doesn't
        // populate caller_self_perspective; we want an explicit setup
        // that matches what M0 Task 6 produces in production.
        let mut registry = FlavorRegistry::new();
        proxima_flavor_goal::register(&mut registry);
        let ctx = McpToolCtx {
            pool: pg.pool().clone(),
            owner: owner.clone(),
            handles: Arc::new(HandleTable::new()),
            registry: Arc::new(registry.freeze()),
            author: McpAuthorContext {
                model_id: "test".into(),
                client_name: "test".into(),
                client_version: "0".into(),
                caller_self_perspective: Some(identity.self_perspective_memory_id),
            },
            caller_self_perspective: Some(identity.self_perspective_memory_id),
            master_token_id: Some(token),
            engine: None,
        };

        let outcome = ProposeTool::call(
            ctx.clone(),
            ProposeArgs {
                payload: GoalPayloadInput::SimpleText(SimpleTextGoalBody {
                    title: "rename unwrap helper".into(),
                    text: "Rename `unwrap` in tauri-client.ts to `unwrapTauri`.".into(),
                }),
                evidence: vec![],
                idempotency_key: Some("m0-acceptance-1".into()),
            },
        )
        .await?;

        // Acceptance: ProposeTool returns an inspires_edge_handle.
        let inspires_edge_handle = outcome
            .inspires_edge_handle
            .as_deref()
            .expect("master-token propose should create an inspires edge");
        assert!(!inspires_edge_handle.is_empty());

        // Resolve the edge handle and read the row directly to verify
        // the target_memory_id matches the per-token Self-Perspective.
        let edge_id = ctx
            .handles
            .resolve_edge(inspires_edge_handle)
            .expect("inspires edge handle resolves")
            .into_inner();

        let row: (String, uuid::Uuid, uuid::Uuid, String, Option<uuid::Uuid>) = sqlx::query_as(
            "SELECT relation, source_goal_id, target_memory_id, authorship_kind,
                    authorship_owner_memory_id
               FROM proxima_core.edges
              WHERE edge_id = $1",
        )
        .bind(edge_id)
        .fetch_one(pg.pool())
        .await?;

        assert_eq!(row.0, "core/inspires");
        assert_eq!(
            row.2,
            identity.self_perspective_memory_id.into_inner(),
            "inspires edge should target the per-token shell-author Self-Perspective",
        );
        assert_eq!(row.3, "ExternalAgent");
        assert_eq!(
            row.4,
            Some(identity.self_perspective_memory_id.into_inner()),
        );

        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;

    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn mcp_propose_accept_emits_lifecycle_facts_authored_by_master_token_self()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = migrated().await else {
        return Ok(());
    };

    let result = async {
        let owner = owner_fixture();
        let (server, auth_ctx, token) = mcp_server_for_owner(&pg, &owner).await?;
        propose_accept_via_mcp(&server, &auth_ctx).await?;

        let proposal_id: uuid::Uuid = sqlx::query_scalar(
            "SELECT goal_id FROM proxima_core.goals
              WHERE title = 'mcp lifecycle' AND state = 'Proposed'",
        )
        .fetch_one(pg.pool())
        .await?;
        let active_id: uuid::Uuid = sqlx::query_scalar(
            "SELECT goal_id FROM proxima_core.goals
              WHERE title = 'mcp lifecycle' AND state = 'Active'
                AND supersedes = $1",
        )
        .bind(proposal_id)
        .fetch_one(pg.pool())
        .await?;
        let proposed_fact_id: uuid::Uuid = sqlx::query_scalar(
            "SELECT memory_id FROM proxima_goal.goal_proposed_v1
              WHERE goal_id = $1",
        )
        .bind(proposal_id)
        .fetch_one(pg.pool())
        .await?;
        let activated_fact_id: uuid::Uuid = sqlx::query_scalar(
            "SELECT memory_id FROM proxima_goal.goal_activated_v1
              WHERE goal_id = $1",
        )
        .bind(active_id)
        .fetch_one(pg.pool())
        .await?;

        let identity = pg.ensure_master_token_personality(&owner, token).await?;
        let authored_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.edges
              WHERE relation = 'core/authored'
                AND source_memory_id = $1
                AND target_memory_id = ANY($2::uuid[])",
        )
        .bind(identity.self_perspective_memory_id.into_inner())
        .bind(vec![proposed_fact_id, activated_fact_id])
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(authored_count, 2);

        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;

    drop_db(&db_name).await?;
    result
}

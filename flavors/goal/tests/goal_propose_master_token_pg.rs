//! M0 acceptance: a master-token-driven `goal_propose` creates a
//! core/inspires edge from the new Goal to the per-token shell-author's
//! Self-Perspective. This is the v0.1.0 acceptance criterion for M0
//! per docs/superpowers/specs/2026-05-10-spinning-wheel-proof-roadmap.md
//! §M0.

#[allow(dead_code)]
mod common;

use common::{drop_db, migrated, owner_fixture};
use proxima_core::mcp::{HandleTable, McpAuthorContext, McpToolCtx};
use proxima_core::storage::Storage;
use proxima_core::{FlavorRegistry, McpTool};
use proxima_flavor_goal::tools::propose::{ProposeArgs, ProposeTool};
use proxima_flavor_goal::tools::util::{GoalPayloadInput, SimpleTextGoalBody};
use std::sync::Arc;
use uuid::Uuid;

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

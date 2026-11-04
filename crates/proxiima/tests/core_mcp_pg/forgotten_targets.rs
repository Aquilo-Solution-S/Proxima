//! #488 S12: a pin or lineage edge to a memory the reader could read but
//! which is forgotten lists as forgotten, and lineage stops there.

use proxiima::{CoreMcpErrorKind, Proxiima};
use proxiima_core::{OwnerId, Role, UserId};
use uuid::Uuid;

use super::split_core_db::clone_split_core_db;
use super::{
    AgentMemoryApp, call_test_model_tool, read_test_model_resource,
    server_issued_group_space_selector, space_authz,
};

/// A ← B ← C, and C also rests on a live Abstraction E over a Fact D, so
/// forgetting B leaves C grounded. The owner then sees B as forgotten, from C's pins and from C's
/// lineage, and the walk does not reach A.
#[tokio::test]
async fn the_owner_sees_a_forgotten_source_as_forgotten_and_lineage_stops_there() {
    let split_db = clone_split_core_db("proxiima_forgotten_targets")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxiima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxiima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        let call = |tool: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), owner, tool, args)
        };
        let handle = |made: serde_json::Value| made["handle"].as_str().expect("handle").to_owned();
        let remember = |title: &'static str| {
            call(
                "core_remember",
                serde_json::json!({"title": title, "body": title, "tags": []}),
            )
        };
        let derive = |title: &'static str, sources: Vec<String>| {
            call(
                "core_derive",
                serde_json::json!({
                    "kind": "Abstraction",
                    "title": title,
                    "body": title,
                    "source_handles": sources,
                    "model_id": "test-model"
                }),
            )
        };
        let root = handle(remember("root fact").await?);
        let other = handle(remember("other fact").await?);
        let middle = handle(derive("middle", vec![root.clone()]).await?);
        let side = handle(derive("side", vec![other.clone()]).await?);
        let top = handle(derive("top", vec![middle.clone(), side.clone()]).await?);

        call("core_forget", serde_json::json!({"memory": middle})).await?;

        let read = read_test_model_resource(
            &tools,
            authz.clone(),
            owner,
            &format!("proxiima://memory/{top}?expand_neighbors=true"),
        )
        .await?;
        let targets: Vec<&str> = read["neighbor_edges"]
            .as_array()
            .expect("neighbor edges")
            .iter()
            .filter(|edge| edge["source"] == top.as_str())
            .filter_map(|edge| edge["target"].as_str())
            .collect();
        assert!(targets.contains(&"forgotten target"), "{read}");
        assert!(targets.contains(&side.as_str()), "{read}");
        assert!(!targets.contains(&"redacted target"), "{read}");

        let lineage = read_test_model_resource(
            &tools,
            authz.clone(),
            owner,
            &format!("proxiima://memory/{top}/lineage?direction=ancestors&depth=3"),
        )
        .await?;
        let nodes = lineage["nodes"].as_array().expect("nodes");
        let node = |memory: &str| nodes.iter().find(|node| node["memory"] == memory);
        assert_eq!(
            node(&middle).map(|node| node["status"].clone()),
            Some("forgotten".into()),
            "{lineage}"
        );
        assert!(node(&root).is_none(), "the walk stops at B: {lineage}");
        assert!(node(&side).is_some(), "{lineage}");
        let edge_targets: Vec<&str> = lineage["edges"]
            .as_array()
            .expect("edges")
            .iter()
            .filter_map(|edge| edge["target"].as_str())
            .collect();
        assert!(edge_targets.contains(&"forgotten target"), "{lineage}");
        assert!(!edge_targets.contains(&"redacted target"), "{lineage}");
        Ok(())
    }
    .await;
    result.expect("forgotten targets");
}

/// #488 S13: a forgotten memory still grounds what was derived from it.
/// Forgetting the only source of a Perspective (an Abstraction) is no
/// longer refused, nor is forgetting the only source of that Abstraction
/// (a Fact); both derived memories stay readable and their lineage names
/// the source forgotten.
#[tokio::test]
async fn forgetting_the_last_source_is_allowed_and_the_derived_memory_stays_readable() {
    let split_db = clone_split_core_db("proxiima_forget_last_source")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxiima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxiima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        let call = |tool: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), owner, tool, args)
        };
        let handle = |made: serde_json::Value| made["handle"].as_str().expect("handle").to_owned();
        let fact = handle(
            call(
                "core_remember",
                serde_json::json!({"title": "observed", "body": "observed", "tags": []}),
            )
            .await?,
        );
        let derive = |kind: &'static str, title: &'static str, source: String| {
            call(
                "core_derive",
                serde_json::json!({
                    "kind": kind,
                    "title": title,
                    "body": title,
                    "source_handles": [source],
                    "model_id": "test-model"
                }),
            )
        };
        let abstraction = handle(derive("Abstraction", "concluded", fact.clone()).await?);
        let perspective = handle(derive("Perspective", "viewed", abstraction.clone()).await?);

        call("core_forget", serde_json::json!({"memory": abstraction})).await?;
        call("core_forget", serde_json::json!({"memory": fact})).await?;

        let read = read_test_model_resource(
            &tools,
            authz.clone(),
            owner,
            &format!("proxiima://memory/{perspective}"),
        )
        .await?;
        assert_eq!(read["handle"], perspective.as_str(), "{read}");
        let lineage = read_test_model_resource(
            &tools,
            authz.clone(),
            owner,
            &format!("proxiima://memory/{perspective}/lineage?direction=ancestors&depth=3"),
        )
        .await?;
        let status = lineage["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|node| node["memory"] == abstraction.as_str())
            .map(|node| node["status"].clone());
        assert_eq!(status, Some("forgotten".into()), "{lineage}");
        Ok(())
    }
    .await;
    result.expect("forget last source");
}

/// #488 S14: `core_restore` brings a forgotten memory back. Restoring a live
/// memory is a no-op success; the output keeps forget's shape. `core_forget`
/// takes no `action`: restore is its own tool with its own effect.
#[tokio::test]
async fn the_owner_restores_a_forgotten_memory_through_core_restore() {
    let split_db = clone_split_core_db("proxiima_restore_forgotten")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxiima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxiima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        let call = |tool: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), owner, tool, args)
        };
        let read = |memory: String| {
            let (tools, authz) = (&tools, authz.clone());
            async move {
                let uri = format!("proxiima://memory/{memory}");
                read_test_model_resource(tools, authz, owner, &uri).await
            }
        };
        let memory = call(
            "core_remember",
            serde_json::json!({"title": "kept aside", "body": "kept aside", "tags": []}),
        )
        .await?["handle"]
            .as_str()
            .expect("handle")
            .to_owned();

        call("core_forget", serde_json::json!({"memory": memory})).await?;
        assert!(
            read(memory.clone()).await.is_err(),
            "forgotten reads absent"
        );
        call(
            "core_forget",
            serde_json::json!({"memory": memory, "action": "restore"}),
        )
        .await
        .expect_err("core_forget has no action");

        let restored = call("core_restore", serde_json::json!({"memory": memory})).await?;
        assert_eq!(
            restored,
            serde_json::json!({"ok": true, "memory": memory}),
            "the output is unchanged"
        );
        let back = read(memory.clone()).await?;
        assert_eq!(back["handle"], memory.as_str(), "{back}");

        let again = call("core_restore", serde_json::json!({"memory": memory})).await?;
        assert_eq!(again["ok"], true, "restoring a live memory is a no-op");
        assert!(read(memory.clone()).await.is_ok());
        Ok(())
    }
    .await;
    result.expect("restore through core_restore");
}

/// Issue #667: forget and restore select the same writable memory space as
/// other memory writes. Omitting the selector keeps the current-owner default.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one fixture exercises selected-space lifecycle and refused writes"
)]
async fn forget_and_restore_follow_an_explicit_memory_space() {
    let split_db = clone_split_core_db("proxiima_forget_restore_space")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let personal = OwnerId::from(UserId::new(Uuid::now_v7()));
        let group = OwnerId::from(proxiima_core::GroupId::new(Uuid::now_v7()));
        let built = Proxiima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(personal)
            .served_surface(proxiima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(personal, vec![personal, group], Role::admin());
        let shared = server_issued_group_space_selector(&tools, authz.clone(), personal).await;
        let call = |name: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), personal, name, args)
        };
        let search = |space: String| {
            let (tools, authz) = (&tools, authz.clone());
            async move {
                call_test_model_tool(
                    tools,
                    authz,
                    personal,
                    "core_search_memories",
                    serde_json::json!({
                        "query": "space lifecycle needle",
                        "mode": "lexical",
                        "spaces": [space],
                        "limit": 10
                    }),
                )
                .await
            }
        };
        let read = |handle: String| {
            let (tools, authz) = (&tools, authz.clone());
            async move {
                read_test_model_resource(
                    tools,
                    authz,
                    personal,
                    &format!("proxiima://memory/{handle}"),
                )
                .await
            }
        };
        let memory = call(
            "core_remember",
            serde_json::json!({
                "space": shared.clone(),
                "title": "space lifecycle needle",
                "body": "space lifecycle needle body",
                "tags": []
            }),
        )
        .await?["handle"]
            .as_str()
            .ok_or("handle")?
            .to_owned();

        // The old owner-only path cannot reach a memory in the selected group.
        let no_space_forget = call("core_forget", serde_json::json!({"memory": memory}))
            .await
            .expect_err("omitting space must keep the current-owner boundary");
        let no_space_restore = call("core_restore", serde_json::json!({"memory": memory}))
            .await
            .expect_err("omitting space must keep the current-owner boundary");
        assert_eq!(no_space_forget.kind(), CoreMcpErrorKind::NotFound);
        assert_eq!(no_space_restore.kind(), CoreMcpErrorKind::NotFound);

        let before = search(shared.clone()).await?;
        assert!(
            before["memories"]
                .as_array()
                .expect("search memories array")
                .iter()
                .any(|row| row["memory"] == memory),
            "{before}"
        );
        assert_eq!(read(memory.clone()).await?["handle"], memory);

        let key = shared.clone();
        call(
            "core_forget",
            serde_json::json!({"memory": memory, "space": key}),
        )
        .await?;
        let forgotten_search = search(shared.clone()).await?;
        assert!(
            !forgotten_search["memories"]
                .as_array()
                .expect("search memories array")
                .iter()
                .any(|row| row["memory"] == memory)
        );
        let forgotten_read = read(memory.clone()).await.expect_err("forgotten reads absent");
        assert_eq!(forgotten_read.kind(), CoreMcpErrorKind::NotFound);

        call(
            "core_restore",
            serde_json::json!({"memory": memory, "space": shared}),
        )
        .await?;
        let restored_search = search(shared.clone()).await?;
        assert!(
            restored_search["memories"]
                .as_array()
                .expect("search memories array")
                .iter()
                .any(|row| row["memory"] == memory)
        );
        assert_eq!(read(memory.clone()).await?["handle"], memory);

        // Selector failures use the same spring as core_remember, for both
        // lifecycle tools. A visible group without write authority is refused.
        let inaccessible = OwnerId::from(proxiima_core::GroupId::new(Uuid::now_v7())).to_string();
        for unknown in [String::from("not-a-space"), inaccessible] {
            let remember_unknown = call(
                "core_remember",
                serde_json::json!({"space": unknown.clone(), "title": "x", "body": "x", "tags": []}),
            )
            .await
            .expect_err("unknown space rejected by remember");
            let forget_unknown = call(
                "core_forget",
                serde_json::json!({"memory": memory, "space": unknown.clone()}),
            )
            .await
            .expect_err("unknown space rejected by forget");
            let restore_unknown = call(
                "core_restore",
                serde_json::json!({"memory": memory, "space": unknown}),
            )
            .await
            .expect_err("unknown space rejected by restore");
            assert_eq!(forget_unknown.to_string(), remember_unknown.to_string());
            assert_eq!(restore_unknown.to_string(), remember_unknown.to_string());
        }

        let viewer_authz = space_authz(personal, vec![personal, group], Role::viewer());
        let viewer_remember = call_test_model_tool(
            &tools,
            viewer_authz.clone(),
            personal,
            "core_remember",
            serde_json::json!({"space": shared.clone(), "title": "x", "body": "x", "tags": []}),
        )
        .await
        .expect_err("viewer cannot write the group space");
        let viewer_forget = call_test_model_tool(
            &tools,
            viewer_authz.clone(),
            personal,
            "core_forget",
            serde_json::json!({"space": shared, "memory": memory}),
        )
        .await
        .expect_err("viewer cannot forget in the group space");
        let viewer_restore = call_test_model_tool(
            &tools,
            viewer_authz,
            personal,
            "core_restore",
            serde_json::json!({"space": shared, "memory": memory}),
        )
        .await
        .expect_err("viewer cannot restore in the group space");
        assert_eq!(viewer_remember.kind(), CoreMcpErrorKind::NotAuthorized);
        assert_eq!(viewer_forget.kind(), viewer_remember.kind());
        assert_eq!(viewer_restore.kind(), viewer_remember.kind());
        assert_eq!(read(memory.clone()).await?["handle"], memory);
        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("forget and restore explicit space");
}

//! #488 S12: a pin or lineage edge to a memory the reader could read but
//! which is forgotten lists as forgotten, and lineage stops there.

use proxima::Proxima;
use proxima_core::{OwnerId, Role, UserId};
use uuid::Uuid;

use super::split_core_db::clone_split_core_db;
use super::{AgentMemoryApp, call_test_model_tool, read_test_model_resource, space_authz};

/// A ← B ← C, and C also rests on a live Abstraction E over a Fact D, so
/// forgetting B leaves C grounded. The owner then sees B as forgotten, from C's pins and from C's
/// lineage, and the walk does not reach A.
#[tokio::test]
async fn the_owner_sees_a_forgotten_source_as_forgotten_and_lineage_stops_there() {
    let split_db = clone_split_core_db("proxima_forgotten_targets")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
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
            &format!("proxima://memory/{top}?expand_neighbors=true"),
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
            &format!("proxima://memory/{top}/lineage?direction=ancestors&depth=3"),
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
    let split_db = clone_split_core_db("proxima_forget_last_source")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
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
            &format!("proxima://memory/{perspective}"),
        )
        .await?;
        assert_eq!(read["handle"], perspective.as_str(), "{read}");
        let lineage = read_test_model_resource(
            &tools,
            authz.clone(),
            owner,
            &format!("proxima://memory/{perspective}/lineage?direction=ancestors&depth=3"),
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

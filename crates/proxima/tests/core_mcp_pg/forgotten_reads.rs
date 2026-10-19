//! #488 review r1: a forgotten memory reads as absent on every read path.

use proxima::Proxima;
use proxima_core::{OwnerId, Role, UserId};
use uuid::Uuid;

use super::split_core_db::clone_split_core_db;
use super::{AgentMemoryApp, call_test_model_tool, space_authz};

fn handles(rows: &serde_json::Value, list: &str, field: &str) -> Vec<String> {
    rows[list]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row[field].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Recall takes caller-supplied subjects as raw ids. A forgotten subject's
/// sketch is not returned, while a live subject in the same call is.
#[tokio::test]
async fn recall_through_a_forgotten_subject_returns_nothing_for_it() {
    let split_db = clone_split_core_db("proxima_recall_forgotten")
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
        let remember = |title: &'static str| {
            call(
                "core_remember",
                serde_json::json!({"title": title, "body": title, "tags": []}),
            )
        };
        let gone = remember("recallneedle forgotten").await?["handle"]
            .as_str()
            .ok_or("handle")?
            .to_owned();
        let kept = remember("recallneedle kept").await?["handle"]
            .as_str()
            .ok_or("handle")?
            .to_owned();
        let args = serde_json::json!({"subjects": [gone.clone(), kept.clone()], "limit": 16});
        let before = call("core_recall", args.clone()).await?;
        assert!(
            handles(&before, "sketches", "handle").contains(&gone),
            "{before}"
        );

        call("core_forget", serde_json::json!({"memory": gone})).await?;

        let after = call("core_recall", args).await?;
        let recalled = handles(&after, "sketches", "handle");
        assert!(!recalled.contains(&gone), "{after}");
        assert!(
            !after.to_string().contains("recallneedle forgotten"),
            "{after}"
        );
        assert!(recalled.contains(&kept), "{after}");
        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("recall forgotten subject");
}

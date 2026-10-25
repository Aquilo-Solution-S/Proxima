//! #479 items 2 and 3: a memory reads the same wherever it is reached from.
//! A by-id or batch read reports the memory's own space, not the caller's,
//! and a search hit and a by-id read of one memory report the same body and
//! tags, both derived from the schema's search declaration.

use super::*;

fn find<'a>(rows: &'a serde_json::Value, list: &str, handle: &str) -> &'a serde_json::Value {
    rows[list]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["memory"] == handle))
        .unwrap_or_else(|| panic!("{handle} in {list}: {rows}"))
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one fixture: spaces, follow-up write, body and tag parity
async fn a_memory_reads_the_same_from_search_and_by_id() {
    let split_db = clone_split_core_db("proxima_core_memory_reads_alike")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let personal = OwnerId::from(UserId::new(Uuid::now_v7()));
        let group = OwnerId::from(GroupId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(personal)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(personal, vec![personal, group], Role::admin());
        let shared = server_issued_group_space_selector(&tools, authz.clone(), personal).await;
        assert_eq!(shared, group.to_string());
        let call = |name: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), personal, name, args)
        };
        let read = |uri: String| {
            let (tools, authz) = (&tools, authz.clone());
            async move { read_test_model_resource(tools, authz, personal, &uri).await }
        };
        let handle = |value: serde_json::Value| -> Result<String, Box<dyn std::error::Error>> {
            Ok(value["handle"].as_str().ok_or("handle")?.to_owned())
        };

        let m_p = handle(
            call(
                "core_remember",
                serde_json::json!({"space":"current","title":"alike personal",
                    "body":"alikeneedle personal note","tags":["alike"]}),
            )
            .await?,
        )?;
        let m_g = handle(
            call(
                "core_remember",
                serde_json::json!({"space":shared.clone(),"title":"alike group",
                    "body":"alikeneedle group note","tags":["grp"]}),
            )
            .await?,
        )?;

        // Item 2: the reported space is the memory's own.
        let by_id = read(format!("proxima://memory/{m_g}")).await?;
        assert_eq!(
            by_id["space"], shared,
            "a group memory reports the group: {by_id}"
        );
        assert_eq!(by_id["owner"], group.to_string());
        let batch = read(format!("proxima://memories?ids={m_p},{m_g}")).await?;
        assert_eq!(
            find(&batch, "memories", &m_p)["space"],
            "current",
            "{batch}"
        );
        assert_eq!(find(&batch, "memories", &m_g)["space"], shared, "{batch}");
        assert_eq!(
            find(&batch, "memories", &m_p)["owner"],
            personal.to_string()
        );
        assert_eq!(find(&batch, "memories", &m_g)["owner"], group.to_string());
        let group_current = read_test_model_resource(
            &tools,
            authz.clone(),
            group,
            &format!("proxima://memory/{m_g}"),
        )
        .await?;
        assert_eq!(group_current["space"], "current");
        assert_eq!(group_current["owner"], group.to_string());

        // A follow-up write to the reported space lands with the group.
        let reported = by_id["space"].as_str().ok_or("space")?.to_owned();
        let follow_up = handle(
            call(
                "core_remember",
                serde_json::json!({"space":reported,"title":"alike follow-up",
                    "body":"alikeneedle follow-up note","tags":[]}),
            )
            .await?,
        )?;
        let follow_read = read(format!("proxima://memory/{follow_up}")).await?;
        assert_eq!(follow_read["space"], shared, "{follow_read}");
        let in_group = call(
            "core_search_memories",
            serde_json::json!({"query":"alikeneedle","mode":"lexical","kind":"Fact",
                "spaces":[shared.clone()],"limit":10}),
        )
        .await?;
        let hit = find(&in_group, "memories", &follow_up);
        assert_eq!(
            hit["space"], shared,
            "a search hit reports the searched space"
        );

        // Item 3: an interpretation's body is its claim, in search and by id.
        let claim = "alikeclaim the personal note is a stance";
        let m_i = handle(
            call(
                "core_interpret",
                serde_json::json!({"claim":claim,"confidence":70,"subjects":[m_p.clone()]}),
            )
            .await?,
        )?;
        let search = |query: &'static str, kind: &'static str| {
            call(
                "core_search_memories",
                serde_json::json!({"query":query,"mode":"lexical","kind":kind,
                    "include_body":true,"limit":10}),
            )
        };
        let perspectives = search("alikeclaim", "Perspective").await?;
        let interpretation_hit = find(&perspectives, "memories", &m_i).clone();
        assert_eq!(interpretation_hit["body"], claim, "{perspectives}");
        let interpretation_read = read(format!("proxima://memory/{m_i}")).await?;
        assert_eq!(interpretation_read["body"], claim, "{interpretation_read}");

        // Parity: the same memory reports the same body and tags either way.
        let facts = search("alikeneedle", "Fact").await?;
        let note_hit = find(&facts, "memories", &m_p).clone();
        let note_read = read(format!("proxima://memory/{m_p}")).await?;
        assert_eq!(note_hit["tags"], serde_json::json!(["alike"]), "{facts}");
        for (hit, read) in [
            (&note_hit, &note_read),
            (&interpretation_hit, &interpretation_read),
        ] {
            assert_eq!(hit["body"], read["body"], "body parity: {hit} vs {read}");
            assert_eq!(hit["tags"], read["tags"], "tag parity: {hit} vs {read}");
        }

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("memory read parity test failed");
}

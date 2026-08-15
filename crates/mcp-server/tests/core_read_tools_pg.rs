mod common;

use std::sync::Arc;

use common::{create_db, db_url, drop_db};
use proxima_core::FlavorServices;
use proxima_core::mcp::McpAuthorContext;
use proxima_core::{AuthPath, AuthzContext, Engine, FlavorRegistry, Owner, OwnerRef, UserId};
use proxima_mcp_server::{McpAuthContext, McpToolHost};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

#[tokio::test]
async fn core_read_resources_return_prefixed_ids_and_author()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = create_db().await?;
    let database_url = db_url(&db_name);
    let pg = PgStorage::connect(&database_url).await?;
    pg.run_migrations().await?;

    let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let source = insert_memory(&pg, &owner, "source lineage memory", &[]).await?;
    let derived = insert_memory(&pg, &owner, "derived lineage memory", &[source]).await?;

    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let engine = Arc::new(
        Engine::new(registry.clone()).with_storage_ports(Arc::new(pg.clone()).storage_ports()),
    );
    let server = McpToolHost::from_engine(engine, FlavorServices::default());
    // The host is now the authoritative scope chokepoint, so reads need an
    // authenticated full-scope context (production always passes Some(auth);
    // a None context is unauthenticated and correctly denied).
    let auth = McpAuthContext {
        owner,
        authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer)
            .narrowed_to_owner(owner)
            .expect("personal owner narrows"),
        model_id: None,
    };

    let fetched = server
        .read_resource(
            &format!("proxima://memory/A:{derived}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(fetched["memory"], format!("A:{derived}"));
    assert_eq!(fetched["kind"], "Abstraction");
    assert_eq!(fetched["handle"], format!("A:{derived}"));
    assert_eq!(fetched["body"], "derived lineage memory");
    assert!(
        fetched.get("neighbor_edges").is_none(),
        "neighbor_edges should be omitted unless expand_neighbors is true"
    );

    let bare_uuid_err = server
        .read_resource(
            &format!("proxima://memory/{derived}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await
        .expect_err("bare uuid must be rejected; the wire speaks prefixed ids only");
    assert!(
        bare_uuid_err.to_string().contains("malformed memory id"),
        "unexpected error: {bare_uuid_err}"
    );

    let expanded = server
        .read_resource(
            &format!("proxima://memory/A:{derived}?expand_neighbors=true"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(expanded["memory"], format!("A:{derived}"));
    // Four fields is the whole edge: no handle, no relation, no payload.
    assert_eq!(expanded["neighbor_edges"][0]["kind"], "origin");
    assert_eq!(
        expanded["neighbor_edges"][0]["source"],
        format!("A:{derived}")
    );
    assert_eq!(
        expanded["neighbor_edges"][0]["target"],
        format!("A:{source}")
    );
    assert!(
        expanded["neighbor_edges"][0].get("edge").is_none(),
        "an edge has no id to hand back"
    );

    let lineage = server
        .read_resource(
            &format!("proxima://memory/A:{derived}/lineage?direction=ancestors&depth=1"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(lineage["start"], format!("A:{derived}"));
    assert!(
        lineage["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .any(|node| node["memory"] == format!("A:{source}"))
    );
    assert_eq!(lineage["edges"][0]["kind"], "origin");
    assert_eq!(lineage["edges"][0]["source"], format!("A:{derived}"));
    assert_eq!(lineage["edges"][0]["target"], format!("A:{source}"));

    drop(server);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// Slice E surface: the batch `proxima://memories` read, typed resource
/// errors (unknown path vs bad/missing parameter vs missing entity), and
/// lineage keyset pagination through the resource.
#[tokio::test]
async fn batch_memories_resource_error_classes_and_lineage_paging()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = create_db().await?;
    let database_url = db_url(&db_name);
    let pg = PgStorage::connect(&database_url).await?;
    pg.run_migrations().await?;

    let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let stranger = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let first = insert_memory(&pg, &owner, "batch first", &[]).await?;
    let second = insert_memory(&pg, &owner, "batch second", &[]).await?;
    let foreign = insert_memory(&pg, &stranger, "foreign memory", &[]).await?;
    let absent = uuid::Uuid::now_v7();

    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let engine = Arc::new(
        Engine::new(registry.clone()).with_storage_ports(Arc::new(pg.clone()).storage_ports()),
    );
    let server = McpToolHost::from_engine(engine, FlavorServices::default());
    let auth = McpAuthContext {
        owner,
        authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer)
            .narrowed_to_owner(owner)
            .expect("personal owner narrows"),
        model_id: None,
    };

    // One call returns the visible subset in request order and names the
    // rest as missing — invisible and nonexistent are indistinguishable.
    let batch = server
        .read_resource(
            &format!("proxima://memories?ids=A:{second},A:{absent},A:{first},A:{foreign}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    let memories = batch["memories"].as_array().expect("memories array");
    assert_eq!(memories.len(), 2);
    assert_eq!(memories[0]["memory"], format!("A:{second}"));
    assert_eq!(memories[1]["memory"], format!("A:{first}"));
    assert_eq!(memories[1]["body"], "batch first");
    assert_eq!(
        batch["missing"],
        serde_json::json!([format!("A:{absent}"), format!("A:{foreign}")])
    );

    assert_resource_error_classes(&server, &auth, absent).await;

    // Lineage: oversized depth clamps instead of erroring, pages follow
    // the opaque cursor to exhaustion, and a missing start is a not-found.
    let d1 = insert_memory(&pg, &owner, "derived one", &[]).await?;
    let d2 = insert_memory(&pg, &owner, "derived two", &[]).await?;
    insert_origin_edge(&pg, &owner, d1, first).await?;
    insert_origin_edge(&pg, &owner, d2, d1).await?;
    assert_lineage_clamps_pages_and_reports_missing_start(&server, &auth, d2, absent).await?;

    drop(server);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// Error classes keep their shapes instead of collapsing into
/// "unknown resource": missing/bad parameters name the parameter, an
/// unknown path classifies as resource-not-found, and a missing entity
/// dereference names the wire handle.
async fn assert_resource_error_classes(
    server: &McpToolHost,
    auth: &McpAuthContext,
    absent: uuid::Uuid,
) {
    let missing_param = server
        .read_resource("proxima://memories", author_ctx(), Some(auth.clone()))
        .await
        .expect_err("ids is required");
    assert!(
        missing_param
            .to_string()
            .contains("missing required parameter `ids`"),
        "{missing_param}"
    );
    let over_cap_ids = (0..101)
        .map(|_| format!("A:{}", uuid::Uuid::now_v7()))
        .collect::<Vec<_>>()
        .join(",");
    let over_cap = server
        .read_resource(
            &format!("proxima://memories?ids={over_cap_ids}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await
        .expect_err("101 ids exceed the batch cap");
    assert!(
        over_cap.to_string().contains("at most 100 memory ids"),
        "{over_cap}"
    );
    let unknown_path = server
        .read_resource("proxima://no-such-thing", author_ctx(), Some(auth.clone()))
        .await
        .expect_err("unknown resource path");
    assert!(
        matches!(
            &unknown_path,
            proxima_mcp_server::ToolInvocationError::ToolNotFound(uri)
                if uri == "proxima://no-such-thing"
        ),
        "unknown path must classify as resource-not-found: {unknown_path}"
    );
    let bad_param = server
        .read_resource(
            "proxima://change-events?limit=not-a-number",
            author_ctx(),
            Some(auth.clone()),
        )
        .await
        .expect_err("unparseable limit");
    assert!(
        bad_param.to_string().contains("invalid parameter `limit`"),
        "{bad_param}"
    );
    let not_found = server
        .read_resource(
            &format!("proxima://memory/A:{absent}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await
        .expect_err("missing memory dereference");
    assert!(
        not_found
            .to_string()
            .contains(&format!("memory A:{absent} not found")),
        "not-found names the wire handle: {not_found}"
    );
}

/// Oversized `depth` clamps instead of erroring, keyset pages cover the
/// walk exactly once, and a missing start memory reads as a not-found.
async fn assert_lineage_clamps_pages_and_reports_missing_start(
    server: &McpToolHost,
    auth: &McpAuthContext,
    start: uuid::Uuid,
    absent: uuid::Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let clamped = server
        .read_resource(
            &format!("proxima://memory/A:{start}/lineage?depth=300&limit=10"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(
        clamped["edges"].as_array().expect("edges").len(),
        2,
        "depth=300 clamps to the documented maximum instead of erroring"
    );

    let mut edges_seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let uri = match &cursor {
            Some(token) => format!("proxima://memory/A:{start}/lineage?limit=1&cursor={token}"),
            None => format!("proxima://memory/A:{start}/lineage?limit=1"),
        };
        let page = server
            .read_resource(&uri, author_ctx(), Some(auth.clone()))
            .await?;
        for edge in page["edges"].as_array().expect("edges") {
            edges_seen.push(format!(
                "{}->{}",
                edge["source"].as_str().expect("edge source"),
                edge["target"].as_str().expect("edge target"),
            ));
        }
        assert_eq!(
            page["has_more"] == serde_json::json!(true),
            page["next_cursor"].is_string(),
            "has_more iff next_cursor"
        );
        match page["next_cursor"].as_str() {
            Some(token) => cursor = Some(token.to_string()),
            None => break,
        }
        assert!(edges_seen.len() <= 2, "lineage paging must terminate");
    }
    edges_seen.sort_unstable();
    edges_seen.dedup();
    assert_eq!(edges_seen.len(), 2, "pages disjoint and exhaustive");

    let lineage_missing = server
        .read_resource(
            &format!("proxima://memory/A:{absent}/lineage"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await
        .expect_err("lineage of a missing memory");
    assert!(
        lineage_missing
            .to_string()
            .contains(&format!("memory A:{absent} not found")),
        "{lineage_missing}"
    );
    Ok(())
}

#[tokio::test]
async fn wake_candidates_resource_returns_armed_goal() -> Result<(), Box<dyn std::error::Error>> {
    let db_name = create_db().await?;
    let database_url = db_url(&db_name);
    let pg = PgStorage::connect(&database_url).await?;
    pg.run_migrations().await?;

    let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let trigger = insert_fact(&pg, &owner, "wake trigger fact").await?;
    let goal_id = insert_active_goal(&pg, &owner, Some(trigger)).await?;

    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let engine = Arc::new(
        Engine::new(registry.clone()).with_storage_ports(Arc::new(pg.clone()).storage_ports()),
    );
    let server = McpToolHost::from_engine(engine, FlavorServices::default());
    let auth = McpAuthContext {
        owner,
        authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer)
            .narrowed_to_owner(owner)
            .expect("personal owner narrows"),
        model_id: None,
    };

    let output = server
        .read_resource(
            &format!("proxima://wake-candidates?fact=F:{trigger}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    let candidates = output["candidates"].as_array().expect("candidates array");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["goal"], format!("G:{goal_id}"));
    assert_eq!(candidates[0]["prompt"], "plan only");
    assert_eq!(candidates[0]["tool_ids"][0], "core_search_memories");
    assert_eq!(candidates[0]["actor_write_owners"][0], owner.external_key());

    // A non-Fact reference class is rejected at parse (F:<uuid> required)...
    let abstraction = insert_memory(&pg, &owner, "not a fact", &[]).await?;
    let wrong_class = server
        .read_resource(
            &format!("proxima://wake-candidates?fact=A:{abstraction}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await
        .expect_err("non-Fact reference class must be rejected at parse");
    assert!(
        wrong_class.to_string().contains("got prefix 'A'"),
        "unexpected error: {wrong_class}"
    );

    // ...and a Fact-classed reference to a non-Fact row is still rejected by
    // the engine's kind check (defense in depth behind the parse gate).
    let mislabeled = server
        .read_resource(
            &format!("proxima://wake-candidates?fact=F:{abstraction}"),
            author_ctx(),
            Some(auth),
        )
        .await
        .expect_err("Fact-classed reference to an Abstraction row must be rejected");
    assert!(
        mislabeled.to_string().contains("must be a Fact"),
        "unexpected error: {mislabeled}"
    );

    drop(server);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn goal_resources_list_read_back_wake_config_and_paginate()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = create_db().await?;
    let database_url = db_url(&db_name);
    let pg = PgStorage::connect(&database_url).await?;
    pg.run_migrations().await?;

    let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let trigger = insert_fact(&pg, &owner, "goal read trigger fact").await?;
    let mut goal_ids = Vec::new();
    goal_ids.push(insert_active_goal(&pg, &owner, Some(trigger)).await?);
    goal_ids.push(insert_active_goal(&pg, &owner, None).await?);
    goal_ids.push(insert_active_goal(&pg, &owner, None).await?);

    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let engine = Arc::new(
        Engine::new(registry.clone()).with_storage_ports(Arc::new(pg.clone()).storage_ports()),
    );
    let server = McpToolHost::from_engine(engine, FlavorServices::default());
    let auth = McpAuthContext {
        owner,
        authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer)
            .narrowed_to_owner(owner)
            .expect("personal owner narrows"),
        model_id: None,
    };

    // Paginate Active goals two-at-a-time: disjoint, exhaustive, terminated.
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let uri = match &cursor {
            Some(token) => format!("proxima://goals?state=Active&limit=2&cursor={token}"),
            None => "proxima://goals?state=Active&limit=2".to_string(),
        };
        let page = server
            .read_resource(&uri, author_ctx(), Some(auth.clone()))
            .await?;
        for goal in page["goals"].as_array().expect("goals array") {
            seen.push(goal["goal"].as_str().expect("goal ref").to_string());
        }
        if page["has_more"] == serde_json::json!(true) {
            cursor = Some(
                page["next_cursor"]
                    .as_str()
                    .expect("has_more implies next_cursor")
                    .to_string(),
            );
            assert!(seen.len() <= 3, "pagination must terminate");
        } else {
            assert_eq!(page["next_cursor"], serde_json::Value::Null);
            break;
        }
    }
    let mut deduped = seen.clone();
    deduped.sort_unstable();
    deduped.dedup();
    assert_eq!(deduped.len(), 3, "pages disjoint and exhaustive: {seen:?}");
    for goal_id in &goal_ids {
        assert!(seen.contains(&format!("G:{goal_id}")));
    }

    // The armed goal reads back its wake config; unarmed goals carry none.
    let armed = server
        .read_resource(
            &format!("proxima://goal/G:{}", goal_ids[0]),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(armed["goal"], format!("G:{}", goal_ids[0]));
    assert_eq!(armed["state"], "Active");
    assert_eq!(armed["wake"]["trigger_fact"], format!("F:{trigger}"));
    assert_eq!(armed["wake"]["prompt"], "plan only");
    assert_eq!(armed["wake"]["tool_ids"][0], "core_search_memories");
    let unarmed = server
        .read_resource(
            &format!("proxima://goal/G:{}", goal_ids[1]),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert!(unarmed.get("wake").is_none(), "unarmed goal has no wake");

    assert_goal_reference_and_state_rejections(&server, &auth, goal_ids[0]).await;

    // Wake candidates now signal truncation instead of dropping silently.
    arm_goal_for_fact(&pg, goal_ids[1], trigger).await?;
    arm_goal_for_fact(&pg, goal_ids[2], trigger).await?;
    assert_wake_candidates_signal_truncation(&server, &auth, trigger).await?;

    drop(server);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// An interpretation Perspective's implied connections show up in the
/// index as `reference` rows nobody wrote. There is no edge handle to
/// dereference: an edge has no id.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn edge_resources_read_back_interpretation_references()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = create_db().await?;
    let database_url = db_url(&db_name);
    let pg = PgStorage::connect(&database_url).await?;
    pg.run_migrations().await?;

    let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let hub = insert_memory(&pg, &owner, "hub abstraction", &[]).await?;
    let spoke = insert_memory(&pg, &owner, "spoke abstraction", &[]).await?;

    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let engine = Arc::new(
        Engine::new(registry.clone()).with_storage_ports(Arc::new(pg.clone()).storage_ports()),
    );
    let server = McpToolHost::from_engine(engine, FlavorServices::default());
    let auth = McpAuthContext {
        owner,
        authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer)
            .narrowed_to_owner(owner)
            .expect("personal owner narrows"),
        model_id: None,
    };

    let interpreted = server
        .call_tool(
            "core_interpret",
            serde_json::json!({
                "claim": "the hub summarizes the spoke",
                "confidence": 61,
                "subjects": [format!("A:{hub}"), format!("A:{spoke}")],
            }),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    let handle = interpreted["handle"]
        .as_str()
        .expect("interpretation handle")
        .to_string();
    assert!(
        handle.starts_with("P:"),
        "an interpretation is a Perspective"
    );
    assert_eq!(
        interpreted["edge_count"],
        serde_json::json!(2),
        "a count, not handles: {interpreted}"
    );

    let listed = server
        .read_resource(
            &format!("proxima://edges?kind=reference&source={handle}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    let edges = listed["edges"].as_array().expect("edges array");
    assert_eq!(edges.len(), 2);
    let targets: Vec<&str> = edges
        .iter()
        .map(|edge| edge["target"].as_str().expect("target handle"))
        .collect();
    assert!(
        targets.contains(&format!("A:{hub}").as_str()),
        "{targets:?}"
    );
    assert!(
        targets.contains(&format!("A:{spoke}").as_str()),
        "{targets:?}"
    );
    for edge in edges {
        assert_eq!(edge["source"], handle);
        assert_eq!(edge["kind"], "reference");
        assert!(
            edge.get("payload").is_none() && edge.get("relation").is_none(),
            "an edge carries no content: {edge}"
        );
        assert!(
            edge["created_at"]
                .as_str()
                .is_some_and(|value| value.contains('T')),
            "created_at is RFC3339: {:?}",
            edge["created_at"]
        );
    }
    assert_eq!(listed["has_more"], serde_json::json!(false));
    assert_eq!(listed["next_cursor"], serde_json::Value::Null);

    // The exact source+target probe an idempotent writer would run — the
    // replacement for asking "does this edge id already exist".
    let probe = server
        .read_resource(
            &format!("proxima://edges?source={handle}&target=A:{spoke}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(probe["edges"].as_array().expect("edges").len(), 1);

    // Re-asserting the same judgment is one memory and the same two rows:
    // structural idempotency, with no id scheme to keep honest.
    let replay = server
        .call_tool(
            "core_interpret",
            serde_json::json!({
                "claim": "the hub summarizes the spoke",
                "confidence": 61,
                "subjects": [format!("A:{hub}"), format!("A:{spoke}")],
            }),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(replay["handle"], serde_json::json!(handle));
    assert_eq!(replay["idempotent_replay"], serde_json::json!(true));

    assert_edge_filter_rejections(&server, &auth, &handle).await;

    drop(server);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// Unfiltered dumps, kinds outside the closed vocabulary, foreign cursors,
/// and the retired single-edge path all fail closed.
async fn assert_edge_filter_rejections(server: &McpToolHost, auth: &McpAuthContext, source: &str) {
    let unfiltered = server
        .read_resource("proxima://edges", author_ctx(), Some(auth.clone()))
        .await
        .expect_err("unfiltered edge dump must be rejected");
    assert!(
        unfiltered.to_string().contains("at least one filter"),
        "{unfiltered}"
    );
    let unknown_kind = server
        .read_resource(
            "proxima://edges?kind=structural",
            author_ctx(),
            Some(auth.clone()),
        )
        .await
        .expect_err("the kind vocabulary is closed at origin and reference");
    assert!(
        unknown_kind.to_string().contains("unknown edge kind"),
        "{unknown_kind}"
    );
    let bad_cursor = server
        .read_resource(
            &format!("proxima://edges?source={source}&cursor=garbage"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await
        .expect_err("malformed cursor must be rejected");
    assert!(
        bad_cursor.to_string().contains("malformed cursor"),
        "{bad_cursor}"
    );
    // `proxima://edge/{id}` is gone with the id it dereferenced: it is not a
    // bad parameter on a known template, it is not a template.
    let retired = server
        .read_resource(
            &format!("proxima://edge/{}", uuid::Uuid::now_v7()),
            author_ctx(),
            Some(auth.clone()),
        )
        .await
        .expect_err("the single-edge resource is retired");
    assert!(
        matches!(
            &retired,
            proxima_mcp_server::ToolInvocationError::ToolNotFound(_)
        ),
        "a retired template is resource-not-found: {retired}"
    );
}

/// Class-checked reference and closed state vocabulary fail closed.
async fn assert_goal_reference_and_state_rejections(
    server: &McpToolHost,
    auth: &McpAuthContext,
    goal_id: uuid::Uuid,
) {
    let bare = server
        .read_resource(
            &format!("proxima://goal/{goal_id}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await
        .expect_err("bare uuid goal reference must be rejected");
    assert!(bare.to_string().contains("malformed Goal id"), "{bare}");
    let bad_state = server
        .read_resource(
            "proxima://goals?state=Everything",
            author_ctx(),
            Some(auth.clone()),
        )
        .await
        .expect_err("unknown state must be rejected");
    assert!(
        bad_state.to_string().contains("must be one of"),
        "{bad_state}"
    );
}

/// A page smaller than the admitted set reports `has_more`; a page that
/// covers it reports completion.
async fn assert_wake_candidates_signal_truncation(
    server: &McpToolHost,
    auth: &McpAuthContext,
    trigger: uuid::Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let capped = server
        .read_resource(
            &format!("proxima://wake-candidates?fact=F:{trigger}&limit=2"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(
        capped["candidates"].as_array().expect("candidates").len(),
        2
    );
    assert_eq!(capped["has_more"], serde_json::json!(true));
    let full = server
        .read_resource(
            &format!("proxima://wake-candidates?fact=F:{trigger}&limit=10"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(full["candidates"].as_array().expect("candidates").len(), 3);
    assert_eq!(full["has_more"], serde_json::json!(false));
    Ok(())
}

async fn insert_fact(
    pg: &PgStorage,
    owner: &Owner,
    text: &str,
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let t = insert_memory_row(pg, owner, "fact", "test/wake-e2e-fact-v1", &[]).await?;
    sqlx::query(
        "INSERT INTO proxima_core.agent_note_v1
            (memory_id, note_id, title, body, tags)
         VALUES ($1, $1, $2, $2, ARRAY[]::text[])",
    )
    .bind(t)
    .bind(text)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(t)
}

async fn insert_active_goal(
    pg: &PgStorage,
    owner: &Owner,
    trigger_t: Option<uuid::Uuid>,
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let handle = uuid::Uuid::now_v7();
    let t = uuid::Uuid::now_v7();
    let owner_id = owner.stored_owner_id();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, $2::proxima_core.owner_kind) ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .bind(proxima_core::OwnerRefKind::of(owner).as_str())
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
         VALUES ($1, 'core/simple-text-v1', $2, $3)",
    )
    .bind(handle)
    .bind(owner_id)
    .bind(t)
    .execute(pg.pool_for_tests())
    .await?;
    let wake_id = if let Some(trigger) = trigger_t {
        let wake_id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.wake_config
                (owner_id, trigger_kind, trigger_t, tool_ids, prompt)
             VALUES ($1, 'fact_memory', $2, ARRAY['core_search_memories'], 'plan only')
             RETURNING wake_id",
        )
        .bind(owner_id)
        .bind(trigger)
        .fetch_one(pg.pool_for_tests())
        .await?;
        Some(wake_id)
    } else {
        None
    };
    sqlx::query(
        "INSERT INTO proxima_core.goal (handle, t, owner_id, title, state, request_id, wake_id)
         VALUES ($1, $2, $3, 'wake goal', 'Active', $4, $5)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .bind(format!("wake-e2e:{t}"))
    .bind(wake_id)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(t)
}

async fn arm_goal_for_fact(
    pg: &PgStorage,
    goal_id: uuid::Uuid,
    trigger_memory_id: uuid::Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let (handle, owner_id, title, request_id): (Uuid, Uuid, String, String) = sqlx::query_as(
        "SELECT handle, owner_id, title, request_id FROM proxima_core.goal WHERE t = $1",
    )
    .bind(goal_id)
    .fetch_one(pg.pool_for_tests())
    .await?;
    let wake_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.wake_config
            (owner_id, trigger_kind, trigger_t, tool_ids, prompt)
         VALUES ($1, 'fact_memory', $2, ARRAY['core_search_memories'], 'plan only')
         RETURNING wake_id",
    )
    .bind(owner_id)
    .bind(trigger_memory_id)
    .fetch_one(pg.pool_for_tests())
    .await?;
    let t = uuid::Uuid::now_v7();
    sqlx::query("UPDATE proxima_core.goal_head SET t = $2 WHERE handle = $1")
        .bind(handle)
        .bind(t)
        .execute(pg.pool_for_tests())
        .await?;
    sqlx::query(
        "INSERT INTO proxima_core.goal
            (handle, t, owner_id, title, state, request_id, wake_id)
         VALUES ($1, $2, $3, $4, 'Active', $5, $6)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .bind(title)
    .bind(format!("{request_id}:armed"))
    .bind(wake_id)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(())
}

async fn insert_memory_row(
    pg: &PgStorage,
    owner: &Owner,
    kind: &str,
    schema_id: &str,
    origins: &[uuid::Uuid],
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let handle = uuid::Uuid::now_v7();
    let t = uuid::Uuid::now_v7();
    let owner_id = owner.stored_owner_id();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, $2::proxima_core.owner_kind) ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .bind(proxima_core::OwnerRefKind::of(owner).as_str())
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, $2::proxima_core.memory_kind, $3, $4, $5)",
    )
    .bind(handle)
    .bind(kind)
    .bind(schema_id)
    .bind(owner_id)
    .bind(t)
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, origins)
         VALUES ($1, $2, $3::proxima_core.memory_kind, $4, $5)",
    )
    .bind(handle)
    .bind(t)
    .bind(kind)
    .bind(owner_id)
    .bind(origins)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(t)
}

async fn insert_memory(
    pg: &PgStorage,
    owner: &Owner,
    text: &str,
    origins: &[uuid::Uuid],
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let t = insert_memory_row(pg, owner, "abstraction", "core/agent-derivation-v1", origins)
        .await?;
    sqlx::query(
        "INSERT INTO proxima_core.agent_derivation_v1
            (memory_id, title, body, tags, source_memory_ids,
             model_id, client_name, client_version)
         VALUES ($1, $2, $2, ARRAY[]::text[], ARRAY[]::uuid[],
                 'test-model', 'test', 'test-v1')",
    )
    .bind(t)
    .bind(text)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(t)
}

/// Assert one `origin` row directly. Five columns is the whole insert —
/// there is no id to mint and no relation to name.
async fn insert_origin_edge(
    pg: &PgStorage,
    owner: &Owner,
    source: uuid::Uuid,
    target: uuid::Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let _ = owner;
    let (handle, kind, owner_id): (Uuid, String, Uuid) = sqlx::query_as(
        "SELECT handle, kind::text, owner_id FROM proxima_core.memory WHERE t = $1",
    )
    .bind(source)
    .fetch_one(pg.pool_for_tests())
    .await?;
    let schema_id: String = sqlx::query_scalar(
        "SELECT schema_id FROM proxima_core.memory_head WHERE handle = $1",
    )
    .bind(handle)
    .fetch_one(pg.pool_for_tests())
    .await?;
    let t = uuid::Uuid::now_v7();
    sqlx::query("UPDATE proxima_core.memory_head SET t = $2 WHERE handle = $1")
        .bind(handle)
        .bind(t)
        .execute(pg.pool_for_tests())
        .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, origins)
         VALUES ($1, $2, $3::proxima_core.memory_kind, $4, $5)",
    )
    .bind(handle)
    .bind(t)
    .bind(&kind)
    .bind(owner_id)
    .bind([target])
    .execute(pg.pool_for_tests())
    .await?;
    let _ = schema_id;
    Ok(())
}

fn author_ctx() -> McpAuthorContext {
    McpAuthorContext {
        model_id: "codex-test".into(),
        client_name: "codex".into(),
        client_version: "1".into(),
        caller_self_perspective: None,
    }
}

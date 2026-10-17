mod common;

use std::sync::Arc;

use common::{create_core_db, db_url, drop_db, storage};
use proxima_core::FlavorServices;
use proxima_core::mcp::McpAuthorContext;
use proxima_core::test_fixtures::authenticated_context;
use proxima_core::{AuthPath, AuthzContext, Engine, FlavorRegistry, MemoryId, OwnerId, UserId};
use proxima_mcp_server::{McpAuthContext, McpToolHost};
use proxima_storage_pg::PgStorage;

fn owner_authz(owner: OwnerId) -> AuthzContext {
    authenticated_context(
        AuthzContext::single_owner(&owner, AuthPath::HostBearer)
            .narrowed_to_owner(owner)
            .expect("personal owner narrows"),
    )
}

#[tokio::test]
async fn core_read_resources_return_bare_uuids_and_author() -> Result<(), Box<dyn std::error::Error>>
{
    let db_name = create_core_db().await?;
    let database_url = db_url(&db_name);
    let pg = storage(&database_url).await?;

    let owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
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
        authz: owner_authz(owner),
    };

    let fetched = server
        .read_resource(
            &format!("proxima://memory/{derived}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(fetched["memory"], format!("{derived}"));
    assert_eq!(fetched["kind"], "Abstraction");
    assert_eq!(fetched["handle"], format!("{derived}"));
    assert_eq!(fetched["body"], "derived lineage memory");
    assert!(
        fetched.get("neighbor_edges").is_none(),
        "neighbor_edges should be omitted unless expand_neighbors is true"
    );

    let prefixed_uuid_err = server
        .read_resource(
            &format!("proxima://memory/A:{derived}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await
        .expect_err("prefixed UUID must be rejected");
    assert!(
        prefixed_uuid_err.to_string().contains("hyphenated UUID"),
        "a prefixed resource must report the bare UUID grammar"
    );

    let expanded = server
        .read_resource(
            &format!("proxima://memory/{derived}?expand_neighbors=true"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(expanded["memory"], format!("{derived}"));
    // Four fields is the whole edge: no handle, no relation, no payload.
    assert_eq!(expanded["neighbor_edges"][0]["kind"], "origin");
    assert_eq!(
        expanded["neighbor_edges"][0]["source"],
        format!("{derived}")
    );
    assert_eq!(expanded["neighbor_edges"][0]["target"], format!("{source}"));
    assert!(
        expanded["neighbor_edges"][0].get("edge").is_none(),
        "an edge has no id to hand back"
    );

    let lineage = server
        .read_resource(
            &format!("proxima://memory/{derived}/lineage?direction=ancestors&depth=1"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(lineage["start"], format!("{derived}"));
    assert!(
        lineage["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .any(|node| node["memory"] == format!("{source}"))
    );
    assert_eq!(lineage["edges"][0]["kind"], "origin");
    assert_eq!(lineage["edges"][0]["source"], format!("{derived}"));
    assert_eq!(lineage["edges"][0]["target"], format!("{source}"));

    drop(server);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn search_results_expose_structured_memory_id() -> Result<(), Box<dyn std::error::Error>> {
    let db_name = create_core_db().await?;
    let database_url = db_url(&db_name);
    let pg = storage(&database_url).await?;

    let owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
    let memory_id = insert_memory(&pg, &owner, "structured search identity", &[]).await?;
    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let engine = Arc::new(
        Engine::new(registry.clone()).with_storage_ports(Arc::new(pg.clone()).storage_ports()),
    );
    let server = McpToolHost::from_engine(engine, FlavorServices::default());
    let auth = McpAuthContext {
        owner,
        authz: owner_authz(owner),
    };

    let output = server
        .call_tool(
            "core_search_memories",
            serde_json::json!({
                "query": "structured search identity",
                "mode": "lexical",
                "limit": 1,
            }),
            author_ctx(),
            Some(auth),
        )
        .await?;
    let result = output["memories"]
        .as_array()
        .and_then(|memories| memories.first())
        .expect("search returns the inserted memory");
    let structured_id = MemoryId::new(serde_json::from_value(result["memory_id"].clone())?);
    assert_eq!(structured_id.into_inner(), memory_id);
    assert_eq!(result["memory"], format!("{memory_id}"));

    drop(server);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

async fn assert_goals_do_not_transfer(
    server: &McpToolHost,
    auth: &McpAuthContext,
    references: &[uuid::Uuid],
) {
    for goal_reference in references {
        let transfer = server
            .call_tool(
                "core_transfer",
                serde_json::json!({
                    "action": "transfer_to_owner",
                    "entity": goal_reference.to_string(),
                    "to_owner": auth.owner.to_string(),
                }),
                author_ctx(),
                Some(auth.clone()),
            )
            .await
            .expect_err("a stored Goal UUID retains the typed transfer refusal");
        assert!(
            transfer.to_string().contains("goals do not transfer"),
            "{transfer}"
        );
    }
}

#[tokio::test]
async fn goal_resources_list_and_paginate() -> Result<(), Box<dyn std::error::Error>> {
    let db_name = create_core_db().await?;
    let database_url = db_url(&db_name);
    let pg = storage(&database_url).await?;

    let owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
    let (goal_handles, goal_ids): (Vec<_>, Vec<_>) = [
        insert_active_goal(&pg, &owner).await?,
        insert_active_goal(&pg, &owner).await?,
        insert_active_goal(&pg, &owner).await?,
    ]
    .into_iter()
    .unzip();
    let foreign_owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
    let (foreign_handle, _) = insert_active_goal(&pg, &foreign_owner).await?;

    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let engine = Arc::new(
        Engine::new(registry.clone()).with_storage_ports(Arc::new(pg.clone()).storage_ports()),
    );
    let server = McpToolHost::from_engine(engine, FlavorServices::default());
    let auth = McpAuthContext {
        owner,
        authz: owner_authz(owner),
    };

    assert_goals_do_not_transfer(&server, &auth, &[goal_ids[0], goal_handles[0]]).await;

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
        assert!(seen.contains(&format!("{goal_id}")));
    }

    let paused_head = proxima_core::new_memory_uuid(0);
    insert_goal_version(
        &pg,
        &owner,
        Some(goal_handles[1]),
        paused_head,
        "Paused",
        None,
    )
    .await?;
    let union = server
        .read_resource(
            "proxima://goals?state=paused,ACTIVE,paused",
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(union["goals"].as_array().expect("goals array").len(), 3);

    let goal = server
        .read_resource(
            &format!("proxima://goal/{}", goal_handles[1]),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(goal["goal"], format!("{paused_head}"));

    for handle in [foreign_handle, proxima_core::new_memory_uuid(0)] {
        let error = server
            .read_resource(
                &format!("proxima://goal/{handle}"),
                author_ctx(),
                Some(auth.clone()),
            )
            .await
            .expect_err("unreadable and nonexistent series are both absent");
        assert!(error.to_string().contains("not found"), "{error}");
    }

    assert_goal_reference_and_state_rejections(&server, &auth, goal_ids[0]).await;

    drop(server);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// A Goal read returns the prompt text, when the series was set and when this
/// version changed, over the wire a client sees: `text` is a string when the
/// version knows it and JSON `null` (the key present) when it does not, and
/// both times are RFC 3339.
#[tokio::test]
async fn goal_resources_carry_text_set_at_and_changed_at() -> Result<(), Box<dyn std::error::Error>>
{
    let db_name = create_core_db().await?;
    let pg = storage(&db_url(&db_name)).await?;

    let owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
    // Unknown text: a version no declaration can speak for.
    let (unknown_handle, unknown) = insert_active_goal(&pg, &owner).await?;
    // A series set at 22:13:20Z and paused a minute later; the head is the
    // second version, which keeps the first one's text.
    let set = goal_t_at(1_700_000_000_000);
    let paused = goal_t_at(1_700_000_060_000);
    let handle =
        insert_goal_version(&pg, &owner, None, set, "Active", Some("write the report")).await?;
    insert_goal_version(
        &pg,
        &owner,
        Some(handle),
        paused,
        "Paused",
        Some("write the report"),
    )
    .await?;

    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let engine = Arc::new(
        Engine::new(registry.clone()).with_storage_ports(Arc::new(pg.clone()).storage_ports()),
    );
    let server = McpToolHost::from_engine(engine, FlavorServices::default());
    let auth = McpAuthContext {
        owner,
        authz: owner_authz(owner),
    };

    let listing = server
        .read_resource("proxima://goals", author_ctx(), Some(auth.clone()))
        .await?;
    let listed = |goal: uuid::Uuid| {
        listing["goals"]
            .as_array()
            .expect("goals array")
            .iter()
            .find(|row| row["goal"] == format!("{goal}"))
            .unwrap_or_else(|| panic!("goal {goal} is listed: {listing}"))
            .clone()
    };

    // The listing returns the full text and both times.
    let series = listed(paused);
    assert_eq!(series["handle"], format!("{handle}"));
    assert_eq!(series["state"], "Paused");
    assert_eq!(series["text"], "write the report");
    assert_eq!(series["set_at"], "2023-11-14T22:13:20Z");
    assert_eq!(series["changed_at"], "2023-11-14T22:14:20Z");

    // Unknown is explicit: the key is there and it is JSON null.
    let unlisted = listed(unknown);
    assert_eq!(
        unlisted.get("text"),
        Some(&serde_json::Value::Null),
        "an unknown text serialises as null, not as an absent key: {unlisted}"
    );
    assert_eq!(unlisted["set_at"], unlisted["changed_at"]);
    let set_at = unlisted["set_at"].as_str().expect("set_at is a string");
    time::OffsetDateTime::parse(set_at, &time::format_description::well_known::Rfc3339)
        .expect("set_at is RFC 3339");

    // A handle reads the current head, and so does any version id a client
    // was given, here the superseded first version. The current head is the
    // paused version at this point.
    let single = server
        .read_resource(
            &format!("proxima://goal/{handle}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(single, series, "the single read and the listing agree");
    assert_version_id_reads_series_head(&server, &auth, set, &series).await;
    let single_unknown = server
        .read_resource(
            &format!("proxima://goal/{unknown_handle}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(single_unknown.get("text"), Some(&serde_json::Value::Null));

    // The stable handle reads the current head rather than a version id.
    let current: uuid::Uuid =
        sqlx::query_scalar("SELECT t FROM proxima_core.goal_head WHERE handle = $1")
            .bind(handle)
            .fetch_one(pg.pool_for_tests())
            .await?;
    let current_read = server
        .read_resource(
            &format!("proxima://goal/{handle}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(current_read["goal"], format!("{current}"));
    assert_eq!(current_read["handle"], format!("{handle}"));
    assert_eq!(current_read["state"], "Paused");

    drop(server);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// An interpretation Perspective's implied connections show up as
/// `reference` `neighbor_edges` on `proxima://memory`. There is no
/// `proxima://edges` resource.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn edge_resources_read_back_interpretation_references()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = create_core_db().await?;
    let database_url = db_url(&db_name);
    let pg = storage(&database_url).await?;

    let owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
    let hub = insert_memory(&pg, &owner, "hub abstraction", &[]).await?;
    let spoke = insert_memory(&pg, &owner, "spoke abstraction", &[]).await?;

    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let engine = Arc::new(
        Engine::new(registry.clone()).with_storage_ports(Arc::new(pg.clone()).storage_ports()),
    );
    let server = McpToolHost::from_engine(engine, FlavorServices::default());
    let auth = McpAuthContext {
        owner,
        authz: owner_authz(owner),
    };

    let interpreted = server
        .call_tool(
            "core_interpret",
            serde_json::json!({
                "claim": "the hub summarizes the spoke",
                "confidence": 61,
                "subjects": [format!("{hub}"), format!("{spoke}")],
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
        handle.parse::<uuid::Uuid>().is_ok(),
        "an interpretation is a Perspective"
    );
    assert_eq!(
        interpreted["edge_count"],
        serde_json::json!(2),
        "a count, not handles: {interpreted}"
    );

    let listed = server
        .read_resource(
            &format!("proxima://memory/{handle}?expand_neighbors=true"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    let edges = listed["neighbor_edges"]
        .as_array()
        .expect("neighbor_edges array");
    assert_eq!(edges.len(), 2);
    let targets: Vec<&str> = edges
        .iter()
        .map(|edge| edge["target"].as_str().expect("target handle"))
        .collect();
    assert!(targets.contains(&format!("{hub}").as_str()), "{targets:?}");
    assert!(
        targets.contains(&format!("{spoke}").as_str()),
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
            edge.get("edge").is_none(),
            "an edge has no id to hand back: {edge}"
        );
    }

    // Re-asserting the same judgment is one memory and the same two rows:
    // structural idempotency, with no id scheme to keep honest.
    let replay = server
        .call_tool(
            "core_interpret",
            serde_json::json!({
                "claim": "the hub summarizes the spoke",
                "confidence": 61,
                "subjects": [format!("{hub}"), format!("{spoke}")],
            }),
            author_ctx(),
            Some(auth.clone()),
        )
        .await?;
    assert_eq!(replay["handle"], serde_json::json!(handle));
    assert_eq!(replay["idempotent_replay"], serde_json::json!(true));

    let retired = server
        .read_resource(
            &format!("proxima://edges?kind=reference&source={handle}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await
        .expect_err("proxima://edges is not a resource");
    assert!(
        matches!(
            &retired,
            proxima_mcp_server::ToolInvocationError::ToolNotFound(_)
        ),
        "retired edges catalog is resource-not-found: {retired}"
    );

    drop(server);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// A version UUID resolves to its series' current head.
async fn assert_version_id_reads_series_head(
    server: &McpToolHost,
    auth: &McpAuthContext,
    version: uuid::Uuid,
    head: &serde_json::Value,
) {
    let from_version = server
        .read_resource(
            &format!("proxima://goal/{version}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await
        .expect("a version id reads its series");
    assert_eq!(
        &from_version, head,
        "a superseded version id reads its series' current head"
    );
    let missing = server
        .read_resource(
            &format!("proxima://goal/{}", proxima_core::new_memory_uuid(0)),
            author_ctx(),
            Some(auth.clone()),
        )
        .await
        .expect_err("an id naming no version and no series is not found");
    assert!(missing.to_string().contains("not found"), "{missing}");
}

/// Goal UUIDs read successfully; prefixes and unknown states fail closed.
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
        .expect("bare UUID goal reference reads the current head");
    assert_eq!(bare["goal"], goal_id.to_string());
    let prefixed = server
        .read_resource(
            &format!("proxima://goal/G:{goal_id}"),
            author_ctx(),
            Some(auth.clone()),
        )
        .await
        .expect_err("prefixed goal references are refused");
    assert!(
        prefixed.to_string().contains("hyphenated UUID"),
        "{prefixed}"
    );
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
    for state in ["", " ", "Active,", "Active,,Paused"] {
        let error = server
            .read_resource(
                &format!("proxima://goals?state={state}"),
                author_ctx(),
                Some(auth.clone()),
            )
            .await
            .expect_err("empty state members must be rejected");
        assert!(error.to_string().contains("empty"), "{state:?}: {error}");
    }
}

async fn insert_active_goal(
    pg: &PgStorage,
    owner: &OwnerId,
) -> Result<(uuid::Uuid, uuid::Uuid), Box<dyn std::error::Error>> {
    let handle = proxima_core::new_memory_uuid(0);
    let t = proxima_core::new_memory_uuid(0);
    let owner_id = owner.into_inner();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal'::proxima_core.owner_kind) ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
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
    sqlx::query(
        "INSERT INTO proxima_core.goal (handle, t, owner_id, title, state, request_id, \"by\", via)
         VALUES ($1, $2, $3, 'goal', 'Active', $4, $3, NULL)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .bind(format!("read-e2e:{t}"))
    .execute(pg.pool_for_tests())
    .await?;
    Ok((handle, t))
}

/// A `UUIDv7` minted at an exact millisecond.
fn goal_t_at(millis: u64) -> uuid::Uuid {
    uuid::Uuid::new_v7(uuid::Timestamp::from_unix(
        uuid::NoContext,
        millis / 1000,
        u32::try_from(millis % 1000).expect("below 1000") * 1_000_000,
    ))
}

/// One version of a Goal series with its text, written straight to the
/// tables: `handle` `None` starts a new series. Returns the series handle.
async fn insert_goal_version(
    pg: &PgStorage,
    owner: &OwnerId,
    handle: Option<uuid::Uuid>,
    t: uuid::Uuid,
    state: &str,
    text: Option<&str>,
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let owner_id = owner.into_inner();
    let handle = if let Some(handle) = handle {
        sqlx::query("UPDATE proxima_core.goal_head SET t = $2 WHERE handle = $1")
            .bind(handle)
            .bind(t)
            .execute(pg.pool_for_tests())
            .await?;
        handle
    } else {
        let handle = proxima_core::new_memory_uuid(0);
        sqlx::query(
            "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
             VALUES ($1, 'core/simple-text-v1', $2, $3)",
        )
        .bind(handle)
        .bind(owner_id)
        .bind(t)
        .execute(pg.pool_for_tests())
        .await?;
        handle
    };
    sqlx::query(
        "INSERT INTO proxima_core.goal (handle, t, owner_id, title, text, state, request_id, \"by\", via)
         VALUES ($1, $2, $3, 'report', $4, $5::proxima_core.goal_state, $6, $3, NULL)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .bind(text)
    .bind(state)
    .bind(format!("text-read:{t}"))
    .execute(pg.pool_for_tests())
    .await?;
    Ok(handle)
}

/// `sidecars` is the tables this admission's caller then writes a row into.
/// It is the stamp forget, owner erase and owner export reach a sidecar row
/// through, and since the declaration trigger the database refuses an
/// unstamped one outright.
async fn insert_memory_row(
    conn: &mut sqlx::PgConnection,
    owner: &OwnerId,
    kind: &str,
    schema_id: &str,
    origins: &[uuid::Uuid],
    sidecars: &[&str],
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let handle = proxima_core::new_memory_uuid(0);
    let t = proxima_core::new_memory_uuid(0);
    let mut origins = origins.to_vec();
    if kind != "fact" && origins.is_empty() {
        let fact_t = Box::pin(insert_memory_row(
            &mut *conn,
            owner,
            "fact",
            "core/test-fact-v1",
            &[],
            &[],
        ))
        .await?;
        if kind == "perspective" {
            let abs_t = Box::pin(insert_memory_row(
                &mut *conn,
                owner,
                "abstraction",
                "core/test-abs-v1",
                &[fact_t],
                &[],
            ))
            .await?;
            origins.push(abs_t);
        } else {
            origins.push(fact_t);
        }
    }
    let owner_id = owner.into_inner();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal'::proxima_core.owner_kind) ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .execute(&mut *conn)
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
    .execute(&mut *conn)
    .await?;
    let content_id: Option<uuid::Uuid> = if kind == "fact" {
        None
    } else {
        let mut hash = [0_u8; 32];
        hash[..16].copy_from_slice(t.as_bytes());
        hash[16..].copy_from_slice(t.as_bytes());
        Some(
            sqlx::query_scalar(
                "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
                 VALUES ($1, $2, $3)
                 RETURNING content_id",
            )
            .bind(owner_id)
            .bind(schema_id)
            .bind(hash.as_slice())
            .fetch_one(&mut *conn)
            .await?,
        )
    };
    let sidecars: Vec<String> = sidecars.iter().map(|table| (*table).to_owned()).collect();
    sqlx::query(
        "INSERT INTO proxima_core.memory
             (handle, t, kind, owner_id, schema_id, origins, content_id, sidecar_tables, occurred_at, \"by\", via)
         VALUES ($1, $2, $3::proxima_core.memory_kind, $4, $5, $6, $7, $8, CASE WHEN $3::proxima_core.memory_kind = 'fact' THEN proxima_core.memory_created_at($2) END, $4, NULL)",
    )
    .bind(handle)
    .bind(t)
    .bind(kind)
    .bind(owner_id)
    .bind(schema_id)
    .bind(origins)
    .bind(content_id)
    .bind(&sidecars)
    .execute(&mut *conn)
    .await?;
    Ok(t)
}

async fn insert_memory(
    pg: &PgStorage,
    owner: &OwnerId,
    text: &str,
    origins: &[uuid::Uuid],
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    // The stamp and the row it promises land in one transaction: see above.
    let mut stamped: sqlx::Transaction<'static, sqlx::Postgres> =
        sqlx::Acquire::begin(pg.pool_for_tests()).await?;
    let t = insert_memory_row(
        &mut stamped,
        owner,
        "abstraction",
        "core/agent-derivation-v1",
        origins,
        &["proxima_core.agent_derivation_v1"],
    )
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.agent_derivation_v1
            (t, title, body, tags, source_memory_ids,
             model_id, client_name, client_version)
         VALUES ($1, $2, $2, ARRAY[]::text[], ARRAY[]::uuid[],
                 'test-model', 'test', 'test-v1')",
    )
    .bind(t)
    .bind(text)
    .execute(&mut *stamped)
    .await?;
    stamped.commit().await?;
    Ok(t)
}

fn author_ctx() -> McpAuthorContext {
    McpAuthorContext {
        model_id: "codex-test".into(),
        trusted_model_id: None,
        client_name: "codex".into(),
        client_version: "1".into(),
        caller_self_perspective: None,
    }
}

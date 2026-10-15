//! Issue #462 slice 3: the `OtherStores` port, end to end against Postgres
//! with an in-memory fake store.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use proxima::{FlavorServices, Proxima, ToolScope};
use proxima_core::storage_ports::other_stores::{
    OtherStores, OtherStoresError, OtherStoresService, RemoteHit, RemoteMemory, RemoteRead,
    RemoteSearch, StoreCaller, StoreInfo, StoreName,
};
use proxima_core::verbs::query::EntityKind;
use proxima_core::{OwnerId, Role, UserId};
use uuid::Uuid;

use super::split_core_db::clone_split_core_db;
use super::{AgentMemoryApp, call_test_model_tool, read_test_model_resource, space_authz};

#[derive(Clone)]
enum Entry {
    Readable(RemoteMemory),
    Denied,
    Erased,
}

#[derive(Default)]
struct FakeStore {
    entries: BTreeMap<String, Entry>,
    delay: Option<Duration>,
    schema_ids: Vec<String>,
}

/// In-memory stores. A `Denied` entry answers `NotFound`, exactly as a
/// store hides what the caller may not read.
#[derive(Default)]
struct FakeStores {
    stores: Mutex<BTreeMap<String, FakeStore>>,
}

fn fact(handle: &str, body: &str) -> RemoteMemory {
    RemoteMemory {
        handle: handle.to_string(),
        kind: EntityKind::Fact,
        schema_id: "remote/note".into(),
        title: Some(format!("title of {handle}")),
        body: body.to_string(),
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        tags: vec!["remote".into()],
    }
}

impl FakeStores {
    fn with(self, store: &str, handle: &str, entry: Entry) -> Self {
        self.stores
            .lock()
            .expect("lock")
            .entry(store.to_string())
            .or_default()
            .entries
            .insert(handle.to_string(), entry);
        self
    }

    fn slow(self, store: &str, delay: Duration) -> Self {
        self.stores
            .lock()
            .expect("lock")
            .entry(store.to_string())
            .or_default()
            .delay = Some(delay);
        self
    }

    fn erase(&self, store: &str, handle: &str) {
        self.stores
            .lock()
            .expect("lock")
            .get_mut(store)
            .expect("store")
            .entries
            .insert(handle.to_string(), Entry::Erased);
    }
}

impl OtherStores for FakeStores {
    fn stores<'a>(
        &'a self,
        _caller: StoreCaller<'a>,
    ) -> BoxFuture<'a, Result<Vec<StoreInfo>, OtherStoresError>> {
        Box::pin(async move {
            Ok(self
                .stores
                .lock()
                .expect("lock")
                .iter()
                .map(|(name, store)| StoreInfo {
                    name: StoreName::new(name.clone()).expect("store name"),
                    description: format!("fake {name}"),
                    schema_ids: store.schema_ids.clone(),
                })
                .collect())
        })
    }

    fn search<'a>(
        &'a self,
        _caller: StoreCaller<'a>,
        store: &'a StoreName,
        args: &'a RemoteSearch,
    ) -> BoxFuture<'a, Result<Vec<RemoteHit>, OtherStoresError>> {
        Box::pin(async move {
            let (delay, hits) = {
                let stores = self.stores.lock().expect("lock");
                let fake = stores
                    .get(store.as_str())
                    .ok_or_else(|| OtherStoresError::UnknownStore(store.to_string()))?;
                let hits: Vec<RemoteHit> = fake
                    .entries
                    .values()
                    .filter_map(|entry| match entry {
                        Entry::Readable(memory) if memory.body.contains(&args.query) => {
                            Some(RemoteHit {
                                handle: memory.handle.clone(),
                                kind: memory.kind,
                                schema_id: memory.schema_id.clone(),
                                snippet: memory.body.clone(),
                                created_at: memory.created_at,
                                tags: memory.tags.clone(),
                            })
                        }
                        _ => None,
                    })
                    .collect();
                (fake.delay, hits)
            };
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            Ok(hits)
        })
    }

    fn read<'a>(
        &'a self,
        _caller: StoreCaller<'a>,
        store: &'a StoreName,
        handle: &'a str,
    ) -> BoxFuture<'a, Result<RemoteRead, OtherStoresError>> {
        Box::pin(async move {
            let stores = self.stores.lock().expect("lock");
            let entry = stores
                .get(store.as_str())
                .and_then(|fake| fake.entries.get(handle).cloned());
            Ok(match entry {
                Some(Entry::Readable(memory)) => RemoteRead::Found(memory),
                Some(Entry::Erased) => RemoteRead::Erased,
                Some(Entry::Denied) | None => RemoteRead::NotFound,
            })
        })
    }
}

/// Rows of the derived-memory tables, counted as the platform role.
async fn row_counts(platform_url: &str) -> (i64, i64) {
    let pool = sqlx::PgPool::connect(platform_url)
        .await
        .expect("platform pool");
    let mut tx = pool.begin().await.expect("tx");
    sqlx::query("SELECT set_config('app.proxima_scope', 'platform', true)")
        .execute(&mut *tx)
        .await
        .expect("platform scope");
    let memories: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.memory")
        .fetch_one(&mut *tx)
        .await
        .expect("count memory");
    let external: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.memory_external_origin")
            .fetch_one(&mut *tx)
            .await
            .expect("count external origins");
    tx.rollback().await.expect("rollback");
    pool.close().await;
    (memories, external)
}

fn node<'a>(lineage: &'a serde_json::Value, memory: &str) -> Option<&'a serde_json::Value> {
    lineage["nodes"]
        .as_array()?
        .iter()
        .find(|node| node["memory"] == memory)
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one end-to-end story: refuse, derive, walk, erase
async fn derive_from_a_local_and_a_remote_fact_names_both_and_survives_remote_erasure() {
    let split_db = clone_split_core_db("proxima_other_stores_derive")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let fake = Arc::new(
            FakeStores::default()
                .with(
                    "wiki",
                    "fact-1",
                    Entry::Readable(fact("fact-1", "remote basis")),
                )
                .with("wiki", "secret", Entry::Denied),
        );
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone())
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .tool_scope(ToolScope::All)
            .other_stores(fake.clone())
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());

        let local = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({"title":"local fact","body":"local basis","tags":[]}),
        )
        .await?;
        let local_handle = local["handle"].as_str().expect("handle").to_string();

        // A remote Fact the caller cannot read refuses the whole derive
        // before any write.
        let before = row_counts(&platform_url).await;
        let refused = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_derive",
            serde_json::json!({
                "kind": "Abstraction",
                "title": "unreadable basis",
                "body": "must not land",
                "source_handles": [local_handle, "wiki:secret"],
                "model_id": "test-model"
            }),
        )
        .await
        .expect_err("an unreadable remote origin refuses");
        assert!(refused.to_string().contains("wiki:secret"), "{refused}");
        assert_eq!(row_counts(&platform_url).await, before, "zero rows written");

        // Remote sources alone are not a derivation of this store.
        let alone = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_derive",
            serde_json::json!({
                "kind": "Abstraction",
                "title": "remote only",
                "body": "must not land either",
                "source_handles": ["wiki:fact-1"],
                "model_id": "test-model"
            }),
        )
        .await
        .expect_err("remote-only sources refuse");
        assert!(
            alone
                .to_string()
                .contains("at least one memory of this store"),
            "{alone}"
        );
        assert_eq!(row_counts(&platform_url).await, before);

        let derived = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_derive",
            serde_json::json!({
                "kind": "Abstraction",
                "title": "cross-store pattern",
                "body": "local and remote agree",
                "source_handles": [local_handle, "wiki:fact-1"],
                "model_id": "test-model"
            }),
        )
        .await?;
        let abstraction = derived["handle"].as_str().expect("handle").to_string();
        assert_eq!(derived["edge_count"], serde_json::json!(2), "{derived}");
        assert_eq!(row_counts(&platform_url).await.1, before.1 + 1);

        let lineage_uri = format!("proxima://memory/{abstraction}/lineage");
        let lineage = read_test_model_resource(&tools, authz.clone(), owner, &lineage_uri).await?;
        assert!(node(&lineage, &local_handle).is_some(), "{lineage}");
        let remote = node(&lineage, "wiki:fact-1").expect("remote origin node");
        assert_eq!(remote["store"], "wiki");
        assert_eq!(remote["kind"], "Fact");
        assert!(remote.get("status").is_none(), "{remote}");
        let edges = lineage["edges"].as_array().expect("edges");
        for target in [local_handle.as_str(), "wiki:fact-1"] {
            assert!(
                edges
                    .iter()
                    .any(|edge| edge["source"] == abstraction.as_str() && edge["target"] == target),
                "edge to {target}: {lineage}"
            );
        }

        // The store erases its Fact: the walk renders it erased and the
        // Abstraction still reads.
        fake.erase("wiki", "fact-1");
        let lineage = read_test_model_resource(&tools, authz.clone(), owner, &lineage_uri).await?;
        assert_eq!(
            node(&lineage, "wiki:fact-1").expect("remote node")["status"],
            "erased"
        );
        let read = read_test_model_resource(
            &tools,
            authz.clone(),
            owner,
            &format!("proxima://memory/{abstraction}"),
        )
        .await?;
        assert_eq!(read["body"], "local and remote agree");

        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("cross-store derive test failed");
}

#[tokio::test]
async fn search_fans_out_tags_stores_and_reports_the_slow_one_unanswered() {
    let split_db = clone_split_core_db("proxima_other_stores_search")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let fake = Arc::new(
            FakeStores::default()
                .with(
                    "wiki",
                    "w1",
                    Entry::Readable(fact("w1", "zebra crossing in the wiki")),
                )
                .with(
                    "crm",
                    "c1",
                    Entry::Readable(fact("c1", "zebra account in the crm")),
                )
                .with(
                    "slow",
                    "s1",
                    Entry::Readable(fact("s1", "zebra that never arrives")),
                )
                .slow("slow", Duration::from_secs(5)),
        );
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone())
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .tool_scope(ToolScope::All)
            .services(FlavorServices::with(
                OtherStoresService::new(fake).with_timeout(Duration::from_millis(300)),
            ))
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({"title":"zebra","body":"a local zebra fact","tags":[]}),
        )
        .await?;

        let found = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_search_memories",
            serde_json::json!({"query":"zebra","mode":"lexical"}),
        )
        .await?;
        let memories = found["memories"].as_array().expect("memories");
        let stores: std::collections::BTreeSet<&str> = memories
            .iter()
            .map(|hit| hit["store"].as_str().expect("every hit names its store"))
            .collect();
        assert_eq!(
            stores,
            ["crm", "local", "wiki"].into_iter().collect(),
            "{found}"
        );
        assert!(memories.iter().any(|hit| hit["memory"] == "wiki:w1"));
        assert_eq!(found["unanswered"], serde_json::json!(["slow"]), "{found}");
        assert_eq!(found["ranking"], "rank");

        // An explicit selection searches only what it names.
        let only_wiki = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_search_memories",
            serde_json::json!({"query":"zebra","mode":"lexical","stores":["wiki"]}),
        )
        .await?;
        let memories = only_wiki["memories"].as_array().expect("memories");
        assert_eq!(memories.len(), 1, "{only_wiki}");
        assert_eq!(memories[0]["memory"], "wiki:w1");

        // `proxima://memory/<store>:<handle>` reads through the port.
        let remote =
            read_test_model_resource(&tools, authz.clone(), owner, "proxima://memory/crm:c1")
                .await?;
        assert_eq!(remote["handle"], "crm:c1");
        assert_eq!(remote["space"], "crm");
        assert_eq!(remote["body"], "zebra account in the crm");

        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("search fan-out test failed");
}

#[tokio::test]
async fn without_a_port_qualified_handles_and_other_stores_are_refused() {
    let split_db = clone_split_core_db("proxima_other_stores_absent")
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
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        let local = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({"title":"zebra","body":"a local zebra fact","tags":[]}),
        )
        .await?;

        let refused = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_derive",
            serde_json::json!({
                "kind": "Abstraction",
                "title": "t",
                "body": "b",
                "source_handles": [local["handle"], "wiki:fact-1"],
                "model_id": "test-model"
            }),
        )
        .await
        .expect_err("a qualified handle without a port refuses");
        assert!(
            refused.to_string().contains("expected memory id prefix"),
            "{refused}"
        );

        let found = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_search_memories",
            serde_json::json!({"query":"zebra","mode":"lexical"}),
        )
        .await?;
        assert!(found.get("unanswered").is_none(), "{found}");
        assert!(found["memories"][0].get("store").is_none(), "{found}");
        call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_search_memories",
            serde_json::json!({"query":"zebra","mode":"lexical","stores":["local"]}),
        )
        .await?;
        let unknown = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_search_memories",
            serde_json::json!({"query":"zebra","stores":["wiki"]}),
        )
        .await
        .expect_err("no port, no other store");
        assert!(
            unknown.to_string().contains("unknown store 'wiki'"),
            "{unknown}"
        );
        read_test_model_resource(&tools, authz, owner, "proxima://memory/wiki:abc")
            .await
            .expect_err("no port, no remote read");

        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("absent-port test failed");
}

#[tokio::test]
async fn interpret_about_a_remote_memory_reads_it_first_and_walks_it_through_the_port() {
    let split_db = clone_split_core_db("proxima_other_stores_interpret")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let fake = Arc::new(
            FakeStores::default()
                .with(
                    "wiki",
                    "page-1",
                    Entry::Readable(fact("page-1", "the remote subject")),
                )
                .with("wiki", "secret", Entry::Denied),
        );
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone())
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .tool_scope(ToolScope::All)
            .other_stores(fake.clone())
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        let local = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({"title":"local subject","body":"local subject body","tags":[]}),
        )
        .await?;
        let local_handle = local["handle"].as_str().expect("handle").to_string();

        let before = row_counts(&platform_url).await;
        for subjects in [
            serde_json::json!([local_handle, "wiki:secret"]),
            serde_json::json!(["wiki:page-1"]),
        ] {
            call_test_model_tool(
                &tools,
                authz.clone(),
                owner,
                "core_interpret",
                serde_json::json!({
                    "claim": "must not land",
                    "confidence": 70,
                    "subjects": subjects,
                    "model_id": "test-model"
                }),
            )
            .await
            .expect_err("unreadable or remote-only subjects refuse");
            assert_eq!(row_counts(&platform_url).await, before, "zero rows written");
        }

        let interpreted = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_interpret",
            serde_json::json!({
                "claim": "both say the same thing",
                "confidence": 80,
                "subjects": [local_handle, "wiki:page-1"],
                "model_id": "test-model"
            }),
        )
        .await?;
        let perspective = interpreted["handle"].as_str().expect("handle").to_string();
        assert!(perspective.starts_with("P:"), "{interpreted}");
        assert_eq!(row_counts(&platform_url).await.1, before.1 + 1);

        let lineage_uri = format!("proxima://memory/{perspective}/lineage");
        let lineage = read_test_model_resource(&tools, authz.clone(), owner, &lineage_uri).await?;
        let remote = node(&lineage, "wiki:page-1").expect("remote subject node");
        assert_eq!(remote["store"], "wiki");
        assert!(remote.get("status").is_none(), "{remote}");

        fake.erase("wiki", "page-1");
        let lineage = read_test_model_resource(&tools, authz.clone(), owner, &lineage_uri).await?;
        assert_eq!(
            node(&lineage, "wiki:page-1").expect("remote node")["status"],
            "erased"
        );
        read_test_model_resource(
            &tools,
            authz,
            owner,
            &format!("proxima://memory/{perspective}"),
        )
        .await?;

        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("cross-store interpret test failed");
}

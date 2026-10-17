//! Issue #462 slice 3: the `OtherStores` port, end to end against Postgres
//! with an in-memory fake store.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use proxima::host::{
    OtherStores, OtherStoresError, OtherStoresService, RemoteHit, RemoteMemory, RemoteRead,
    RemoteSearch, RemoteSearchResult, StoreCaller, StoreInfo,
};
use proxima::{FlavorServices, Proxima};
use proxima_core::verbs::query::{
    EntityKind, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
};
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
    entries: BTreeMap<Uuid, Entry>,
    name: String,
    delay: Option<Duration>,
    schema_ids: Vec<String>,
    degraded_to_lexical: bool,
    unsupported_search: Option<String>,
}

/// In-memory stores. A `Denied` entry answers `NotFound`, exactly as a
/// store hides what the caller may not read.
#[derive(Default)]
struct FakeStores {
    stores: Mutex<BTreeMap<u16, FakeStore>>,
    searches: Mutex<Vec<(u16, RemoteSearch)>>,
}

fn remote_id(store: u16, key: &str) -> Uuid {
    let mut bytes = *Uuid::new_v5(&Uuid::NAMESPACE_OID, key.as_bytes()).as_bytes();
    bytes[10..12].copy_from_slice(&store.to_be_bytes());
    Uuid::from_bytes(bytes)
}

fn fact(handle: Uuid, body: &str) -> RemoteMemory {
    RemoteMemory {
        handle,
        kind: EntityKind::Fact,
        schema_id: "remote/note".into(),
        title: Some(format!("title of {handle}")),
        body: body.to_string(),
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        tags: vec!["remote".into()],
    }
}

fn dated_fact(handle: Uuid, body: &str, day: i64) -> RemoteMemory {
    RemoteMemory {
        created_at: time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(day),
        ..fact(handle, body)
    }
}

impl FakeStores {
    fn set_search_outcome(&self, store: u16, degraded: bool, refusal: Option<&str>) {
        let mut stores = self.stores.lock().expect("stores");
        let store = stores.get_mut(&store).expect("configured fake store");
        store.degraded_to_lexical = degraded;
        store.unsupported_search = refusal.map(str::to_owned);
    }

    fn searches(&self) -> Vec<(u16, RemoteSearch)> {
        self.searches.lock().expect("searches").clone()
    }

    fn with(self, store: u16, handle: Uuid, entry: Entry) -> Self {
        self.stores
            .lock()
            .expect("lock")
            .entry(store)
            .or_default()
            .entries
            .insert(handle, entry);
        self
    }

    fn slow(self, store: u16, delay: Duration) -> Self {
        self.stores
            .lock()
            .expect("lock")
            .entry(store)
            .or_default()
            .delay = Some(delay);
        self
    }

    fn rename(&self, store: u16, name: &str) {
        self.stores
            .lock()
            .expect("lock")
            .get_mut(&store)
            .expect("store")
            .name = name.into();
    }

    fn erase(&self, store: u16, handle: Uuid) {
        self.stores
            .lock()
            .expect("lock")
            .get_mut(&store)
            .expect("store")
            .entries
            .insert(handle, Entry::Erased);
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
                .map(|(number, store)| StoreInfo {
                    number: *number,
                    name: store.name.clone(),
                    description: format!("fake store {number}"),
                    schema_ids: store.schema_ids.clone(),
                })
                .collect())
        })
    }

    fn search<'a>(
        &'a self,
        _caller: StoreCaller<'a>,
        store: u16,
        args: &'a RemoteSearch,
    ) -> BoxFuture<'a, Result<RemoteSearchResult, OtherStoresError>> {
        Box::pin(async move {
            self.searches
                .lock()
                .expect("searches")
                .push((store, args.clone()));
            let (delay, mut hits, degraded_to_lexical) = {
                let stores = self.stores.lock().expect("lock");
                let fake = stores
                    .get(&store)
                    .ok_or(OtherStoresError::UnknownStore(store))?;
                if let Some(reason) = &fake.unsupported_search {
                    return Err(OtherStoresError::UnsupportedSearch(reason.clone()));
                }
                let hits: Vec<RemoteHit> = fake
                    .entries
                    .values()
                    .filter_map(|entry| match entry {
                        Entry::Readable(memory) if remote_matches(memory, args) => {
                            Some(RemoteHit {
                                handle: memory.handle,
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
                (fake.delay, hits, fake.degraded_to_lexical)
            };
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            if matches!(args.order, SearchOrder::Recency) {
                hits.sort_by_key(|hit| std::cmp::Reverse(hit.created_at));
            }
            Ok(RemoteSearchResult {
                hits,
                degraded_to_lexical,
            })
        })
    }

    fn read<'a>(
        &'a self,
        _caller: StoreCaller<'a>,
        handle: Uuid,
    ) -> BoxFuture<'a, Result<RemoteRead, OtherStoresError>> {
        Box::pin(async move {
            let stores = self.stores.lock().expect("lock");
            let entry = stores
                .get(&proxima_core::memory_store_number(handle))
                .and_then(|fake| fake.entries.get(&handle).cloned());
            Ok(match entry {
                Some(Entry::Readable(memory)) => RemoteRead::Found(memory),
                Some(Entry::Erased) => RemoteRead::Erased,
                Some(Entry::Denied) | None => RemoteRead::NotFound,
            })
        })
    }
}

fn remote_matches(memory: &RemoteMemory, args: &RemoteSearch) -> bool {
    memory.body.contains(&args.query)
        && args.kind.is_none_or(|kind| memory.kind == kind)
        && args
            .schema_id
            .as_ref()
            .is_none_or(|schema| &memory.schema_id == schema)
        && args.since.is_none_or(|since| memory.created_at >= since)
        && args.until.is_none_or(|until| memory.created_at <= until)
        && (args.tags.is_empty()
            || match args.tag_match {
                TagMatch::Any => args.tags.iter().any(|tag| memory.tags.contains(tag)),
                TagMatch::All => args.tags.iter().all(|tag| memory.tags.contains(tag)),
            })
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

fn node(lineage: &serde_json::Value, memory: impl AsRef<str>) -> Option<&serde_json::Value> {
    lineage["nodes"]
        .as_array()?
        .iter()
        .find(|node| node["memory"] == memory.as_ref())
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
                    1,
                    remote_id(1, "fact-1"),
                    Entry::Readable(fact(remote_id(1, "fact-1"), "remote basis")),
                )
                .with(1, remote_id(1, "secret"), Entry::Denied),
        );
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
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
                "source_handles": [local_handle, remote_id(1, "secret").to_string()],
                "model_id": "test-model"
            }),
        )
        .await
        .expect_err("an unreadable remote origin refuses");
        assert!(
            refused
                .to_string()
                .contains(&remote_id(1, "secret").to_string()),
            "{refused}"
        );
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
                "source_handles": [remote_id(1, "fact-1").to_string()],
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
                "source_handles": [local_handle, remote_id(1, "fact-1").to_string()],
                "model_id": "test-model"
            }),
        )
        .await?;
        let abstraction = derived["handle"].as_str().expect("handle").to_string();
        assert_eq!(derived["edge_count"], serde_json::json!(2), "{derived}");
        assert_eq!(row_counts(&platform_url).await.1, before.1 + 1);

        fake.rename(1, "renamed-wiki");
        let lineage_uri = format!("proxima://memory/{abstraction}/lineage");
        let lineage = read_test_model_resource(&tools, authz.clone(), owner, &lineage_uri).await?;
        assert!(node(&lineage, &local_handle).is_some(), "{lineage}");
        let remote =
            node(&lineage, remote_id(1, "fact-1").to_string()).expect("remote origin node");
        assert_eq!(remote["store"], 1);
        assert_eq!(remote["kind"], "Fact");
        assert!(remote.get("status").is_none(), "{remote}");
        let edges = lineage["edges"].as_array().expect("edges");
        for target in [local_handle.clone(), remote_id(1, "fact-1").to_string()] {
            assert!(
                edges
                    .iter()
                    .any(|edge| edge["source"] == abstraction.as_str() && edge["target"] == target),
                "edge to {target}: {lineage}"
            );
        }

        // The store erases its Fact: the walk renders it erased and the
        // Abstraction still reads.
        fake.erase(1, remote_id(1, "fact-1"));
        let lineage = read_test_model_resource(&tools, authz.clone(), owner, &lineage_uri).await?;
        assert_eq!(
            node(&lineage, remote_id(1, "fact-1").to_string()).expect("remote node")["status"],
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

async fn assert_remote_and_batch_reads(
    tools: &proxima::CoreMcpTools,
    authz: proxima::AuthzContext,
    owner: OwnerId,
    local: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // The UUID alone routes the read through the port.
    let remote = read_test_model_resource(
        tools,
        authz.clone(),
        owner,
        &format!("proxima://memory/{}", remote_id(2, "c1")),
    )
    .await?;
    assert_eq!(remote["handle"], remote_id(2, "c1").to_string());
    assert_eq!(remote["space"], "2");
    assert_eq!(remote["body"], "zebra account in the crm");
    let batch = read_test_model_resource(
        tools,
        authz.clone(),
        owner,
        &format!(
            "proxima://memories?ids={},{},{},{}",
            local,
            remote_id(1, "w1"),
            remote_id(2, "c1"),
            remote_id(2, "absent")
        ),
    )
    .await?;
    assert_eq!(batch["memories"].as_array().expect("memories").len(), 3);
    assert_eq!(batch["memories"][0]["memory"], local);
    assert_eq!(
        batch["memories"][1]["memory"],
        remote_id(1, "w1").to_string()
    );
    assert_eq!(
        batch["memories"][2]["memory"],
        remote_id(2, "c1").to_string()
    );
    assert_eq!(
        batch["missing"],
        serde_json::json!([remote_id(2, "absent")])
    );

    Ok(())
}

#[tokio::test]
async fn search_fans_out_tags_stores_and_reports_the_slow_one_unanswered() {
    let split_db = clone_split_core_db("proxima_other_stores_search")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let fake = Arc::new(
            FakeStores::default()
                .with(
                    1,
                    remote_id(1, "w1"),
                    Entry::Readable(dated_fact(
                        remote_id(1, "w1"),
                        "zebra crossing in the wiki",
                        3,
                    )),
                )
                .with(
                    2,
                    remote_id(2, "c1"),
                    Entry::Readable(dated_fact(
                        remote_id(2, "c1"),
                        "zebra account in the crm",
                        2,
                    )),
                )
                .with(
                    3,
                    remote_id(3, "s1"),
                    Entry::Readable(fact(remote_id(3, "s1"), "zebra that never arrives")),
                )
                .slow(3, Duration::from_secs(5)),
        );
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(split_db.runtime_url())
            .store_number(0)
            .platform_database_url(split_db.platform_url())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .services(FlavorServices::with(
                OtherStoresService::new(fake.clone()).with_timeout(Duration::from_millis(300)),
            ))
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

        let found = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_search_memories",
            serde_json::json!({"query":"zebra","mode":"lexical"}),
        )
        .await?;
        let memories = found["memories"].as_array().expect("memories");
        let stores: std::collections::BTreeSet<u64> = memories
            .iter()
            .map(|hit| hit["store"].as_u64().expect("every hit names its store"))
            .collect();
        assert_eq!(stores, [0, 1, 2].into_iter().collect(), "{found}");
        assert!(
            memories
                .iter()
                .any(|hit| hit["memory"] == remote_id(1, "w1").to_string())
        );
        assert_eq!(found["unanswered"], serde_json::json!([3]), "{found}");
        assert_eq!(found["ranking"], "rank");

        // An explicit selection searches only what it names.
        let only_wiki = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_search_memories",
            serde_json::json!({"query":"zebra","mode":"lexical","stores":[1]}),
        )
        .await?;
        let memories = only_wiki["memories"].as_array().expect("memories");
        assert_eq!(memories.len(), 1, "{only_wiki}");
        assert_eq!(memories[0]["memory"], remote_id(1, "w1").to_string());

        assert_remote_and_batch_reads(
            &tools,
            authz.clone(),
            owner,
            local["handle"].as_str().expect("local UUID"),
        )
        .await?;

        assert_remote_search_intent(&tools, &authz, owner, &fake).await?;

        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("search fan-out test failed");
}

async fn assert_remote_search_intent(
    tools: &proxima::CoreMcpTools,
    authz: &proxima_core::AuthzContext,
    owner: OwnerId,
    fake: &FakeStores,
) -> Result<(), Box<dyn std::error::Error>> {
    fake.set_search_outcome(1, true, None);
    let before = fake.searches().len();
    let found = call_test_model_tool(
        tools,
        authz.clone(),
        owner,
        "core_search_memories",
        serde_json::json!({
            "query":"zebra", "mode":"hybrid", "limit":1, "kind":"Fact",
            "schema_id":"remote/note", "tags":[" ReMoTe ","remote"], "tag_match":"all",
            "since":"1970-01-02T00:00:00Z", "until":"1970-01-05T00:00:00Z",
            "order":"recency", "supersession":"all", "min_score":0.2,
            "semantic_weight":0.7, "stores":[2,1]
        }),
    )
    .await?;
    assert_eq!(found["memories"].as_array().unwrap().len(), 1);
    assert_eq!(
        found["memories"][0]["memory"],
        remote_id(1, "w1").to_string()
    );
    assert_eq!(found["degraded_to_lexical"], true);
    let expected = RemoteSearch {
        query: "zebra".into(),
        limit: 1,
        kind: Some(EntityKind::Fact),
        schema_id: Some("remote/note".into()),
        tags: vec!["remote".into()],
        tag_match: TagMatch::All,
        mode: SearchMode::Hybrid,
        since: Some(time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(1)),
        until: Some(time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(4)),
        order: SearchOrder::Recency,
        supersession: SupersessionStatus::IncludeSuperseded,
        min_score: Some(0.2),
        semantic_weight: Some(0.7),
    };
    let requests = fake.searches();
    assert_eq!(requests.len() - before, 2);
    for (_, request) in &requests[before..] {
        assert_eq!(request, &expected);
    }
    let empty = call_test_model_tool(
        tools,
        authz.clone(),
        owner,
        "core_search_memories",
        serde_json::json!({"query":"absent phrase", "mode":"hybrid", "stores":[1]}),
    )
    .await?;
    assert_eq!(empty["memories"], serde_json::json!([]));
    assert_eq!(empty["degraded_to_lexical"], true);

    fake.set_search_outcome(1, false, Some("tag filter unsupported"));
    let before = fake.searches().len();
    let refused = call_test_model_tool(
        tools,
        authz.clone(),
        owner,
        "core_search_memories",
        serde_json::json!({"query":"zebra", "mode":"lexical", "tags":["remote"], "stores":[1]}),
    )
    .await?;
    assert_eq!(refused["memories"], serde_json::json!([]));
    assert_eq!(refused["unanswered"], serde_json::json!([1]));
    assert_eq!(fake.searches().len(), before + 1, "no weaker retry");
    Ok(())
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
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
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
                "source_handles": [local["handle"], remote_id(1, "fact-1").to_string()],
                "model_id": "test-model"
            }),
        )
        .await
        .expect_err("a qualified handle without a port refuses");
        assert!(refused.to_string().contains("not found"), "{refused}");

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
            serde_json::json!({"query":"zebra","mode":"lexical","stores":[0]}),
        )
        .await?;
        let unknown = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_search_memories",
            serde_json::json!({"query":"zebra","stores":[1]}),
        )
        .await
        .expect_err("no port, no other store");
        assert!(
            unknown.to_string().contains("unknown store '1'"),
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
                    1,
                    remote_id(1, "page-1"),
                    Entry::Readable(fact(remote_id(1, "page-1"), "the remote subject")),
                )
                .with(1, remote_id(1, "secret"), Entry::Denied),
        );
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
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
            serde_json::json!([local_handle, remote_id(1, "secret").to_string()]),
            serde_json::json!([remote_id(1, "page-1").to_string()]),
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
                "subjects": [local_handle, remote_id(1, "page-1").to_string()],
                "model_id": "test-model"
            }),
        )
        .await?;
        let perspective = interpreted["handle"].as_str().expect("handle").to_string();
        assert!(perspective.parse::<Uuid>().is_ok(), "{interpreted}");
        assert_eq!(row_counts(&platform_url).await.1, before.1 + 1);

        let lineage_uri = format!("proxima://memory/{perspective}/lineage");
        let lineage = read_test_model_resource(&tools, authz.clone(), owner, &lineage_uri).await?;
        let remote =
            node(&lineage, remote_id(1, "page-1").to_string()).expect("remote subject node");
        assert_eq!(remote["store"], 1);
        assert!(remote.get("status").is_none(), "{remote}");

        fake.erase(1, remote_id(1, "page-1"));
        let lineage = read_test_model_resource(&tools, authz.clone(), owner, &lineage_uri).await?;
        assert_eq!(
            node(&lineage, remote_id(1, "page-1").to_string()).expect("remote node")["status"],
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

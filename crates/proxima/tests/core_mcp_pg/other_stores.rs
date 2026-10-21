//! Issue #462 slice 3: the `OtherStores` port, end to end against Postgres
//! with an in-memory fake store.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use proxima::host::{
    OtherStores, OtherStoresError, OtherStoresService, ReadAt, RemoteHit, RemoteMemory, RemoteRead,
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
    /// The store fails the read instead of answering it.
    Unavailable,
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
        series: handle,
        writer: proxima_core::WriterAttribution {
            by: Uuid::now_v7(),
            via: None,
        },
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

    fn put(&self, store: u16, handle: Uuid, entry: Entry) {
        self.stores
            .lock()
            .expect("lock")
            .get_mut(&store)
            .expect("store")
            .entries
            .insert(handle, entry);
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
                                writer: memory.writer,
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
        at: ReadAt,
    ) -> BoxFuture<'a, Result<RemoteRead, OtherStoresError>> {
        Box::pin(async move {
            let stores = self.stores.lock().expect("lock");
            let fake = stores.get(&proxima_core::memory_store_number(handle));
            let entry = fake.and_then(|fake| fake.entries.get(&handle).cloned());
            if matches!(entry, Some(Entry::Unavailable)) {
                return Err(OtherStoresError::Unavailable("store is down".into()));
            }
            Ok(match entry {
                Some(Entry::Readable(memory)) => RemoteRead::Found(match at {
                    ReadAt::Version => memory,
                    // The newest version of the series the caller may read.
                    ReadAt::SeriesHead => fake
                        .into_iter()
                        .flat_map(|fake| fake.entries.values())
                        .filter_map(|entry| match entry {
                            Entry::Readable(version) if version.series == memory.series => {
                                Some(version)
                            }
                            _ => None,
                        })
                        .max_by_key(|version| (version.created_at, version.handle))
                        .cloned()
                        .unwrap_or(memory),
                }),
                Some(Entry::Erased) => RemoteRead::Erased,
                Some(Entry::Denied | Entry::Unavailable) | None => RemoteRead::NotFound,
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

/// The edge kind recorded for `(t, memory)` in `memory_external_origin`,
/// read as the platform role (#480).
async fn external_edge_kind(platform_url: &str, t: &str, memory: Uuid) -> String {
    let pool = sqlx::PgPool::connect(platform_url)
        .await
        .expect("platform pool");
    let mut tx = pool.begin().await.expect("tx");
    sqlx::query("SELECT set_config('app.proxima_scope', 'platform', true)")
        .execute(&mut *tx)
        .await
        .expect("platform scope");
    let kind: String = sqlx::query_scalar(
        "SELECT edge_kind FROM proxima_core.memory_external_origin WHERE t = $1 AND memory = $2",
    )
    .bind(t.parse::<Uuid>().expect("t"))
    .bind(memory)
    .fetch_one(&mut *tx)
    .await
    .expect("external edge kind");
    tx.rollback().await.expect("rollback");
    pool.close().await;
    kind
}

async fn set_external_edge_kind(platform_url: &str, t: &str, memory: Uuid, kind: &str) {
    let pool = sqlx::PgPool::connect(platform_url)
        .await
        .expect("platform pool");
    let mut tx = pool.begin().await.expect("tx");
    sqlx::query("SELECT set_config('app.proxima_scope', 'platform', true)")
        .execute(&mut *tx)
        .await
        .expect("platform scope");
    sqlx::query(
        "UPDATE proxima_core.memory_external_origin SET edge_kind = $3 WHERE t = $1 AND memory = $2",
    )
    .bind(t.parse::<Uuid>().expect("t"))
    .bind(memory)
    .bind(kind)
    .execute(&mut *tx)
    .await
    .expect("flip edge kind");
    tx.commit().await.expect("commit");
    pool.close().await;
}

fn edge_kind(lineage: &serde_json::Value, source: &str, target: Uuid) -> Option<String> {
    lineage["edges"]
        .as_array()?
        .iter()
        .find(|edge| edge["source"] == source && edge["target"] == target.to_string())
        .and_then(|edge| edge["kind"].as_str().map(str::to_owned))
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
        // #480: a remote source of a derivation is an origin.
        assert_eq!(
            external_edge_kind(&platform_url, &abstraction, remote_id(1, "fact-1")).await,
            "origin"
        );
        assert_eq!(
            edge_kind(&lineage, &abstraction, remote_id(1, "fact-1")).as_deref(),
            Some("origin"),
            "{lineage}"
        );
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
    assert_eq!(batch["unanswered"], serde_json::json!([]));

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
        let slow = serde_json::json!([{"store": 3, "why": "deadline has elapsed"}]);
        assert_eq!(found["unanswered"], slow, "{found}");
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
    let why = "store cannot honor search: tag filter unsupported";
    assert_eq!(
        refused["unanswered"],
        serde_json::json!([{"store": 1, "why": why}])
    );
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
#[allow(clippy::too_many_lines)] // one cross-store interpret walk, end to end
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
        // #480: a remote subject of an interpretation is a reference, as a
        // local subject is.
        assert_eq!(
            external_edge_kind(&platform_url, &perspective, remote_id(1, "page-1")).await,
            "reference"
        );
        assert_eq!(
            edge_kind(&lineage, &perspective, remote_id(1, "page-1")).as_deref(),
            Some("reference"),
            "{lineage}"
        );
        // #480: core_think walks the same ancestor edges: both subjects,
        // each over a reference edge, the remote one a leaf of store 1.
        let think = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_think",
            serde_json::json!({"seeds": [perspective], "direction": "ancestors"}),
        )
        .await?;
        let visit = |handle: &str| {
            think["visits"]
                .as_array()
                .expect("visits")
                .iter()
                .find(|visit| visit["handle"] == handle)
                .cloned()
        };
        let local_visit = visit(&local_handle).expect("local subject visit");
        assert_eq!(local_visit["edge"], "reference", "{think}");
        assert!(local_visit.get("store").is_none(), "{think}");
        let remote_visit = visit(&remote_id(1, "page-1").to_string()).expect("remote visit");
        assert_eq!(remote_visit["edge"], "reference", "{think}");
        assert_eq!(remote_visit["store"], 1, "{think}");
        assert_eq!(remote_visit["kind"], "Fact", "{think}");
        assert_eq!(remote_visit["depth"], 1, "{think}");
        assert!(remote_visit.get("status").is_none(), "{think}");
        assert!(visit(&perspective).expect("seed").get("edge").is_none());
        // #480: lineage ancestors follow the same edges as core_think: the
        // local subject is a reference edge too, and both tools reach the
        // same (target, kind) set from the same seed.
        assert_eq!(
            edge_kind(&lineage, &perspective, local_handle.parse()?).as_deref(),
            Some("reference"),
            "{lineage}"
        );
        assert_eq!(
            lineage_edge_set(&lineage),
            think_edge_set(&think),
            "{lineage} vs {think}"
        );
        assert_replay_pins_external_edge_kinds(
            &tools,
            authz.clone(),
            owner,
            &platform_url,
            &perspective,
            &local_handle,
        )
        .await?;

        fake.erase(1, remote_id(1, "page-1"));
        let lineage = read_test_model_resource(&tools, authz.clone(), owner, &lineage_uri).await?;
        assert_eq!(
            node(&lineage, remote_id(1, "page-1").to_string()).expect("remote node")["status"],
            "erased"
        );
        let think = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_think",
            serde_json::json!({"seeds": [perspective], "direction": "ancestors"}),
        )
        .await?;
        let erased = think["visits"]
            .as_array()
            .expect("visits")
            .iter()
            .find(|visit| visit["handle"] == remote_id(1, "page-1").to_string())
            .cloned()
            .expect("erased remote still visited");
        assert_eq!(erased["status"], "erased", "{think}");
        assert_eq!(erased["kind"], "", "{think}");
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

/// A replay must declare the same external edges, kind included (#480).
async fn assert_replay_pins_external_edge_kinds(
    tools: &proxima::CoreMcpTools,
    authz: proxima::AuthzContext,
    owner: OwnerId,
    platform_url: &str,
    perspective: &str,
    local_handle: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // A replay must declare the same external edges, kind included: the
    // same interpretation replays; with its row's kind flipped it is a
    // conflict, not a silent second meaning.
    let args = serde_json::json!({
        "claim": "both say the same thing",
        "confidence": 80,
        "subjects": [local_handle, remote_id(1, "page-1").to_string()],
        "model_id": "test-model"
    });
    let replay =
        call_test_model_tool(tools, authz.clone(), owner, "core_interpret", args.clone()).await?;
    assert_eq!(replay["handle"], perspective, "{replay}");
    assert_eq!(replay["idempotent_replay"], true, "{replay}");
    set_external_edge_kind(platform_url, perspective, remote_id(1, "page-1"), "origin").await;
    let flipped = call_test_model_tool(tools, authz, owner, "core_interpret", args)
        .await
        .expect_err("a replay whose stored edge kind differs conflicts");
    assert!(flipped.to_string().contains("external edges"), "{flipped}");
    set_external_edge_kind(
        platform_url,
        perspective,
        remote_id(1, "page-1"),
        "reference",
    )
    .await;
    Ok(())
}

/// `(target, kind)` of every lineage edge.
fn lineage_edge_set(lineage: &serde_json::Value) -> std::collections::BTreeSet<(String, String)> {
    lineage["edges"]
        .as_array()
        .expect("edges")
        .iter()
        .map(|edge| {
            (
                edge["target"].as_str().expect("target").to_owned(),
                edge["kind"].as_str().expect("kind").to_owned(),
            )
        })
        .collect()
}

/// `(handle, edge)` of every non-seed `core_think` visit.
fn think_edge_set(think: &serde_json::Value) -> std::collections::BTreeSet<(String, String)> {
    think["visits"]
        .as_array()
        .expect("visits")
        .iter()
        .filter_map(|visit| {
            Some((
                visit["handle"].as_str()?.to_owned(),
                visit["edge"].as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// #480: a two-level ancestors walk over both edge kinds and two remote
/// leaves pages within `limit` (remote nodes counted) with no duplicate or
/// missing edge, and the pages together equal one unpaged walk.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one graph, walked whole and page by page
async fn lineage_ancestors_page_within_limit_over_references_and_remote_leaves() {
    let split_db = clone_split_core_db("proxima_other_stores_lineage_pages")
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
                    remote_id(1, "basis"),
                    Entry::Readable(fact(remote_id(1, "basis"), "remote basis")),
                )
                .with(
                    1,
                    remote_id(1, "subject"),
                    Entry::Readable(fact(remote_id(1, "subject"), "remote subject")),
                ),
        );
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .other_stores(fake)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        let call = |tool: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), owner, tool, args)
        };
        let mut facts = Vec::new();
        for title in ["first fact", "second fact"] {
            let made = call(
                "core_remember",
                serde_json::json!({"title": title, "body": title, "tags": []}),
            )
            .await?;
            facts.push(made["handle"].as_str().expect("handle").to_owned());
        }
        let derived = call(
            "core_derive",
            serde_json::json!({
                "kind": "Abstraction",
                "title": "derived",
                "body": "from two local facts and a remote one",
                "source_handles": [facts[0], facts[1], remote_id(1, "basis").to_string()],
                "model_id": "test-model"
            }),
        )
        .await?;
        let derived = derived["handle"].as_str().expect("handle").to_owned();
        let interpreted = call(
            "core_interpret",
            serde_json::json!({
                "claim": "about the derivation and a remote page",
                "confidence": 60,
                "subjects": [derived, remote_id(1, "subject").to_string()],
                "model_id": "test-model"
            }),
        )
        .await?;
        let perspective = interpreted["handle"].as_str().expect("handle").to_owned();

        let base = format!("proxima://memory/{perspective}/lineage?direction=ancestors&depth=2");
        let whole =
            read_test_model_resource(&tools, authz.clone(), owner, &format!("{base}&limit=50"))
                .await?;
        let expected: std::collections::BTreeSet<(String, String, String)> = [
            (perspective.clone(), derived.clone(), "reference"),
            (
                perspective.clone(),
                remote_id(1, "subject").to_string(),
                "reference",
            ),
            (derived.clone(), facts[0].clone(), "origin"),
            (derived.clone(), facts[1].clone(), "origin"),
            (derived.clone(), remote_id(1, "basis").to_string(), "origin"),
        ]
        .into_iter()
        .map(|(source, target, kind)| (source, target, kind.to_owned()))
        .collect();
        let triples = |page: &serde_json::Value| -> Vec<(String, String, String)> {
            page["edges"]
                .as_array()
                .expect("edges")
                .iter()
                .map(|edge| {
                    (
                        edge["source"].as_str().expect("source").to_owned(),
                        edge["target"].as_str().expect("target").to_owned(),
                        edge["kind"].as_str().expect("kind").to_owned(),
                    )
                })
                .collect()
        };
        assert_eq!(
            triples(&whole)
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            expected,
            "{whole}"
        );
        assert_eq!(whole["has_more"], false, "{whole}");
        let remote_basis = node(&whole, remote_id(1, "basis").to_string()).expect("remote basis");
        assert_eq!(remote_basis["distance"], 2, "{whole}");
        assert_eq!(remote_basis["store"], 1, "{whole}");

        let mut paged = Vec::new();
        let mut cursor: Option<String> = None;
        let mut page_count = 0;
        loop {
            let uri = match &cursor {
                Some(cursor) => format!("{base}&limit=2&cursor={cursor}"),
                None => format!("{base}&limit=2"),
            };
            let page = read_test_model_resource(&tools, authz.clone(), owner, &uri).await?;
            let edges = triples(&page);
            assert!(edges.len() <= 2, "a page holds at most limit edges: {page}");
            for (_, target, _) in &edges {
                // every target, remote or local, is a node of its page
                assert!(node(&page, target).is_some(), "{page}");
            }
            paged.extend(edges);
            page_count += 1;
            cursor = page["next_cursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
            assert!(page_count < 10, "paging must terminate");
        }
        assert_eq!(page_count, 3, "5 edges at limit 2");
        let unique: std::collections::BTreeSet<_> = paged.iter().cloned().collect();
        assert_eq!(unique.len(), paged.len(), "no duplicate edge: {paged:?}");
        assert_eq!(unique, expected, "no missing edge");

        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("lineage paging test failed");
}

/// #480 follow-up: every lineage node carries its real hop distance, as
/// edges do: a three-memory local chain renders 0, 1, 2 toward ancestors
/// and toward descendants, also when the deepest edge comes on its own page.
#[tokio::test]
async fn lineage_nodes_carry_their_hop_distance() {
    let split_db = clone_split_core_db("proxima_lineage_node_distance")
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
                serde_json::json!({"title": "root", "body": "root fact", "tags": []}),
            )
            .await?,
        );
        let mut chain = vec![fact];
        for title in ["first derivation", "second derivation"] {
            let made = call(
                "core_derive",
                serde_json::json!({
                    "kind": "Abstraction",
                    "title": title,
                    "body": title,
                    "source_handles": [chain.last().expect("parent")],
                    "model_id": "test-model"
                }),
            )
            .await?;
            chain.push(handle(made));
        }
        let (root, middle, top) = (&chain[0], &chain[1], &chain[2]);
        let base = format!("proxima://memory/{top}/lineage?direction=ancestors&depth=3");
        let whole = read_test_model_resource(&tools, authz.clone(), owner, &base).await?;
        let distance = |page: &serde_json::Value, memory: &str| {
            node(page, memory).map(|node| node["distance"].clone())
        };
        assert_eq!(distance(&whole, top), Some(0.into()), "{whole}");
        assert_eq!(distance(&whole, middle), Some(1.into()), "{whole}");
        assert_eq!(distance(&whole, root), Some(2.into()), "{whole}");

        let first =
            read_test_model_resource(&tools, authz.clone(), owner, &format!("{base}&limit=1"))
                .await?;
        let cursor = first["next_cursor"].as_str().expect("a second page");
        let second = read_test_model_resource(
            &tools,
            authz.clone(),
            owner,
            &format!("{base}&limit=1&cursor={cursor}"),
        )
        .await?;
        assert_eq!(distance(&second, middle), Some(1.into()), "{second}");
        assert_eq!(distance(&second, root), Some(2.into()), "{second}");

        // Descendants (the storage walk the engine-level verb runs) carry
        // the same real distances: root 0, middle 1, top 2, across pages.
        let down = format!("proxima://memory/{root}/lineage?direction=descendants&depth=3");
        let whole = read_test_model_resource(&tools, authz.clone(), owner, &down).await?;
        assert_eq!(distance(&whole, root), Some(0.into()), "{whole}");
        assert_eq!(distance(&whole, middle), Some(1.into()), "{whole}");
        assert_eq!(distance(&whole, top), Some(2.into()), "{whole}");
        let first =
            read_test_model_resource(&tools, authz.clone(), owner, &format!("{down}&limit=1"))
                .await?;
        assert_eq!(distance(&first, middle), Some(1.into()), "{first}");
        let cursor = first["next_cursor"]
            .as_str()
            .expect("a second descendants page");
        let second = read_test_model_resource(
            &tools,
            authz,
            owner,
            &format!("{down}&limit=1&cursor={cursor}"),
        )
        .await?;
        assert_eq!(distance(&second, middle), Some(1.into()), "{second}");
        assert_eq!(distance(&second, top), Some(2.into()), "{second}");

        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("lineage distance test failed");
}

/// Other stores whose list never answers.
struct DownInventory;

impl OtherStores for DownInventory {
    fn stores<'a>(
        &'a self,
        _caller: StoreCaller<'a>,
    ) -> BoxFuture<'a, Result<Vec<StoreInfo>, OtherStoresError>> {
        Box::pin(async { Err(OtherStoresError::Unavailable("store list down".into())) })
    }

    fn search<'a>(
        &'a self,
        _caller: StoreCaller<'a>,
        store: u16,
        _args: &'a RemoteSearch,
    ) -> BoxFuture<'a, Result<RemoteSearchResult, OtherStoresError>> {
        Box::pin(async move { panic!("store {store} searched without an inventory") })
    }

    fn read<'a>(
        &'a self,
        _caller: StoreCaller<'a>,
        _id: Uuid,
        _at: ReadAt,
    ) -> BoxFuture<'a, Result<RemoteRead, OtherStoresError>> {
        Box::pin(async { Ok(RemoteRead::NotFound) })
    }
}

/// Acceptance 1a of #480: when the store list fails, a search still returns
/// the local hits, and says the inventory went unanswered instead of
/// passing for "there are no other stores". Naming a store is retryable.
#[tokio::test]
async fn a_failed_store_list_is_reported_on_search_and_refuses_named_stores() {
    let split_db = clone_split_core_db("proxima_other_stores_inventory_down")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(split_db.runtime_url())
            .store_number(0)
            .platform_database_url(split_db.platform_url())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .services(FlavorServices::with(OtherStoresService::new(Arc::new(
                DownInventory,
            ))))
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
        assert_eq!(found["inventory_unanswered"], true, "{found}");
        assert_eq!(found["unanswered"], serde_json::json!([]), "{found}");
        let memories = found["memories"].as_array().expect("memories");
        assert!(
            memories.iter().any(|hit| hit["memory"] == local["handle"]),
            "the local hit is still returned: {found}"
        );

        let named = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_search_memories",
            serde_json::json!({"query":"zebra","mode":"lexical","stores":[1]}),
        )
        .await
        .expect_err("a named store without an inventory");
        assert_eq!(
            named.kind(),
            proxima::CoreMcpErrorKind::StoreUnavailable,
            "{named:?}"
        );

        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("failed inventory search test failed");
}

/// Number of `memory` rows this store holds under `t`, as the platform role.
async fn local_rows_named(platform_url: &str, t: Uuid) -> i64 {
    let pool = sqlx::PgPool::connect(platform_url)
        .await
        .expect("platform pool");
    let mut tx = pool.begin().await.expect("tx");
    sqlx::query("SELECT set_config('app.proxima_scope', 'platform', true)")
        .execute(&mut *tx)
        .await
        .expect("platform scope");
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.memory WHERE t = $1")
        .bind(t)
        .fetch_one(&mut *tx)
        .await
        .expect("count memory");
    tx.rollback().await.expect("rollback");
    pool.close().await;
    rows
}

/// #526: a goal names evidence held only in another store; the store revises
/// it; reading the named evidence at its head returns the newest version the
/// caller may read, and exact reads still return the version as written. An
/// unreadable memory and an unavailable store are refused before any write,
/// and nothing of the remote memory is stored locally.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one end-to-end story: name, revise, read, refuse
async fn goal_evidence_in_another_store_resolves_to_its_series_head() {
    let split_db = clone_split_core_db("proxima_other_stores_goal_evidence")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let (v1, v2) = (remote_id(1, "basis-v1"), remote_id(1, "basis-v2"));
        let (secret, down) = (remote_id(1, "secret"), remote_id(2, "down"));
        let fake = Arc::new(
            FakeStores::default()
                .with(
                    1,
                    v1,
                    Entry::Readable(dated_fact(v1, "basis as written", 1)),
                )
                .with(1, secret, Entry::Denied)
                .with(2, down, Entry::Unavailable),
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
        let goal = |evidence: Vec<String>, key: &str| {
            call_test_model_tool(
                &tools,
                authz.clone(),
                owner,
                "core_goal",
                serde_json::json!({
                    "action": "set", "schema_id": "core/simple-text-v1",
                    "title": key, "text": key, "body": {},
                    "evidence": evidence, "idempotency_key": key,
                }),
            )
        };

        // Unreadable and unavailable evidence refuse the goal before it lands.
        let before = row_counts(&platform_url).await;
        let unreadable = goal(vec![secret.to_string()], "goal-unreadable")
            .await
            .expect_err("unreadable evidence refuses");
        assert_eq!(unreadable.kind(), proxima::CoreMcpErrorKind::NotFound);
        assert!(unreadable.to_string().contains(&secret.to_string()));
        let unavailable = goal(vec![down.to_string()], "goal-unavailable")
            .await
            .expect_err("an unavailable store refuses");
        assert_eq!(
            unavailable.kind(),
            proxima::CoreMcpErrorKind::StoreUnavailable,
            "{unavailable:?}"
        );
        assert_eq!(row_counts(&platform_url).await, before, "zero rows written");

        // The goal names the remote version; the memory is not copied.
        let set = goal(vec![v1.to_string()], "goal-remote").await?;
        assert_eq!(local_rows_named(&platform_url, v1).await, 0);
        let goals =
            read_test_model_resource(&tools, authz.clone(), owner, "proxima://goals").await?;
        let named: Vec<&str> = goals["goals"][0]["evidence"]
            .as_array()
            .expect("evidence")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        assert_eq!(named, [v1.to_string()], "{goals}");

        // A modify that omits evidence carries the remote id unchanged.
        let modified = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_goal",
            serde_json::json!({
                "action": "modify", "goal": set["handle"],
                "schema_id": "core/simple-text-v1", "title": "goal-remote-2",
                "text": "goal-remote-2", "body": {}, "idempotency_key": "goal-remote-2",
            }),
        )
        .await?;
        let goals =
            read_test_model_resource(&tools, authz.clone(), owner, "proxima://goals").await?;
        let carried = goals["goals"]
            .as_array()
            .expect("goals")
            .iter()
            .find(|item| item["goal"] == modified["handle"])
            .expect("the modified goal version");
        assert_eq!(carried["evidence"], serde_json::json!([v1.to_string()]));

        // The store revises the memory: a newer version of the same series.
        let series = fake_series(&fake, v1);
        fake.put(
            1,
            v2,
            Entry::Readable(RemoteMemory {
                series,
                ..dated_fact(v2, "basis revised", 2)
            }),
        );
        let memory = |query: &str| format!("proxima://memory/{v1}{query}");
        let exact = read_test_model_resource(&tools, authz.clone(), owner, &memory("")).await?;
        assert_eq!(exact["handle"], v1.to_string());
        assert_eq!(exact["body"], "basis as written");
        let head = read_test_model_resource(&tools, authz.clone(), owner, &memory("?version=head"))
            .await?;
        assert_eq!(head["handle"], v2.to_string(), "{head}");
        assert_eq!(head["body"], "basis revised");
        assert_eq!(head["series"], exact["series"], "one series, two versions");
        assert_eq!(local_rows_named(&platform_url, v2).await, 0);

        // The head of an unreadable memory or of a store that is down is a
        // refusal, not an answer from some other version.
        for (id, kind) in [
            (secret, proxima::CoreMcpErrorKind::NotFound),
            (down, proxima::CoreMcpErrorKind::StoreUnavailable),
        ] {
            let refused = read_test_model_resource(
                &tools,
                authz.clone(),
                owner,
                &format!("proxima://memory/{id}?version=head"),
            )
            .await
            .expect_err("head read refuses");
            assert_eq!(refused.kind(), kind, "{refused:?}");
        }

        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("goal evidence in another store test failed");
}

fn fake_series(fake: &FakeStores, handle: Uuid) -> Uuid {
    let stores = fake.stores.lock().expect("lock");
    match stores
        .get(&proxima_core::memory_store_number(handle))
        .and_then(|store| store.entries.get(&handle))
    {
        Some(Entry::Readable(memory)) => memory.series,
        _ => panic!("{handle} is not a readable version"),
    }
}

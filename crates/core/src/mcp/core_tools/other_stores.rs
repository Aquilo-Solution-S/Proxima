//! Wire-side numbered store routing, search fan-out, and remote reads.

use std::sync::Arc;

use crate::mcp::{McpToolCtx, McpToolError, MemoryHandleClass};
use crate::storage_ports::other_stores::{
    OtherStoresError, OtherStoresService, ReadOrigin, RemoteMemory, RemoteOriginRead, RemoteSearch,
    RemoteSearchResult, StoreCaller, StoreInventory, StoreRoute, route_stores,
};

pub(crate) fn service(ctx: &McpToolCtx) -> Option<Arc<OtherStoresService>> {
    ctx.service::<OtherStoresService>()
}

pub(crate) fn caller(ctx: &McpToolCtx) -> StoreCaller<'_> {
    StoreCaller {
        owner: ctx.owner,
        authz: &ctx.authz,
    }
}

fn not_found(id: uuid::Uuid) -> McpToolError {
    McpToolError::NotFound(format!("memory {id} not found"))
}

pub(crate) async fn read_origin(
    ctx: &McpToolCtx,
    id: uuid::Uuid,
) -> Result<(ReadOrigin, RemoteMemory), McpToolError> {
    let service = service(ctx).ok_or_else(|| not_found(id))?;
    match service.read_origin(caller(ctx), id).await {
        Ok(Some(read)) => Ok(read),
        Ok(None) | Err(_) => Err(not_found(id)),
    }
}

/// The persistence carrier of one readable reference. Every wire spelling is a UUID.
#[derive(Debug)]
pub(crate) enum ResolvedMemory {
    Local(crate::MemoryId, MemoryHandleClass),
    Remote(ReadOrigin),
}

impl ResolvedMemory {
    pub(crate) fn classified(&self) -> Result<(crate::MemoryId, MemoryHandleClass), McpToolError> {
        match self {
            Self::Local(id, kind) => Ok((*id, *kind)),
            Self::Remote(origin) => Ok((
                crate::MemoryId::new(origin.handle()),
                super::get_memory::memory_class(origin.kind())?,
            )),
        }
    }
}

/// Read each unique UUID through its issuing store as the caller.
pub(crate) async fn resolve_references(
    ctx: &McpToolCtx,
    handles: &[String],
) -> Result<Vec<ResolvedMemory>, McpToolError> {
    let local_number = ctx.require_engine()?.store_number();
    let mut seen = std::collections::HashSet::with_capacity(handles.len());
    let mut ids = Vec::with_capacity(handles.len());
    for raw in handles {
        let id = crate::mcp::parse_wire_uuid(raw)?;
        if seen.insert(id) {
            ids.push(id);
        }
    }
    let remote_ids = ids
        .iter()
        .copied()
        .filter(|id| crate::memory_store_number(*id) != local_number)
        .collect::<Vec<_>>();
    let mut remote = resolve_remote_origins(ctx, &remote_ids).await?.into_iter();
    let mut resolved = Vec::with_capacity(ids.len());
    for id in ids {
        resolved.push(if crate::memory_store_number(id) == local_number {
            let (id, kind) = ctx
                .resolve_memory_id_with_kind(crate::MemoryId::new(id))
                .await?;
            ResolvedMemory::Local(id, kind)
        } else {
            ResolvedMemory::Remote(remote.next().expect("one bounded result per remote UUID"))
        });
    }
    Ok(resolved)
}

async fn resolve_remote_origins(
    ctx: &McpToolCtx,
    ids: &[uuid::Uuid],
) -> Result<Vec<ReadOrigin>, McpToolError> {
    let Some(first) = ids.first() else {
        return Ok(Vec::new());
    };
    let service = service(ctx).ok_or_else(|| not_found(*first))?;
    let reads = service.read_origins_bounded(caller(ctx), ids).await;
    ids.iter()
        .zip(reads)
        .map(|(id, read)| match read {
            Ok(RemoteOriginRead::Found(origin, _)) => Ok(origin),
            Err(OtherStoresError::DeadlineExceeded) => Err(McpToolError::Unavailable(
                "other-store origin deadline exceeded".into(),
            )),
            Ok(RemoteOriginRead::NotFound | RemoteOriginRead::Erased) | Err(_) => {
                Err(not_found(*id))
            }
        })
        .collect()
}

pub(crate) struct SearchStores {
    pub(crate) route: StoreRoute,
    pub(crate) service: Option<Arc<OtherStoresService>>,
}

pub(crate) async fn route_search(
    ctx: &McpToolCtx,
    requested: Option<&[u16]>,
    schema_id: Option<&str>,
) -> Result<SearchStores, McpToolError> {
    let local_number = ctx.require_engine()?.store_number();
    let service = service(ctx);
    let readable = match &service {
        Some(service) => service
            .stores_bounded(caller(ctx), local_number)
            .await
            .map_err(|error| McpToolError::Other(error.to_string()))?,
        None => None,
    }
    .unwrap_or_else(|| StoreInventory::empty(local_number));
    let local_declares = |schema_id: &str| {
        ctx.engine().is_some_and(|engine| {
            engine
                .registry()
                .schemas()
                .iter()
                .any(|info| info.schema_id.as_str() == schema_id)
        })
    };
    let route = route_stores(requested, schema_id, local_declares, &readable)
        .map_err(McpToolError::InvalidInput)?;
    Ok(SearchStores { route, service })
}

pub(crate) async fn search_remotes(
    ctx: &McpToolCtx,
    service: &OtherStoresService,
    stores: &[u16],
    args: &RemoteSearch,
) -> (Vec<(u16, RemoteSearchResult)>, Vec<u16>) {
    let caller = caller(ctx);
    let calls = stores.iter().map(|store| async move {
        let result = tokio::time::timeout(
            service.timeout(),
            service.port().search(caller, *store, args),
        )
        .await
        .map(|result| result.and_then(|result| checked_remote_hits(*store, result)));
        (*store, result)
    });
    let mut answered = Vec::new();
    let mut unanswered = Vec::new();
    for (store, result) in futures::future::join_all(calls).await {
        match result {
            Ok(Ok(mut result)) => {
                result
                    .hits
                    .truncate(usize::try_from(args.limit).unwrap_or(usize::MAX));
                answered.push((store, result));
            }
            Ok(Err(_)) | Err(_) => unanswered.push(store),
        }
    }
    (answered, unanswered)
}

/// A port response belongs to the requested numbered store, or the store did not answer safely.
fn checked_remote_hits(
    store: u16,
    result: RemoteSearchResult,
) -> Result<RemoteSearchResult, OtherStoresError> {
    if result
        .hits
        .iter()
        .any(|hit| crate::memory_store_number(hit.handle) != store)
    {
        return Err(OtherStoresError::Unavailable(
            "search returned a memory from another store".into(),
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UserId;
    use crate::mcp::core_tools::memory::derive::MAX_SOURCE_HANDLES;
    use crate::mcp::core_tools::memory_spaces::test_ctx::ctx_for;
    use crate::storage_ports::other_stores::{OtherStores, RemoteHit, RemoteRead, StoreInfo};
    use futures::future::BoxFuture;

    #[test]
    fn a_search_response_must_carry_the_requested_store_number() {
        let hit = RemoteHit {
            handle: crate::new_memory_uuid(2),
            kind: crate::EntityKind::Fact,
            schema_id: "remote/note".into(),
            snippet: "remote result".into(),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            tags: Vec::new(),
        };
        let reply = RemoteSearchResult {
            hits: vec![hit],
            degraded_to_lexical: true,
        };
        assert_eq!(
            checked_remote_hits(2, reply.clone()).expect("same store"),
            reply
        );
        assert!(matches!(
            checked_remote_hits(3, reply),
            Err(OtherStoresError::Unavailable(_))
        ));
    }
    enum InventoryReply {
        Stores(Vec<u16>),
        Unavailable,
        Pending,
    }

    struct InventoryPort {
        reply: InventoryReply,
        listings: std::sync::atomic::AtomicUsize,
        searches: std::sync::Mutex<Vec<u16>>,
    }

    impl OtherStores for InventoryPort {
        fn stores<'a>(
            &'a self,
            _caller: StoreCaller<'a>,
        ) -> BoxFuture<'a, Result<Vec<StoreInfo>, OtherStoresError>> {
            Box::pin(async move {
                self.listings
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                match &self.reply {
                    InventoryReply::Stores(numbers) => Ok(numbers
                        .iter()
                        .enumerate()
                        .map(|(index, number)| StoreInfo {
                            number: *number,
                            name: format!("Store {index}"),
                            description: String::new(),
                            schema_ids: vec![format!("store/{number}")],
                        })
                        .collect()),
                    InventoryReply::Unavailable => {
                        Err(OtherStoresError::Unavailable("offline".into()))
                    }
                    InventoryReply::Pending => futures::future::pending().await,
                }
            })
        }

        fn search<'a>(
            &'a self,
            _caller: StoreCaller<'a>,
            store: u16,
            _args: &'a RemoteSearch,
        ) -> BoxFuture<'a, Result<RemoteSearchResult, OtherStoresError>> {
            Box::pin(async move {
                self.searches.lock().expect("search calls").push(store);
                Ok(RemoteSearchResult {
                    hits: Vec::new(),
                    degraded_to_lexical: false,
                })
            })
        }

        fn read<'a>(
            &'a self,
            _caller: StoreCaller<'a>,
            _id: uuid::Uuid,
        ) -> BoxFuture<'a, Result<RemoteRead, OtherStoresError>> {
            Box::pin(async { Ok(RemoteRead::NotFound) })
        }
    }

    fn inventory_ctx(reply: InventoryReply) -> (McpToolCtx, Arc<InventoryPort>) {
        let port = Arc::new(InventoryPort {
            reply,
            listings: std::sync::atomic::AtomicUsize::new(0),
            searches: std::sync::Mutex::new(Vec::new()),
        });
        let mut ctx = ctx_for(UserId::new(uuid::Uuid::now_v7()), Vec::new());
        ctx.engine = Some(Arc::new(crate::Engine::new(ctx.registry.as_ref().clone())));
        ctx.services = crate::FlavorServices::with(OtherStoresService::new(port.clone()));
        (ctx, port)
    }

    #[tokio::test]
    async fn invalid_inventories_refuse_search_and_metadata_without_fanout() {
        use crate::mcp::McpTool;
        use crate::mcp::core_tools::search_memories::{
            SearchMemoriesArgs, SearchMemoriesTool, caller_input_schema,
        };

        for (numbers, expected) in [
            (vec![1, 0], OtherStoresError::LocalStoreInInventory(0)),
            (
                vec![1, 2, 1],
                OtherStoresError::DuplicateStoreInInventory(1),
            ),
        ] {
            let (ctx, port) = inventory_ctx(InventoryReply::Stores(numbers));
            assert_eq!(
                service(&ctx)
                    .expect("wired port")
                    .stores_bounded(caller(&ctx), 0)
                    .await,
                Err(expected.clone()),
            );
            assert_eq!(
                caller_input_schema(&ctx, serde_json::json!({"type": "object"})).await,
                Err(expected.clone()),
            );
            for (stores, schema_id) in [
                (None, None),
                (Some(vec![0]), None),
                (Some(vec![1]), None),
                (None, Some("store/1")),
            ] {
                let args: SearchMemoriesArgs = serde_json::from_value(serde_json::json!({
                    "query": "needle", "mode": "lexical", "stores": stores, "schema_id": schema_id,
                }))
                .expect("valid search arguments");
                let error = SearchMemoriesTool::call(ctx.clone(), args)
                    .await
                    .expect_err("an invalid inventory must refuse before searching");
                assert!(
                    matches!(error, McpToolError::Other(ref message) if message == &expected.to_string()),
                    "{error:?}"
                );
            }
            assert_eq!(port.listings.load(std::sync::atomic::Ordering::Relaxed), 6);
            assert!(port.searches.lock().expect("search calls").is_empty());
        }
    }

    #[tokio::test]
    async fn valid_inventories_route_each_selected_remote_once_in_request_order() {
        for (requested, schema_id, expected) in [
            (
                None,
                None,
                StoreRoute {
                    local: true,
                    remotes: vec![1, 2],
                },
            ),
            (
                None,
                Some("store/2"),
                StoreRoute {
                    local: false,
                    remotes: vec![2],
                },
            ),
            (
                Some(vec![2, 1, 2, 0]),
                None,
                StoreRoute {
                    local: true,
                    remotes: vec![2, 1],
                },
            ),
            (
                Some(vec![0]),
                None,
                StoreRoute {
                    local: true,
                    remotes: Vec::new(),
                },
            ),
        ] {
            let (ctx, port) = inventory_ctx(InventoryReply::Stores(vec![1, 2]));
            let routed = route_search(&ctx, requested.as_deref(), schema_id)
                .await
                .expect("valid route");
            assert_eq!(routed.route, expected);
            let args = RemoteSearch {
                query: "needle".into(),
                limit: 8,
                kind: None,
                schema_id: schema_id.map(str::to_owned),
                tags: Vec::new(),
                tag_match: crate::verbs::query::TagMatch::Any,
                mode: crate::verbs::query::SearchMode::Lexical,
                since: None,
                until: None,
                order: crate::verbs::query::SearchOrder::Relevance,
                supersession: crate::verbs::query::SupersessionStatus::HeadsOnly,
                min_score: None,
                semantic_weight: None,
            };
            let (answered, unanswered) = search_remotes(
                &ctx,
                routed.service.as_deref().expect("wired port"),
                &routed.route.remotes,
                &args,
            )
            .await;
            assert_eq!(
                answered
                    .iter()
                    .map(|(number, _)| *number)
                    .collect::<Vec<_>>(),
                expected.remotes
            );
            assert!(unanswered.is_empty());
            assert_eq!(
                *port.searches.lock().expect("search calls"),
                expected.remotes
            );
            assert_eq!(port.listings.load(std::sync::atomic::Ordering::Relaxed), 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn unavailable_inventory_preserves_local_routing_and_static_metadata() {
        for reply in [InventoryReply::Unavailable, InventoryReply::Pending] {
            let (ctx, port) = inventory_ctx(reply);
            let schema = serde_json::json!({"type": "object", "properties": {}});
            assert_eq!(
                super::super::search_memories::caller_input_schema(&ctx, schema.clone())
                    .await
                    .expect("unavailable inventory"),
                schema,
            );
            let routed = route_search(&ctx, None, None).await.expect("local route");
            assert_eq!(
                routed.route,
                StoreRoute {
                    local: true,
                    remotes: Vec::new()
                }
            );
            let error = route_search(&ctx, Some(&[1]), None)
                .await
                .err()
                .expect("unknown remote");
            assert!(matches!(error, McpToolError::InvalidInput(_)));
            assert_eq!(port.listings.load(std::sync::atomic::Ordering::Relaxed), 3);
            assert!(port.searches.lock().expect("search calls").is_empty());
        }
    }

    struct SlowOriginStore;

    impl OtherStores for SlowOriginStore {
        fn stores<'a>(
            &'a self,
            _caller: StoreCaller<'a>,
        ) -> BoxFuture<'a, Result<Vec<StoreInfo>, OtherStoresError>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn search<'a>(
            &'a self,
            _caller: StoreCaller<'a>,
            _store: u16,
            _args: &'a RemoteSearch,
        ) -> BoxFuture<'a, Result<RemoteSearchResult, OtherStoresError>> {
            Box::pin(async {
                Ok(RemoteSearchResult {
                    hits: Vec::new(),
                    degraded_to_lexical: false,
                })
            })
        }

        fn read<'a>(
            &'a self,
            _caller: StoreCaller<'a>,
            id: uuid::Uuid,
        ) -> BoxFuture<'a, Result<RemoteRead, OtherStoresError>> {
            Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_millis(1_900)).await;
                Ok(RemoteRead::Found(RemoteMemory {
                    handle: id,
                    kind: crate::EntityKind::Fact,
                    schema_id: "test/fact-v1".into(),
                    title: None,
                    body: String::new(),
                    created_at: time::OffsetDateTime::UNIX_EPOCH,
                    tags: Vec::new(),
                }))
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn derive_origin_admission_refuses_255_slow_origins_before_writing() {
        let mut ctx = ctx_for(UserId::new(uuid::Uuid::now_v7()), Vec::new());
        ctx.services =
            crate::FlavorServices::with(OtherStoresService::new(Arc::new(SlowOriginStore)));
        ctx.engine = Some(Arc::new(crate::Engine::new(ctx.registry.as_ref().clone())));
        let origins = (0..MAX_SOURCE_HANDLES - 1)
            .map(|_| crate::new_memory_uuid(1).to_string())
            .collect::<Vec<_>>();
        let started = tokio::time::Instant::now();
        let deadline = crate::storage_ports::other_stores::DEFAULT_REMOTE_BATCH_DEADLINE;
        let error = tokio::time::timeout(
            deadline + std::time::Duration::from_millis(1),
            resolve_references(&ctx, &origins),
        )
        .await
        .expect("derive admission must finish within the call deadline")
        .expect_err("deadline must refuse derivation");
        assert!(started.elapsed() <= deadline);
        assert!(
            matches!(error, McpToolError::Unavailable(ref message) if message.contains("deadline")),
            "{error:?}"
        );
    }
}

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

/// THE mapping of a failed remote read onto the wire (#480).
///
/// An unknown store is `NotFound` (no existence leak, and the same answer as
/// "no other store wired"). Every other failure (unavailable, past the
/// deadline, an answer for a different memory, or anything else) says
/// nothing about whether the memory exists, so it is a retryable
/// [`McpToolError::StoreUnavailable`] naming the store, never "not found".
/// An answer of "not found" or "erased" is not an error and never gets here.
pub(crate) fn remote_read_error(id: uuid::Uuid, error: &OtherStoresError) -> McpToolError {
    match error {
        OtherStoresError::UnknownStore(_) => not_found(id),
        OtherStoresError::Unavailable(_)
        | OtherStoresError::DeadlineExceeded
        | OtherStoresError::MismatchedMemory { .. }
        | OtherStoresError::LocalStoreInInventory(_)
        | OtherStoresError::DuplicateStoreInInventory(_)
        | OtherStoresError::UnsupportedSearch(_) => McpToolError::StoreUnavailable {
            store: crate::memory_store_number(id),
        },
    }
}

pub(crate) async fn read_origin(
    ctx: &McpToolCtx,
    id: uuid::Uuid,
) -> Result<(ReadOrigin, RemoteMemory), McpToolError> {
    let service = service(ctx).ok_or_else(|| not_found(id))?;
    match service.read_origin(caller(ctx), id).await {
        Ok(Some(read)) => Ok(read),
        Ok(None) => Err(not_found(id)),
        Err(error) => Err(remote_read_error(id, &error)),
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
    // The first failing id in request order decides the error: deterministic.
    ids.iter()
        .zip(reads)
        .map(|(id, read)| match read {
            Ok(RemoteOriginRead::Found(origin, _)) => Ok(origin),
            Ok(RemoteOriginRead::NotFound | RemoteOriginRead::Erased) => Err(not_found(*id)),
            Err(error) => Err(remote_read_error(*id, &error)),
        })
        .collect()
}

pub(crate) struct SearchStores {
    pub(crate) route: StoreRoute,
    pub(crate) service: Option<Arc<OtherStoresService>>,
    /// Other stores are wired but their list did not answer (failed or
    /// timed out), so the search ran locally only.
    pub(crate) inventory_unanswered: bool,
}

/// Route a search over the stores the caller may read.
///
/// When other stores are wired but their list does not answer, a search
/// that names a remote store fails retryable (`StoreUnavailable` for the
/// first one named), one that names only this store runs as asked, and
/// one that names none runs locally and reports `inventory_unanswered`.
pub(crate) async fn route_search(
    ctx: &McpToolCtx,
    requested: Option<&[u16]>,
    schema_id: Option<&str>,
) -> Result<SearchStores, McpToolError> {
    let local_number = ctx.require_engine()?.store_number();
    let service = service(ctx);
    let answered = match &service {
        Some(service) => Some(
            service
                .stores_bounded(caller(ctx), local_number)
                .await
                .map_err(|error| McpToolError::Other(error.to_string()))?,
        ),
        None => None,
    };
    let mut inventory_unanswered = false;
    if matches!(answered, Some(None)) {
        if let Some(store) = requested
            .into_iter()
            .flatten()
            .copied()
            .find(|number| *number != local_number)
        {
            return Err(McpToolError::StoreUnavailable { store });
        }
        inventory_unanswered = requested.is_none();
    }
    let readable = answered
        .flatten()
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
    Ok(SearchStores {
        route,
        service,
        inventory_unanswered,
    })
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
            writer: crate::WriterAttribution {
                by: uuid::Uuid::from_u128(3),
                via: None,
            },
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

    /// Acceptance 1 of #480: a store list that failed or timed out is not
    /// "there are no other stores". A search that names no store still runs
    /// locally but says the inventory went unanswered; one that names a
    /// remote store is a retryable `StoreUnavailable` (not "unknown store");
    /// one that names only the local store needed no inventory. The tool
    /// listing keeps its static schema.
    #[tokio::test(start_paused = true)]
    async fn unavailable_inventory_is_reported_and_refuses_named_stores() {
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
            assert!(routed.inventory_unanswered);
            let error = route_search(&ctx, Some(&[0, 2, 1]), None)
                .await
                .err()
                .expect("a named remote store");
            assert!(
                matches!(error, McpToolError::StoreUnavailable { store: 2 }),
                "{error:?}"
            );
            let local_only = route_search(&ctx, Some(&[0]), None)
                .await
                .expect("local only");
            assert_eq!(
                local_only.route,
                StoreRoute {
                    local: true,
                    remotes: Vec::new()
                }
            );
            assert!(!local_only.inventory_unanswered);
            assert_eq!(port.listings.load(std::sync::atomic::Ordering::Relaxed), 4);
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
                    writer: crate::WriterAttribution {
                        by: uuid::Uuid::from_u128(3),
                        via: None,
                    },
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
            matches!(error, McpToolError::StoreUnavailable { store: 1 }),
            "{error:?}"
        );
    }

    #[derive(Clone, Copy)]
    enum ReadReply {
        Pending,
        Unavailable,
        Mismatched,
        NotFound,
        Erased,
    }

    struct ReadReplyStore(ReadReply);

    impl OtherStores for ReadReplyStore {
        fn stores<'a>(
            &'a self,
            _caller: StoreCaller<'a>,
        ) -> BoxFuture<'a, Result<Vec<StoreInfo>, OtherStoresError>> {
            Box::pin(async { Ok(vec![]) })
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
            _id: uuid::Uuid,
        ) -> BoxFuture<'a, Result<RemoteRead, OtherStoresError>> {
            let reply = self.0;
            Box::pin(async move {
                match reply {
                    ReadReply::Pending => futures::future::pending().await,
                    ReadReply::Unavailable => {
                        Err(OtherStoresError::Unavailable("remote internals".into()))
                    }
                    ReadReply::Mismatched => Ok(RemoteRead::Found(RemoteMemory {
                        handle: crate::new_memory_uuid(1),
                        writer: crate::WriterAttribution {
                            by: uuid::Uuid::from_u128(3),
                            via: None,
                        },
                        kind: crate::EntityKind::Fact,
                        schema_id: "test/fact-v1".into(),
                        title: None,
                        body: String::new(),
                        created_at: time::OffsetDateTime::UNIX_EPOCH,
                        tags: Vec::new(),
                    })),
                    ReadReply::NotFound => Ok(RemoteRead::NotFound),
                    ReadReply::Erased => Ok(RemoteRead::Erased),
                }
            })
        }
    }

    fn read_reply_ctx(reply: ReadReply) -> McpToolCtx {
        let mut ctx = ctx_for(UserId::new(uuid::Uuid::now_v7()), Vec::new());
        ctx.services =
            crate::FlavorServices::with(OtherStoresService::new(Arc::new(ReadReplyStore(reply))));
        ctx.engine = Some(Arc::new(crate::Engine::new(ctx.registry.as_ref().clone())));
        ctx
    }

    /// Acceptance 2 of #480: a remote source that times out, fails, or answers
    /// for another memory is a retryable `StoreUnavailable` naming its store,
    /// on both the batch path (derive sources, interpret subjects) and the
    /// single read (`get_memory`). A store that answers "no such memory" (or
    /// erased) stays `NotFound`, so nothing about existence leaks.
    #[tokio::test(start_paused = true)]
    async fn an_unanswered_remote_read_is_store_unavailable_and_an_answer_is_not_found() {
        for (reply, unavailable) in [
            (ReadReply::Pending, true),
            (ReadReply::Unavailable, true),
            (ReadReply::Mismatched, true),
            (ReadReply::NotFound, false),
            (ReadReply::Erased, false),
        ] {
            let ctx = read_reply_ctx(reply);
            let id = crate::new_memory_uuid(1);
            let batch = resolve_references(&ctx, &[id.to_string()])
                .await
                .expect_err("no remote origin is readable");
            let single = read_origin(&ctx, id)
                .await
                .expect_err("no remote memory is readable");
            for error in [batch, single] {
                if unavailable {
                    assert!(
                        matches!(error, McpToolError::StoreUnavailable { store: 1 }),
                        "{error:?}"
                    );
                    assert_eq!(error.kind(), crate::mcp::McpToolErrorKind::StoreUnavailable);
                    assert!(!error.client_message().contains("internals"));
                } else {
                    assert!(matches!(error, McpToolError::NotFound(_)), "{error:?}");
                }
            }
        }
    }
}

//! Wire-side use of the other-stores port: qualified source handles,
//! search fan-out, and remote reads. Every function here is a no-op path
//! when no port is wired, so a deployment without one behaves as before.

use std::sync::Arc;

use crate::mcp::{McpToolCtx, McpToolError, MemoryHandleClass};
use crate::storage_ports::other_stores::{
    LOCAL_STORE, OtherStoresService, QualifiedHandle, ReadOrigin, RemoteHit, RemoteMemory,
    RemoteSearch, StoreCaller, StoreInfo, StoreName, StoreRoute, route_stores,
};

/// The wired port, if any.
pub(crate) fn service(ctx: &McpToolCtx) -> Option<Arc<OtherStoresService>> {
    ctx.service::<OtherStoresService>()
}

pub(crate) fn caller(ctx: &McpToolCtx) -> StoreCaller<'_> {
    StoreCaller {
        owner: ctx.owner,
        authz: &ctx.authz,
    }
}

/// `raw` as a qualified handle, when a port is wired. Without one nothing
/// is qualified, so the handle keeps today's local parse and its error.
pub(crate) fn qualified(ctx: &McpToolCtx, raw: &str) -> Option<QualifiedHandle> {
    service(ctx).and_then(|_| QualifiedHandle::parse(raw))
}

fn not_found(raw: &str) -> McpToolError {
    McpToolError::NotFound(format!("memory {raw} not found"))
}

/// Read one remote memory as the caller, with the [`ReadOrigin`] a write
/// names it by. Unknown, unreadable, erased, and unreachable all refuse with
/// the not-found a local handle gets.
pub(crate) async fn read_origin(
    ctx: &McpToolCtx,
    origin: &QualifiedHandle,
) -> Result<(ReadOrigin, RemoteMemory), McpToolError> {
    let raw = origin.to_string();
    let service = service(ctx).ok_or_else(|| not_found(&raw))?;
    match service.read_origin(caller(ctx), origin).await {
        Ok(Some(read)) => Ok(read),
        Ok(None) | Err(_) => Err(not_found(&raw)),
    }
}

/// Split derive sources into local handles and other-store origins, read
/// every origin as the caller, and check it sits on the local sources'
/// layer. Runs before any write, so a refusal writes nothing.
///
/// # Errors
///
/// `InvalidInput` when only other-store sources are named,
/// `LayeringViolation` when a remote layer differs, and the not-found of
/// [`read_origin`] for an origin the caller cannot read.
pub(crate) async fn admit_external_origins(
    ctx: &McpToolCtx,
    local_layer: Option<MemoryHandleClass>,
    remote: &[QualifiedHandle],
) -> Result<Vec<ReadOrigin>, McpToolError> {
    if remote.is_empty() {
        return Ok(Vec::new());
    }
    let Some(layer) = local_layer else {
        return Err(McpToolError::InvalidInput(
            "source_handles must include at least one memory of this store; memories of other stores only join local ones".into(),
        ));
    };
    let mut admitted = Vec::with_capacity(remote.len());
    for origin in remote {
        let (read, memory) = read_origin(ctx, origin).await?;
        let class = super::get_memory::memory_class(memory.kind)?;
        if class != layer {
            return Err(McpToolError::InvalidInput(
                "source_handles must have one memory layer per operator invocation".into(),
            ));
        }
        admitted.push(read);
    }
    Ok(admitted)
}

/// Partition `handles` into local handles (in order) and deduplicated
/// other-store origins.
pub(crate) fn partition_sources(
    ctx: &McpToolCtx,
    handles: &[String],
) -> (Vec<String>, Vec<QualifiedHandle>) {
    let mut local = Vec::new();
    let mut remote: Vec<QualifiedHandle> = Vec::new();
    for handle in handles {
        match qualified(ctx, handle) {
            Some(origin) => {
                if !remote.contains(&origin) {
                    remote.push(origin);
                }
            }
            None => local.push(handle.clone()),
        }
    }
    (local, remote)
}

/// The stores of one search.
pub(crate) struct SearchStores {
    pub(crate) route: StoreRoute,
    pub(crate) service: Option<Arc<OtherStoresService>>,
}

/// Route one search. Without a port only `local` may be named.
///
/// # Errors
///
/// `InvalidInput` for an unknown or unreadable store, or an empty list.
pub(crate) async fn route_search(
    ctx: &McpToolCtx,
    requested: Option<&[String]>,
    schema_id: Option<&str>,
) -> Result<SearchStores, McpToolError> {
    let Some(service) = service(ctx) else {
        if let Some(requested) = requested {
            if requested.is_empty() {
                return Err(McpToolError::InvalidInput(
                    "stores must name at least one store when given".into(),
                ));
            }
            if let Some(other) = requested.iter().find(|name| *name != LOCAL_STORE) {
                return Err(McpToolError::InvalidInput(format!(
                    "unknown store '{other}': this deployment searches only 'local'"
                )));
            }
        }
        return Ok(SearchStores {
            route: StoreRoute {
                local: true,
                remotes: Vec::new(),
            },
            service: None,
        });
    };
    let readable: Vec<StoreInfo> =
        tokio::time::timeout(service.timeout(), service.port().stores(caller(ctx)))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
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
        service: Some(service),
    })
}

/// Every routed store's hits, plus the stores that did not answer in time
/// (or failed). Stores run in parallel, each under the service's bound.
pub(crate) async fn search_remotes(
    ctx: &McpToolCtx,
    service: &OtherStoresService,
    stores: &[StoreName],
    args: &RemoteSearch,
) -> (Vec<(StoreName, Vec<RemoteHit>)>, Vec<String>) {
    let caller = caller(ctx);
    let calls = stores.iter().map(|store| async move {
        let result = tokio::time::timeout(
            service.timeout(),
            service.port().search(caller, store, args),
        )
        .await;
        (store, result)
    });
    let mut answered = Vec::new();
    let mut unanswered = Vec::new();
    for (store, result) in futures::future::join_all(calls).await {
        match result {
            Ok(Ok(mut hits)) => {
                hits.truncate(usize::try_from(args.limit).unwrap_or(usize::MAX));
                answered.push((store.clone(), hits));
            }
            Ok(Err(_)) | Err(_) => unanswered.push(store.to_string()),
        }
    }
    (answered, unanswered)
}

//! Wire-side use of the other-stores port: qualified source handles,
//! search fan-out, and remote reads. Every function here is a no-op path
//! when no port is wired, so a deployment without one behaves as before.

use std::sync::Arc;

use crate::mcp::{McpToolCtx, McpToolError, MemoryHandleClass};
use crate::storage_ports::other_stores::{
    LOCAL_STORE, OtherStoresService, QualifiedHandle, ReadOrigin, RemoteHit, RemoteMemory,
    RemoteOriginRead, RemoteSearch, StoreCaller, StoreInfo, StoreName, StoreRoute, route_stores,
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
    let service = service(ctx).ok_or_else(|| not_found(&remote[0].to_string()))?;
    let reads = service.read_origins_bounded(caller(ctx), remote).await;
    let mut admitted = Vec::with_capacity(remote.len());
    for (origin, result) in remote.iter().zip(reads) {
        match result {
            Err(crate::storage_ports::other_stores::OtherStoresError::DeadlineExceeded) => {
                return Err(McpToolError::Unavailable(
                    "other-store origin deadline exceeded".into(),
                ));
            }
            Err(_) => return Err(not_found(&origin.to_string())),
            Ok(RemoteOriginRead::Found(read, memory)) => {
                let class = super::get_memory::memory_class(memory.kind)?;
                if class != layer {
                    return Err(McpToolError::InvalidInput(
                        "source_handles must have one memory layer per operator invocation".into(),
                    ));
                }
                admitted.push(read);
            }
            Ok(RemoteOriginRead::NotFound | RemoteOriginRead::Erased) => {
                return Err(not_found(&origin.to_string()));
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UserId;
    use crate::mcp::core_tools::memory::derive::MAX_SOURCE_HANDLES;
    use crate::mcp::core_tools::memory_spaces::test_ctx::ctx_for;
    use crate::storage_ports::other_stores::{OtherStores, OtherStoresError, RemoteRead};
    use futures::future::BoxFuture;

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
            _store: &'a StoreName,
            _args: &'a RemoteSearch,
        ) -> BoxFuture<'a, Result<Vec<RemoteHit>, OtherStoresError>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn read<'a>(
            &'a self,
            _caller: StoreCaller<'a>,
            _store: &'a StoreName,
            _handle: &'a str,
        ) -> BoxFuture<'a, Result<RemoteRead, OtherStoresError>> {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_millis(1_900)).await;
                Ok(RemoteRead::Found(RemoteMemory {
                    handle: "remote:slow".into(),
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
        let origins = (0..MAX_SOURCE_HANDLES - 1)
            .map(|index| QualifiedHandle::parse(&format!("remote:{index}")))
            .map(|handle| handle.expect("qualified test handle"))
            .collect::<Vec<_>>();
        let started = tokio::time::Instant::now();
        let deadline = crate::storage_ports::other_stores::DEFAULT_REMOTE_BATCH_DEADLINE;
        let error = tokio::time::timeout(
            deadline + std::time::Duration::from_millis(1),
            admit_external_origins(&ctx, Some(MemoryHandleClass::Fact), &origins),
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

//! Read access to other stores, addressed by the number carried in each memory UUID.
//!
//! A host wires one [`OtherStores`] port through `RuntimeBuilder::other_stores`;
//! tools find it as [`OtherStoresService`] in the request's service bag.
//! Display names describe stores; routing and durable origins use their numbers.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::{self, StreamExt};
use uuid::Uuid;

use crate::verbs::query::{EntityKind, SearchMode, SearchOrder, SupersessionStatus, TagMatch};
use crate::{AuthzContext, OwnerId, WriterAttribution};

/// How long a search waits for one other store before reporting it as
/// unanswered.
pub const DEFAULT_REMOTE_TIMEOUT: Duration = Duration::from_secs(2);

/// Maximum time one tool call may spend reading other-store origins.
pub const DEFAULT_REMOTE_BATCH_DEADLINE: Duration = Duration::from_secs(10);

/// Maximum number of origin reads in flight for one other store per call.
pub const MAX_REMOTE_READS_PER_STORE: usize = 8;

/// Maximum distinct foreign origins read for one lineage page; remaining
/// origins render unavailable with an explicit reason.
pub const MAX_LINEAGE_REMOTE_ORIGINS: usize = 64;

/// Reciprocal-rank-fusion constant used to merge store result lists.
pub const STORE_RRF_K: f32 = 60.0;

/// Who is asking: the store decides what this caller may see.
#[derive(Debug, Clone, Copy)]
pub struct StoreCaller<'a> {
    pub owner: OwnerId,
    pub authz: &'a AuthzContext,
}

/// One store the caller may read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreInfo {
    pub number: u16,
    pub name: String,
    pub description: String,
    /// Schema ids the store holds; a search filtered to a `schema_id`
    /// exactly one store declares goes to that store only.
    pub schema_ids: Vec<String>,
}

/// A checked inventory of remote stores for one local store.
///
/// Remote numbers are unique and exclude this store's number. Routing and
/// schema rendering share this inventory without choosing a local number again.
///
/// ```compile_fail
/// use proxima_core::storage_ports::other_stores::StoreInventory;
/// StoreInventory { local_number: 0, remotes: Vec::new() };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreInventory {
    local_number: u16,
    remotes: Vec<StoreInfo>,
}

impl StoreInventory {
    /// Check an inventory where it enters from an other-stores port.
    ///
    /// # Errors
    ///
    /// Refuses a local number or a repeated remote number.
    pub fn new(local_number: u16, remotes: Vec<StoreInfo>) -> Result<Self, OtherStoresError> {
        let mut numbers = std::collections::HashSet::with_capacity(remotes.len());
        for info in &remotes {
            if info.number == local_number {
                return Err(OtherStoresError::LocalStoreInInventory(info.number));
            }
            if !numbers.insert(info.number) {
                return Err(OtherStoresError::DuplicateStoreInInventory(info.number));
            }
        }
        Ok(Self {
            local_number,
            remotes,
        })
    }

    pub(crate) const fn empty(local_number: u16) -> Self {
        Self {
            local_number,
            remotes: Vec::new(),
        }
    }

    #[must_use]
    pub const fn local_number(&self) -> u16 {
        self.local_number
    }

    #[must_use]
    pub fn remotes(&self) -> &[StoreInfo] {
        &self.remotes
    }
}

/// The search a remote store is asked to run.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteSearch {
    pub query: String,
    pub limit: u32,
    pub kind: Option<EntityKind>,
    pub schema_id: Option<String>,
    pub tags: Vec<String>,
    pub tag_match: TagMatch,
    /// Requested mode; each store resolves its own embedding availability.
    pub mode: SearchMode,
    pub since: Option<time::OffsetDateTime>,
    pub until: Option<time::OffsetDateTime>,
    pub order: SearchOrder,
    pub supersession: SupersessionStatus,
    pub min_score: Option<f32>,
    pub semantic_weight: Option<f32>,
}

/// One store's answer, including lexical fallback even when it found no hits.
///
/// The fallback fact is required for an empty answer too.
///
/// ```compile_fail,E0063
/// use proxima_core::storage_ports::other_stores::RemoteSearchResult;
/// let _ = RemoteSearchResult { hits: Vec::new() };
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteSearchResult {
    pub hits: Vec<RemoteHit>,
    pub degraded_to_lexical: bool,
}

/// One remote search hit, in the requested order within its store.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteHit {
    pub handle: Uuid,
    pub writer: WriterAttribution,
    pub kind: EntityKind,
    pub schema_id: String,
    pub snippet: String,
    pub created_at: time::OffsetDateTime,
    pub tags: Vec<String>,
}

/// One remote memory, as read.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteMemory {
    pub handle: Uuid,
    pub writer: WriterAttribution,
    pub kind: EntityKind,
    pub schema_id: String,
    pub title: Option<String>,
    pub body: String,
    pub created_at: time::OffsetDateTime,
    pub tags: Vec<String>,
}

/// A memory of another store that this caller read through the port: what
/// a derived memory may name in another store
/// (`DerivedMemory::external_origins`). Only [`OtherStoresService`] makes
/// one, so every such name was readable by its caller when the write was
/// prepared, and its layer is the one the store reported.
///
/// ```compile_fail
/// use proxima_core::storage_ports::other_stores::ReadOrigin;
/// fn forge(handle: uuid::Uuid) -> ReadOrigin {
///     ReadOrigin { handle, kind: proxima_core::verbs::query::EntityKind::Fact }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOrigin {
    handle: Uuid,
    kind: EntityKind,
}

impl ReadOrigin {
    #[must_use]
    pub const fn handle(&self) -> Uuid {
        self.handle
    }

    /// The layer the store reported for it.
    #[must_use]
    pub const fn kind(&self) -> EntityKind {
        self.kind
    }

    /// A read this crate's unit tests stand in for, without a port.
    #[cfg(test)]
    pub(crate) const fn for_tests(handle: Uuid, kind: EntityKind) -> Self {
        Self { handle, kind }
    }
}

/// The outcome of reading one remote handle.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteRead {
    Found(RemoteMemory),
    /// Unknown to the store, or not readable by this caller; the two are
    /// deliberately indistinguishable, as for a local handle.
    NotFound,
    /// The memory existed and was erased.
    Erased,
}

/// A bounded read result that carries the authenticated origin witness.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteOriginRead {
    Found(ReadOrigin, Box<RemoteMemory>),
    NotFound,
    Erased,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OtherStoresError {
    #[error("unknown store '{0}'")]
    UnknownStore(u16),
    #[error("other-store inventory includes local store '{0}'")]
    LocalStoreInInventory(u16),
    #[error("other-store inventory repeats store '{0}'")]
    DuplicateStoreInInventory(u16),
    #[error("store returned memory {returned} for requested memory {requested}")]
    MismatchedMemory { requested: Uuid, returned: Uuid },
    #[error("store unavailable: {0}")]
    Unavailable(String),
    #[error("store cannot honor search: {0}")]
    UnsupportedSearch(String),
    #[error("origin batch deadline exceeded")]
    DeadlineExceeded,
}

/// Read access to memory stores beside this one.
pub trait OtherStores: Send + Sync {
    /// The stores `caller` may read.
    fn stores<'a>(
        &'a self,
        caller: StoreCaller<'a>,
    ) -> BoxFuture<'a, Result<Vec<StoreInfo>, OtherStoresError>>;

    /// Search one store as `caller`, in the requested order. Every supplied
    /// filter and score option must be honored or refused with
    /// [`OtherStoresError::UnsupportedSearch`]; never retry a weaker request.
    fn search<'a>(
        &'a self,
        caller: StoreCaller<'a>,
        store: u16,
        args: &'a RemoteSearch,
    ) -> BoxFuture<'a, Result<RemoteSearchResult, OtherStoresError>>;

    /// Read one memory of one store as `caller`.
    fn read<'a>(
        &'a self,
        caller: StoreCaller<'a>,
        id: Uuid,
    ) -> BoxFuture<'a, Result<RemoteRead, OtherStoresError>>;
}

/// The wired port plus its per-store time bound, as tools find it in the
/// service bag.
#[derive(Clone)]
pub struct OtherStoresService {
    port: Arc<dyn OtherStores>,
    timeout: Duration,
}

impl fmt::Debug for OtherStoresService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OtherStoresService")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl OtherStoresService {
    #[must_use]
    pub fn new(port: Arc<dyn OtherStores>) -> Self {
        Self {
            port,
            timeout: DEFAULT_REMOTE_TIMEOUT,
        }
    }

    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub fn port(&self) -> &dyn OtherStores {
        self.port.as_ref()
    }

    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Read and check the remote-store inventory under the time bound.
    /// `None` means the list did not answer: it failed or timed out, which
    /// says nothing about which stores exist. An answered inventory is
    /// validated before either routing or schema rendering can use it.
    ///
    /// # Errors
    ///
    /// Refuses an answered inventory containing the local number or duplicates.
    pub async fn stores_bounded(
        &self,
        caller: StoreCaller<'_>,
        local_number: u16,
    ) -> Result<Option<StoreInventory>, OtherStoresError> {
        let Ok(Ok(stores)) = tokio::time::timeout(self.timeout, self.port.stores(caller)).await
        else {
            return Ok(None);
        };
        StoreInventory::new(local_number, stores).map(Some)
    }

    /// Read one handle under the time bound; a timeout is
    /// [`OtherStoresError::Unavailable`].
    ///
    /// # Errors
    ///
    /// Returns the port's error, or `Unavailable` past the bound.
    pub async fn read_bounded(
        &self,
        caller: StoreCaller<'_>,
        id: Uuid,
    ) -> Result<RemoteRead, OtherStoresError> {
        let read = tokio::time::timeout(self.timeout, self.port.read(caller, id))
            .await
            .unwrap_or_else(|_| Err(OtherStoresError::Unavailable("timed out".into())))?;
        if let RemoteRead::Found(memory) = &read
            && memory.handle != id
        {
            return Err(OtherStoresError::MismatchedMemory {
                requested: id,
                returned: memory.handle,
            });
        }
        Ok(read)
    }

    /// [`Self::read_bounded`], and for a memory the caller may read, the
    /// [`ReadOrigin`] that lets a derived memory name it. `None` for a handle
    /// the store does not know, will not show this caller, or erased.
    ///
    /// # Errors
    ///
    /// As [`Self::read_bounded`].
    pub async fn read_origin(
        &self,
        caller: StoreCaller<'_>,
        id: Uuid,
    ) -> Result<Option<(ReadOrigin, RemoteMemory)>, OtherStoresError> {
        Ok(match self.read_bounded(caller, id).await? {
            RemoteRead::Found(memory) => Some((
                ReadOrigin {
                    handle: id,
                    kind: memory.kind,
                },
                memory,
            )),
            RemoteRead::NotFound | RemoteRead::Erased => None,
        })
    }

    /// Read origins under one call-wide deadline and construct witnesses only
    /// for memories returned and UUID-checked by this service.
    pub async fn read_origins_bounded(
        &self,
        caller: StoreCaller<'_>,
        handles: &[Uuid],
    ) -> Vec<Result<RemoteOriginRead, OtherStoresError>> {
        self.read_many_bounded(caller, handles)
            .await
            .into_iter()
            .zip(handles)
            .map(|(result, handle)| {
                result.map(|read| match read {
                    RemoteRead::Found(memory) => RemoteOriginRead::Found(
                        ReadOrigin {
                            handle: *handle,
                            kind: memory.kind,
                        },
                        Box::new(memory),
                    ),
                    RemoteRead::NotFound => RemoteOriginRead::NotFound,
                    RemoteRead::Erased => RemoteOriginRead::Erased,
                })
            })
            .collect()
    }

    /// Read a batch of origins under one call-wide deadline. Reads for each
    /// store are limited to [`MAX_REMOTE_READS_PER_STORE`] in flight; each
    /// read retains [`Self::timeout`] as its individual bound.
    ///
    /// # Errors
    ///
    /// Returns [`OtherStoresError::DeadlineExceeded`] for reads that cannot
    /// start or finish before the call-wide deadline. Individual port errors
    /// remain attached to their corresponding result.
    pub async fn read_many_bounded(
        &self,
        caller: StoreCaller<'_>,
        handles: &[Uuid],
    ) -> Vec<Result<RemoteRead, OtherStoresError>> {
        let mut by_store: std::collections::BTreeMap<u16, Vec<(usize, Uuid)>> =
            std::collections::BTreeMap::new();
        for (index, handle) in handles.iter().copied().enumerate() {
            by_store
                .entry(crate::memory_store_number(handle))
                .or_default()
                .push((index, handle));
        }

        let deadline = tokio::time::Instant::now() + DEFAULT_REMOTE_BATCH_DEADLINE;
        let groups = by_store.into_values().map(|entries| async move {
            stream::iter(entries)
                .map(|(index, handle)| async move {
                    let result = if tokio::time::Instant::now() >= deadline {
                        Err(OtherStoresError::DeadlineExceeded)
                    } else {
                        tokio::time::timeout_at(deadline, self.read_bounded(caller, handle))
                            .await
                            .unwrap_or(Err(OtherStoresError::DeadlineExceeded))
                    };
                    (index, result)
                })
                .buffer_unordered(MAX_REMOTE_READS_PER_STORE)
                .collect::<Vec<_>>()
                .await
        });
        let mut results = stream::iter(groups)
            .buffer_unordered(usize::MAX)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        results.sort_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, result)| result).collect()
    }
}

/// Which stores one search goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreRoute {
    pub local: bool,
    pub remotes: Vec<u16>,
}

/// Pick the stores of one search.
///
/// - `requested` numbers stores explicitly; each must
///   be one the caller may read.
/// - Otherwise a `schema_id` declared by exactly one store — this one
///   counting as a store, by `local_declares` — routes there only.
/// - Otherwise this store and every readable store.
///
/// # Errors
///
/// Returns a message naming the first requested store that is not
/// readable, or an empty request.
pub fn route_stores(
    requested: Option<&[u16]>,
    schema_id: Option<&str>,
    local_declares: impl Fn(&str) -> bool,
    inventory: &StoreInventory,
) -> Result<StoreRoute, String> {
    let local_number = inventory.local_number;
    let readable = &inventory.remotes;
    if let Some(requested) = requested {
        if requested.is_empty() {
            return Err("stores must name at least one store when given".into());
        }
        let mut route = StoreRoute {
            local: false,
            remotes: Vec::new(),
        };
        for number in requested {
            if *number == local_number {
                route.local = true;
                continue;
            }
            let info = readable
                .iter()
                .find(|info| info.number == *number)
                .ok_or_else(|| format!("unknown store '{number}'"))?;
            if !route.remotes.contains(&info.number) {
                route.remotes.push(info.number);
            }
        }
        return Ok(route);
    }
    if let Some(schema_id) = schema_id {
        let mut declaring = readable
            .iter()
            .filter(|info| info.schema_ids.iter().any(|id| id == schema_id));
        match (
            local_declares(schema_id),
            declaring.next(),
            declaring.next(),
        ) {
            (true, None, _) => {
                return Ok(StoreRoute {
                    local: true,
                    remotes: Vec::new(),
                });
            }
            (false, Some(only), None) => {
                return Ok(StoreRoute {
                    local: false,
                    remotes: vec![only.number],
                });
            }
            _ => {}
        }
    }
    Ok(StoreRoute {
        local: true,
        remotes: readable.iter().map(|info| info.number).collect(),
    })
}

/// Reciprocal-rank fusion of ranked lists, `k` = [`STORE_RRF_K`]. Items are
/// never deduplicated across lists: two stores never share a memory. Ties
/// keep list order, then rank.
#[must_use]
pub fn rrf_merge<T>(lists: Vec<Vec<T>>) -> Vec<(T, f32)> {
    let mut scored: Vec<(usize, usize, T, f32)> = Vec::new();
    for (list_index, list) in lists.into_iter().enumerate() {
        for (rank, item) in list.into_iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let score = 1.0 / (STORE_RRF_K + rank as f32 + 1.0);
            scored.push((list_index, rank, item, score));
        }
    }
    scored.sort_by(|a, b| b.3.total_cmp(&a.3).then(a.1.cmp(&b.1)).then(a.0.cmp(&b.0)));
    scored
        .into_iter()
        .map(|(_, _, item, score)| (item, score))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(number: u16, name: &str, schemas: &[&str]) -> StoreInfo {
        StoreInfo {
            number,
            name: name.into(),
            description: String::new(),
            schema_ids: schemas.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn a_schema_declared_by_one_store_routes_there_only() {
        let stores = StoreInventory::new(
            3,
            vec![
                info(5, "wiki", &["wiki/page"]),
                info(7, "crm", &["crm/contact", "shared"]),
            ],
        )
        .expect("valid inventory");
        let local = |id: &str| matches!(id, "core/note" | "shared");
        let only = route_stores(None, Some("wiki/page"), local, &stores).expect("route");
        assert_eq!(
            only,
            StoreRoute {
                local: false,
                remotes: vec![5]
            }
        );

        let local_only = route_stores(None, Some("core/note"), local, &stores).expect("route");
        assert_eq!(
            local_only,
            StoreRoute {
                local: true,
                remotes: Vec::new()
            }
        );

        for schema in ["shared", "nobody/has"] {
            let all = route_stores(None, Some(schema), local, &stores).expect("route");
            assert!(all.local);
            assert_eq!(all.remotes, [5, 7]);
        }
        let shared = StoreInventory::new(
            3,
            vec![info(5, "wiki", &["shared"]), info(7, "crm", &["shared"])],
        )
        .expect("valid inventory");
        let all = route_stores(None, Some("shared"), |_| false, &shared).expect("route");
        assert!(all.local);
        assert_eq!(all.remotes, [5, 7]);

        let explicit =
            route_stores(Some(&[3, 7, 7]), Some("wiki/page"), local, &stores).expect("route");
        assert_eq!(
            explicit,
            StoreRoute {
                local: true,
                remotes: vec![7]
            }
        );

        let error = route_stores(Some(&[9]), None, local, &stores).expect_err("unknown store");
        assert!(error.contains('9'));
        assert!(route_stores(Some(&[]), None, local, &stores).is_err());
    }

    #[test]
    fn inventories_refuse_local_and_duplicate_store_numbers() {
        assert_eq!(
            StoreInventory::new(3, vec![info(5, "remote", &[]), info(3, "local", &[])]),
            Err(OtherStoresError::LocalStoreInInventory(3))
        );
        assert_eq!(
            StoreInventory::new(3, vec![info(5, "first", &[]), info(5, "second", &[])]),
            Err(OtherStoresError::DuplicateStoreInInventory(5))
        );
    }

    #[test]
    fn changing_display_names_does_not_change_routes() {
        let before =
            StoreInventory::new(3, vec![info(5, "wiki", &["wiki/page"])]).expect("valid inventory");
        let after = StoreInventory::new(3, vec![info(5, "knowledge", &["wiki/page"])])
            .expect("valid inventory");
        for requested in [None, Some([5].as_slice())] {
            assert_eq!(
                route_stores(requested, Some("wiki/page"), |_| false, &before),
                route_stores(requested, Some("wiki/page"), |_| false, &after),
            );
        }
    }

    #[derive(Debug)]
    struct WrongMemoryStore;

    impl OtherStores for WrongMemoryStore {
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
            _id: Uuid,
        ) -> BoxFuture<'a, Result<RemoteRead, OtherStoresError>> {
            Box::pin(async {
                Ok(RemoteRead::Found(RemoteMemory {
                    handle: Uuid::from_u128(2),
                    writer: crate::WriterAttribution {
                        by: Uuid::from_u128(3),
                        via: None,
                    },
                    kind: EntityKind::Fact,
                    schema_id: "remote/note".into(),
                    title: None,
                    body: "different memory".into(),
                    created_at: time::OffsetDateTime::UNIX_EPOCH,
                    tags: Vec::new(),
                }))
            })
        }
    }

    #[tokio::test]
    async fn a_port_cannot_bind_another_memorys_kind_to_the_requested_origin() {
        let owner = crate::UserId::new(Uuid::nil());
        let authz = AuthzContext::for_subject(owner, crate::AuthPath::System);
        let service = OtherStoresService::new(Arc::new(WrongMemoryStore));
        let requested = Uuid::from_u128(1);
        let error = service
            .read_origin(
                StoreCaller {
                    owner: owner.into(),
                    authz: &authz,
                },
                requested,
            )
            .await
            .expect_err("a mismatched memory cannot become an origin");
        assert_eq!(
            error,
            OtherStoresError::MismatchedMemory {
                requested,
                returned: Uuid::from_u128(2),
            }
        );
        let reads = service
            .read_origins_bounded(
                StoreCaller {
                    owner: owner.into(),
                    authz: &authz,
                },
                &[requested],
            )
            .await;
        assert!(matches!(
            &reads[0],
            Err(OtherStoresError::MismatchedMemory { requested: actual, returned })
                if *actual == requested && *returned == Uuid::from_u128(2)
        ));
    }

    #[test]
    fn rrf_interleaves_lists_by_rank() {
        let merged = rrf_merge(vec![vec!["l1", "l2", "l3"], vec!["r1"], vec!["s1", "s2"]]);
        let order: Vec<_> = merged.iter().map(|(item, _)| *item).collect();
        assert_eq!(order, ["l1", "r1", "s1", "l2", "s2", "l3"]);
        assert!((merged[0].1 - 1.0 / 61.0).abs() < f32::EPSILON);
        assert!(merged.windows(2).all(|w| w[0].1 >= w[1].1));
    }
    struct SlowOrigins {
        delay: Duration,
        active: std::sync::atomic::AtomicUsize,
        peak: std::sync::atomic::AtomicUsize,
    }

    impl SlowOrigins {
        fn new(delay: Duration) -> Self {
            Self {
                delay,
                active: std::sync::atomic::AtomicUsize::new(0),
                peak: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    struct ActiveRead<'a>(&'a std::sync::atomic::AtomicUsize);

    impl Drop for ActiveRead<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl OtherStores for SlowOrigins {
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
            _id: Uuid,
        ) -> BoxFuture<'a, Result<RemoteRead, OtherStoresError>> {
            Box::pin(async {
                use std::sync::atomic::Ordering::SeqCst;
                let active = self.active.fetch_add(1, SeqCst) + 1;
                self.peak.fetch_max(active, SeqCst);
                let _active_read = ActiveRead(&self.active);
                tokio::time::sleep(self.delay).await;
                Ok(RemoteRead::NotFound)
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn origin_batch_deadline_bounds_255_slow_foreign_origins() {
        let owner = OwnerId::new(uuid::Uuid::now_v7());
        let authz = AuthzContext::denied_for_owner(&owner);
        let caller = StoreCaller {
            owner,
            authz: &authz,
        };
        let handles = (0..255)
            .map(|_| crate::new_memory_uuid(1))
            .collect::<Vec<_>>();
        let port = Arc::new(SlowOrigins::new(Duration::from_millis(1_900)));
        let service = OtherStoresService::new(port.clone());
        let started = tokio::time::Instant::now();
        let reads = tokio::time::timeout(
            DEFAULT_REMOTE_BATCH_DEADLINE + Duration::from_millis(1),
            service.read_many_bounded(caller, &handles),
        )
        .await
        .expect("call-wide deadline bounds the batch");
        assert_eq!(started.elapsed(), DEFAULT_REMOTE_BATCH_DEADLINE);
        assert_eq!(port.peak.load(std::sync::atomic::Ordering::SeqCst), 8);
        assert_eq!(port.active.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(reads.len(), 255);
        assert!(
            reads
                .iter()
                .any(|read| matches!(read, Err(OtherStoresError::DeadlineExceeded)))
        );
    }
    #[tokio::test(start_paused = true)]
    async fn origin_batch_keeps_two_second_per_read_timeout() {
        let owner = OwnerId::new(uuid::Uuid::now_v7());
        let authz = AuthzContext::denied_for_owner(&owner);
        let caller = StoreCaller {
            owner,
            authz: &authz,
        };
        let service = OtherStoresService::new(Arc::new(SlowOrigins::new(Duration::from_secs(3))));
        let handles = [crate::new_memory_uuid(1)];
        let started = tokio::time::Instant::now();
        let reads = service.read_many_bounded(caller, &handles).await;
        assert_eq!(started.elapsed(), Duration::from_secs(2));
        assert!(
            matches!(&reads[0], Err(OtherStoresError::Unavailable(reason)) if reason == "timed out")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn origin_batch_reads_different_stores_concurrently() {
        let owner = OwnerId::new(uuid::Uuid::now_v7());
        let authz = AuthzContext::denied_for_owner(&owner);
        let caller = StoreCaller {
            owner,
            authz: &authz,
        };
        let handles = [1, 2]
            .into_iter()
            .flat_map(|store| (0..8).map(move |_| crate::new_memory_uuid(store)))
            .collect::<Vec<_>>();
        let port = Arc::new(SlowOrigins::new(Duration::from_millis(1_900)));
        let service = OtherStoresService::new(port.clone());
        let started = tokio::time::Instant::now();
        let reads = service.read_many_bounded(caller, &handles).await;
        assert_eq!(started.elapsed(), Duration::from_millis(1_900));
        assert_eq!(port.peak.load(std::sync::atomic::Ordering::SeqCst), 16);
        assert!(
            reads
                .iter()
                .all(|read| matches!(read, Ok(RemoteRead::NotFound)))
        );
    }
}

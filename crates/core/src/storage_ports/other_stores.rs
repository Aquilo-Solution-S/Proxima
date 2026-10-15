//! Other stores: memory stores beside this one that a caller may read
//! through, without their memories ever being copied here (issue #462).
//!
//! A host wires one [`OtherStores`] port through
//! `RuntimeBuilder::other_stores`; it reaches tools as an
//! [`OtherStoresService`] in the request's service bag. With no port wired,
//! every surface behaves exactly as it did before: the `stores` search
//! argument names only `local`, and a qualified handle is refused.
//!
//! A remote memory is addressed by a [`QualifiedHandle`],
//! `<store>:<handle>`. Local handles keep their one-uppercase-letter class
//! prefix (`F:`, `A:`, `P:`), and a [`StoreName`] starts with a lowercase
//! letter and is at least two characters long, so the two can never be
//! read as each other.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;

use crate::verbs::query::EntityKind;
use crate::{AuthzContext, OwnerId};

/// The name `stores` uses for this deployment's own memories.
pub const LOCAL_STORE: &str = "local";

/// How long a search waits for one other store before reporting it as
/// unanswered.
pub const DEFAULT_REMOTE_TIMEOUT: Duration = Duration::from_secs(2);

/// Reciprocal-rank-fusion constant used to merge store result lists.
pub const STORE_RRF_K: f32 = 60.0;

const MAX_STORE_NAME_CHARS: usize = 64;
const MAX_REMOTE_HANDLE_CHARS: usize = 512;

/// A configured store's name: `[a-z][a-z0-9_-]{1,63}`, never `local`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StoreName(String);

impl StoreName {
    /// # Errors
    ///
    /// Returns [`OtherStoresError::InvalidName`] when `raw` is outside the
    /// store-name charset, too short or long, or `local`.
    pub fn new(raw: impl Into<String>) -> Result<Self, OtherStoresError> {
        let raw = raw.into();
        if Self::is_valid(&raw) {
            Ok(Self(raw))
        } else {
            Err(OtherStoresError::InvalidName(raw))
        }
    }

    fn is_valid(raw: &str) -> bool {
        let mut chars = raw.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
            && (2..=MAX_STORE_NAME_CHARS).contains(&raw.len())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
            && raw != LOCAL_STORE
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StoreName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `<store>:<handle>`: a memory that lives in another store.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QualifiedHandle {
    pub store: StoreName,
    pub handle: String,
}

impl QualifiedHandle {
    /// Read `raw` as a qualified handle. `None` means `raw` is not one (a
    /// local handle such as `F:<uuid>`, or nothing handle-shaped at all);
    /// the caller then treats it as local.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let (store, handle) = raw.split_once(':')?;
        let store = StoreName::new(store).ok()?;
        if handle.is_empty()
            || handle.chars().count() > MAX_REMOTE_HANDLE_CHARS
            || handle.chars().any(char::is_control)
        {
            return None;
        }
        Some(Self {
            store,
            handle: handle.to_string(),
        })
    }
}

impl fmt::Display for QualifiedHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.store, self.handle)
    }
}

/// Who is asking: the store decides what this caller may see.
#[derive(Debug, Clone, Copy)]
pub struct StoreCaller<'a> {
    pub owner: OwnerId,
    pub authz: &'a AuthzContext,
}

/// One store the caller may read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreInfo {
    pub name: StoreName,
    pub description: String,
    /// Schema ids the store holds; a search filtered to a `schema_id`
    /// exactly one store declares goes to that store only.
    pub schema_ids: Vec<String>,
}

/// The search a remote store is asked to run.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteSearch {
    pub query: String,
    pub limit: u32,
    pub kind: Option<EntityKind>,
    pub schema_id: Option<String>,
}

/// One remote search hit, best first within its store.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteHit {
    pub handle: String,
    pub kind: EntityKind,
    pub schema_id: String,
    pub snippet: String,
    pub created_at: time::OffsetDateTime,
    pub tags: Vec<String>,
}

/// One remote memory, as read.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteMemory {
    pub handle: String,
    pub kind: EntityKind,
    pub schema_id: String,
    pub title: Option<String>,
    pub body: String,
    pub created_at: time::OffsetDateTime,
    pub tags: Vec<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OtherStoresError {
    #[error("invalid store name '{0}': expected [a-z][a-z0-9_-]{{1,63}}, not 'local'")]
    InvalidName(String),
    #[error("unknown store '{0}'")]
    UnknownStore(String),
    #[error("store unavailable: {0}")]
    Unavailable(String),
}

/// Read access to memory stores beside this one.
pub trait OtherStores: Send + Sync {
    /// The stores `caller` may read.
    fn stores<'a>(
        &'a self,
        caller: StoreCaller<'a>,
    ) -> BoxFuture<'a, Result<Vec<StoreInfo>, OtherStoresError>>;

    /// Search one store as `caller`, best hit first.
    fn search<'a>(
        &'a self,
        caller: StoreCaller<'a>,
        store: &'a StoreName,
        args: &'a RemoteSearch,
    ) -> BoxFuture<'a, Result<Vec<RemoteHit>, OtherStoresError>>;

    /// Read one memory of one store as `caller`.
    fn read<'a>(
        &'a self,
        caller: StoreCaller<'a>,
        store: &'a StoreName,
        handle: &'a str,
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

    /// Read one handle under the time bound; a timeout is
    /// [`OtherStoresError::Unavailable`].
    ///
    /// # Errors
    ///
    /// Returns the port's error, or `Unavailable` past the bound.
    pub async fn read_bounded(
        &self,
        caller: StoreCaller<'_>,
        handle: &QualifiedHandle,
    ) -> Result<RemoteRead, OtherStoresError> {
        tokio::time::timeout(
            self.timeout,
            self.port.read(caller, &handle.store, &handle.handle),
        )
        .await
        .unwrap_or_else(|_| Err(OtherStoresError::Unavailable("timed out".into())))
    }
}

/// Which stores one search goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreRoute {
    pub local: bool,
    pub remotes: Vec<StoreName>,
}

/// Pick the stores of one search.
///
/// - `requested` names stores explicitly (`local` for this one); each must
///   be one the caller may read.
/// - Otherwise a `schema_id` declared by exactly one store routes there only.
/// - Otherwise this store and every readable store.
///
/// # Errors
///
/// Returns a message naming the first requested store that is not
/// readable, or an empty request.
pub fn route_stores(
    requested: Option<&[String]>,
    schema_id: Option<&str>,
    readable: &[StoreInfo],
) -> Result<StoreRoute, String> {
    if let Some(requested) = requested {
        if requested.is_empty() {
            return Err("stores must name at least one store when given".into());
        }
        let mut route = StoreRoute {
            local: false,
            remotes: Vec::new(),
        };
        for name in requested {
            if name == LOCAL_STORE {
                route.local = true;
                continue;
            }
            let info = readable
                .iter()
                .find(|info| info.name.as_str() == name)
                .ok_or_else(|| format!("unknown store '{name}'"))?;
            if !route.remotes.contains(&info.name) {
                route.remotes.push(info.name.clone());
            }
        }
        return Ok(route);
    }
    if let Some(schema_id) = schema_id {
        let mut declaring = readable
            .iter()
            .filter(|info| info.schema_ids.iter().any(|id| id == schema_id));
        if let (Some(only), None) = (declaring.next(), declaring.next()) {
            return Ok(StoreRoute {
                local: false,
                remotes: vec![only.name.clone()],
            });
        }
    }
    Ok(StoreRoute {
        local: true,
        remotes: readable.iter().map(|info| info.name.clone()).collect(),
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

    fn info(name: &str, schemas: &[&str]) -> StoreInfo {
        StoreInfo {
            name: StoreName::new(name).expect("name"),
            description: String::new(),
            schema_ids: schemas.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn local_handles_are_never_qualified() {
        let id = "018f0000-0000-7000-8000-000000000001";
        for prefix in ["F", "A", "P", "G"] {
            assert_eq!(QualifiedHandle::parse(&format!("{prefix}:{id}")), None);
        }
        assert_eq!(QualifiedHandle::parse(id), None);
        assert_eq!(QualifiedHandle::parse("local:abc"), None);
        assert_eq!(QualifiedHandle::parse("remote:"), None);
        let parsed = QualifiedHandle::parse("remote:abc").expect("qualified");
        assert_eq!(parsed.store.as_str(), "remote");
        assert_eq!(parsed.handle, "abc");
        assert_eq!(parsed.to_string(), "remote:abc");
        // The handle part may itself carry colons.
        assert_eq!(
            QualifiedHandle::parse("wiki:F:x")
                .expect("qualified")
                .handle,
            "F:x"
        );
    }

    #[test]
    fn store_names_are_restricted() {
        for bad in ["", "a", "Remote", "1st", "local", "has space", "a:b"] {
            assert!(StoreName::new(bad).is_err(), "{bad}");
        }
        for good in ["ab", "team-wiki", "s3_archive", "x9"] {
            assert!(StoreName::new(good).is_ok(), "{good}");
        }
    }

    #[test]
    fn a_schema_declared_by_one_store_routes_there_only() {
        let stores = [
            info("wiki", &["wiki/page"]),
            info("crm", &["crm/contact", "shared"]),
        ];
        let only = route_stores(None, Some("wiki/page"), &stores).expect("route");
        assert_eq!(
            only,
            StoreRoute {
                local: false,
                remotes: vec![stores[0].name.clone()]
            }
        );

        let shared = [info("wiki", &["shared"]), info("crm", &["shared"])];
        let all = route_stores(None, Some("shared"), &shared).expect("route");
        assert!(all.local);
        assert_eq!(all.remotes.len(), 2);

        let undeclared = route_stores(None, Some("core/note"), &stores).expect("route");
        assert!(undeclared.local);
        assert_eq!(undeclared.remotes.len(), 2);

        let explicit = route_stores(
            Some(&["local".into(), "crm".into()]),
            Some("wiki/page"),
            &stores,
        )
        .expect("route");
        assert_eq!(
            explicit,
            StoreRoute {
                local: true,
                remotes: vec![stores[1].name.clone()]
            }
        );

        assert!(route_stores(Some(&["nope".into()]), None, &stores).is_err());
        assert!(route_stores(Some(&[]), None, &stores).is_err());
    }

    #[test]
    fn rrf_interleaves_lists_by_rank() {
        let merged = rrf_merge(vec![vec!["l1", "l2", "l3"], vec!["r1"], vec!["s1", "s2"]]);
        let order: Vec<_> = merged.iter().map(|(item, _)| *item).collect();
        assert_eq!(order, ["l1", "r1", "s1", "l2", "s2", "l3"]);
        assert!((merged[0].1 - 1.0 / 61.0).abs() < f32::EPSILON);
        assert!(merged.windows(2).all(|w| w[0].1 >= w[1].1));
    }
}

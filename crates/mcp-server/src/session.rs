use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use proxima_core::OwnerId;
use tokio::sync::RwLock;

/// Idle lifetime of a session binding. A binding not observed within this
/// window is treated as unknown and pruned. Bindings are in-process only,
/// so a generous ceiling covers long-lived Streamable-HTTP clients while
/// still bounding memory; it stays in the same order of magnitude as the
/// stream max-lifetime ceiling.
const SESSION_IDLE_TTL: Duration = Duration::from_hours(1);

/// Hard cap on concurrently bound sessions. When full, a principal may evict
/// only one of its own bindings; a new principal is refused rather than
/// evicting another caller's session.
const MAX_SESSIONS: usize = 4096;

/// Maximum number of sessions one authenticated principal may hold. When the
/// principal reaches this limit, only its own least-recently-seen session is
/// evicted.
const MAX_SESSIONS_PER_PRINCIPAL: usize = 64;

struct Binding {
    owner: OwnerId,
    principal: OwnerId,
    last_seen: AtomicU64,
}

#[derive(Clone)]
pub struct McpSessionBindings {
    inner: Arc<RwLock<HashMap<String, Binding>>>,
    idle_ttl: Duration,
    max_sessions: usize,
    clock_origin: Instant,
}

impl Default for McpSessionBindings {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            idle_ttl: SESSION_IDLE_TTL,
            max_sessions: MAX_SESSIONS,
            clock_origin: Instant::now(),
        }
    }
}

impl std::fmt::Debug for McpSessionBindings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpSessionBindings").finish_non_exhaustive()
    }
}

impl McpSessionBindings {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bindings with explicit idle TTL / size cap. Test-only seam for
    /// exercising eviction deterministically.
    #[cfg(test)]
    fn with_limits(idle_ttl: Duration, max_sessions: usize) -> Self {
        Self {
            inner: Arc::default(),
            idle_ttl,
            max_sessions,
            clock_origin: Instant::now(),
        }
    }

    /// Bind a transport session to its selected owner and authenticated
    /// principal. Returns `false` when the global bound is full of other
    /// principals' sessions; no other principal is evicted in that case.
    pub async fn bind(
        &self,
        session_id: impl Into<String>,
        owner: OwnerId,
        principal: OwnerId,
    ) -> bool {
        self.bind_at(session_id.into(), owner, principal, Instant::now())
            .await
    }

    async fn bind_at(
        &self,
        session_id: String,
        owner: OwnerId,
        principal: OwnerId,
        now: Instant,
    ) -> bool {
        let mut guard = self.inner.write().await;
        let map = &mut *guard;
        self.prune_idle(map, now);
        Self::prune_principal(map, principal);
        if map.len() >= self.max_sessions {
            let Some((_, id)) = map
                .iter()
                .filter(|(_, binding)| binding.principal == principal)
                .min_by_key(|(_, binding)| binding.last_seen.load(Ordering::Relaxed))
                .map(|(id, binding)| (binding.last_seen.load(Ordering::Relaxed), id.clone()))
            else {
                // The global bound is full of other principals' sessions. A
                // new principal cannot evict another caller's session.
                return false;
            };
            map.remove(&id);
        }
        map.insert(
            session_id,
            Binding {
                owner,
                principal,
                last_seen: AtomicU64::new(self.timestamp_at(now)),
            },
        );
        true
    }

    pub async fn owner_for(&self, session_id: &str) -> Option<OwnerId> {
        self.owner_for_at(session_id, Instant::now()).await
    }

    async fn owner_for_at(&self, session_id: &str, now: Instant) -> Option<OwnerId> {
        let now = self.timestamp_at(now);
        {
            let guard = self.inner.read().await;
            let binding = guard.get(session_id)?;
            let last_seen = binding.last_seen.load(Ordering::Relaxed);
            if !self.is_expired(last_seen, now) {
                // Refresh so the TTL is idle-based, not creation-based:
                // active sessions never expire mid-use.
                binding.last_seen.fetch_max(now, Ordering::Relaxed);
                return Some(binding.owner);
            }
        }

        // Aged past the idle TTL: acquire the write lock only for removal,
        // then report unknown so the transport answers 404 and the client
        // re-initializes.
        let mut guard = self.inner.write().await;
        if guard
            .get(session_id)
            .is_some_and(|binding| self.is_expired(binding.last_seen.load(Ordering::Relaxed), now))
        {
            guard.remove(session_id);
        }
        None
    }

    fn prune_idle(&self, map: &mut HashMap<String, Binding>, now: Instant) {
        let now = self.timestamp_at(now);
        map.retain(|_, binding| !self.is_expired(binding.last_seen.load(Ordering::Relaxed), now));
    }

    fn prune_principal(map: &mut HashMap<String, Binding>, principal: OwnerId) {
        let mut by_age: Vec<(u64, String)> = map
            .iter()
            .filter(|(_, binding)| binding.principal == principal)
            .map(|(id, binding)| (binding.last_seen.load(Ordering::Relaxed), id.clone()))
            .collect();
        let overflow = (by_age.len() + 1).saturating_sub(MAX_SESSIONS_PER_PRINCIPAL);
        if overflow > 0 {
            by_age.sort_unstable();
            for (_, id) in by_age.into_iter().take(overflow) {
                map.remove(&id);
            }
        }
    }

    fn timestamp_at(&self, now: Instant) -> u64 {
        u64::try_from(now.saturating_duration_since(self.clock_origin).as_nanos())
            .unwrap_or(u64::MAX)
    }

    fn is_expired(&self, last_seen: u64, now: u64) -> bool {
        now.saturating_sub(last_seen) >= u64::try_from(self.idle_ttl.as_nanos()).unwrap_or(u64::MAX)
    }
}

#[must_use]
pub fn parse_owner_key(raw: &str) -> Option<OwnerId> {
    raw.parse().ok()
}

#[must_use]
pub fn owner_key(owner: OwnerId) -> String {
    owner.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_core::{GroupId, OwnerId, UserId};
    use uuid::Uuid;

    fn user_owner() -> OwnerId {
        OwnerId::from(UserId::new(Uuid::now_v7()))
    }

    #[test]
    fn owner_keys_round_trip() {
        let personal = OwnerId::from(UserId::new(Uuid::now_v7()));
        let group = OwnerId::from(GroupId::new(Uuid::now_v7()));

        assert_eq!(parse_owner_key(&owner_key(personal)), Some(personal));
        assert_eq!(parse_owner_key(&owner_key(group)), Some(group));
        assert_eq!(parse_owner_key("current"), None);
        assert_eq!(
            parse_owner_key("world:00000000-0000-0000-0000-000000000001"),
            None
        );
        assert_eq!(parse_owner_key("personal:not-a-uuid"), None);
    }

    /// The `X-Proxima-Owner` header carries the bare id: the old
    /// kind-prefixed spelling of a valid id is refused, with no alias.
    #[test]
    fn an_old_kind_prefixed_owner_header_is_refused() {
        let id = Uuid::now_v7();
        assert_eq!(parse_owner_key(&format!("personal:{id}")), None);
        assert_eq!(parse_owner_key(&format!("group:{id}")), None);
        assert_eq!(parse_owner_key(&id.to_string()), Some(OwnerId::new(id)));
    }

    #[tokio::test]
    async fn binds_and_reads_back_owner() {
        let bindings = McpSessionBindings::new();
        let owner = user_owner();
        bindings.bind("s1", owner, owner).await;
        assert_eq!(bindings.owner_for("s1").await, Some(owner));
        assert_eq!(bindings.owner_for("unknown").await, None);
    }

    #[tokio::test]
    async fn bind_prunes_idle_binding() {
        let bindings = McpSessionBindings::new();
        let owner = user_owner();
        let base = Instant::now();
        bindings.bind_at("old".into(), owner, owner, base).await;
        // Binding a fresh session past the idle TTL prunes the aged one.
        let aged = base + SESSION_IDLE_TTL + Duration::from_secs(1);
        bindings.bind_at("new".into(), owner, owner, aged).await;
        assert_eq!(bindings.owner_for_at("old", aged).await, None);
        assert_eq!(bindings.owner_for_at("new", aged).await, Some(owner));
    }

    #[tokio::test]
    async fn read_past_ttl_expires_binding() {
        let bindings = McpSessionBindings::new();
        let owner = user_owner();
        let base = Instant::now();
        bindings.bind_at("s".into(), owner, owner, base).await;
        // Within the TTL the binding resolves and refreshes.
        assert_eq!(
            bindings
                .owner_for_at("s", base + Duration::from_secs(1))
                .await,
            Some(owner)
        );
        // Past the TTL the binding is treated as unknown and dropped.
        let aged = base + SESSION_IDLE_TTL + Duration::from_secs(2);
        assert_eq!(bindings.owner_for_at("s", aged).await, None);
    }

    #[tokio::test]
    async fn size_cap_evicts_least_recently_seen() {
        let bindings = McpSessionBindings::with_limits(SESSION_IDLE_TTL, 2);
        let owner = user_owner();
        let base = Instant::now();
        bindings.bind_at("s1".into(), owner, owner, base).await;
        bindings
            .bind_at("s2".into(), owner, owner, base + Duration::from_secs(1))
            .await;
        // Touch s1 so s2 becomes the least-recently-seen entry.
        assert_eq!(
            bindings
                .owner_for_at("s1", base + Duration::from_secs(2))
                .await,
            Some(owner)
        );
        // A third binding evicts the LRU (s2), not the freshly-touched s1.
        bindings
            .bind_at("s3".into(), owner, owner, base + Duration::from_secs(3))
            .await;
        let later = base + Duration::from_secs(4);
        assert_eq!(bindings.owner_for_at("s2", later).await, None);
        assert_eq!(bindings.owner_for_at("s1", later).await, Some(owner));
        assert_eq!(bindings.owner_for_at("s3", later).await, Some(owner));
    }

    #[tokio::test]
    async fn principal_cap_evicts_only_that_principals_oldest_session() {
        let bindings = McpSessionBindings::with_limits(SESSION_IDLE_TTL, 65);
        let principal = user_owner();
        let other_principal = user_owner();
        let selected_owner = user_owner();
        let base = Instant::now();

        bindings
            .bind_at("other".into(), other_principal, other_principal, base)
            .await;
        for index in 0..MAX_SESSIONS_PER_PRINCIPAL {
            bindings
                .bind_at(
                    format!("principal-{index}"),
                    selected_owner,
                    principal,
                    base + Duration::from_secs(index as u64 + 1),
                )
                .await;
        }
        bindings
            .bind_at(
                "principal-new".into(),
                selected_owner,
                principal,
                base + Duration::from_secs(100),
            )
            .await;

        assert_eq!(
            bindings.owner_for_at("other", base).await,
            Some(other_principal)
        );
        assert_eq!(
            bindings
                .owner_for_at("principal-0", base + Duration::from_secs(101))
                .await,
            None
        );
        assert_eq!(
            bindings
                .owner_for_at("principal-new", base + Duration::from_secs(101))
                .await,
            Some(selected_owner)
        );
        for index in 1..MAX_SESSIONS_PER_PRINCIPAL {
            assert_eq!(
                bindings
                    .owner_for_at(
                        &format!("principal-{index}"),
                        base + Duration::from_secs(101),
                    )
                    .await,
                Some(selected_owner)
            );
        }
        let guard = bindings.inner.read().await;
        assert_eq!(
            guard
                .values()
                .filter(|binding| binding.principal == principal)
                .count(),
            MAX_SESSIONS_PER_PRINCIPAL
        );
    }

    #[tokio::test]
    async fn a_new_principal_cannot_evict_when_global_bound_is_full() {
        let bindings = McpSessionBindings::with_limits(SESSION_IDLE_TTL, 2);
        let first = user_owner();
        let second = user_owner();
        let newcomer = user_owner();
        let base = Instant::now();
        bindings.bind_at("first".into(), first, first, base).await;
        bindings
            .bind_at(
                "second".into(),
                second,
                second,
                base + Duration::from_secs(1),
            )
            .await;

        assert!(
            !bindings
                .bind_at(
                    "newcomer".into(),
                    newcomer,
                    newcomer,
                    base + Duration::from_secs(2),
                )
                .await
        );
        assert_eq!(bindings.owner_for_at("first", base).await, Some(first));
        assert_eq!(
            bindings
                .owner_for_at("second", base + Duration::from_secs(2))
                .await,
            Some(second)
        );
        assert_eq!(
            bindings
                .owner_for_at("newcomer", base + Duration::from_secs(2))
                .await,
            None
        );
    }
}

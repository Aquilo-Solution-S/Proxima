use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use proxima_core::{Owner, parse_external_key};
use tokio::sync::RwLock;

/// Idle lifetime of a session binding. A binding not observed within this
/// window is treated as unknown and pruned. Bindings are in-process only,
/// so a generous ceiling covers long-lived Streamable-HTTP clients while
/// still bounding memory; it stays in the same order of magnitude as the
/// stream max-lifetime ceiling.
const SESSION_IDLE_TTL: Duration = Duration::from_hours(1);

/// Hard cap on concurrently bound sessions. When full, the
/// least-recently-seen binding is evicted so a client that never releases
/// a session id (or a flood of `initialize`s) cannot grow the map without
/// bound.
const MAX_SESSIONS: usize = 4096;

#[derive(Clone, Copy)]
struct Binding {
    owner: Owner,
    last_seen: Instant,
}

#[derive(Clone)]
pub struct McpSessionBindings {
    inner: Arc<RwLock<HashMap<String, Binding>>>,
    idle_ttl: Duration,
    max_sessions: usize,
}

impl Default for McpSessionBindings {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            idle_ttl: SESSION_IDLE_TTL,
            max_sessions: MAX_SESSIONS,
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
        }
    }

    pub async fn bind(&self, session_id: impl Into<String>, owner: Owner) {
        self.bind_at(session_id.into(), owner, Instant::now()).await;
    }

    async fn bind_at(&self, session_id: String, owner: Owner, now: Instant) {
        let mut guard = self.inner.write().await;
        let map = &mut *guard;
        self.prune(map, now);
        map.insert(
            session_id,
            Binding {
                owner,
                last_seen: now,
            },
        );
    }

    pub async fn owner_for(&self, session_id: &str) -> Option<Owner> {
        self.owner_for_at(session_id, Instant::now()).await
    }

    async fn owner_for_at(&self, session_id: &str, now: Instant) -> Option<Owner> {
        let mut guard = self.inner.write().await;
        match guard.get_mut(session_id) {
            Some(binding) if now.saturating_duration_since(binding.last_seen) < self.idle_ttl => {
                // Refresh so the TTL is idle-based, not creation-based:
                // active sessions never expire mid-use.
                binding.last_seen = now;
                Some(binding.owner)
            }
            // Aged past the idle TTL: drop it and report unknown so the
            // transport answers 404 and the client re-initializes.
            Some(_) => {
                guard.remove(session_id);
                None
            }
            None => None,
        }
    }

    /// Drop idle-expired bindings, then enforce the hard size cap by
    /// evicting the least-recently-seen entries, leaving room for one
    /// pending insert.
    fn prune(&self, map: &mut HashMap<String, Binding>, now: Instant) {
        map.retain(|_, binding| now.saturating_duration_since(binding.last_seen) < self.idle_ttl);
        if map.len() >= self.max_sessions {
            let overflow = map.len() + 1 - self.max_sessions;
            let mut by_age: Vec<(Instant, String)> = map
                .iter()
                .map(|(id, binding)| (binding.last_seen, id.clone()))
                .collect();
            by_age.sort_unstable();
            for (_, id) in by_age.into_iter().take(overflow) {
                map.remove(&id);
            }
        }
    }
}

#[must_use]
pub fn parse_owner_key(raw: &str) -> Option<Owner> {
    parse_external_key(raw).ok()
}

#[must_use]
pub fn owner_key(owner: Owner) -> String {
    owner.external_key()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_core::{GroupId, OwnerRef, UserId};
    use uuid::Uuid;

    fn user_owner() -> Owner {
        OwnerRef::Personal(UserId::new(Uuid::now_v7()))
    }

    #[test]
    fn owner_keys_round_trip() {
        let personal = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let group = OwnerRef::Group(GroupId::new(Uuid::now_v7()));

        assert_eq!(
            parse_owner_key(&owner_key(OwnerRef::World)),
            Some(OwnerRef::World)
        );
        assert_eq!(parse_owner_key(&owner_key(personal)), Some(personal));
        assert_eq!(parse_owner_key(&owner_key(group)), Some(group));
        assert_eq!(parse_owner_key("current"), None);
        assert_eq!(parse_owner_key("world"), None);
        assert_eq!(parse_owner_key("personal:not-a-uuid"), None);
    }

    #[tokio::test]
    async fn binds_and_reads_back_owner() {
        let bindings = McpSessionBindings::new();
        let owner = user_owner();
        bindings.bind("s1", owner).await;
        assert_eq!(bindings.owner_for("s1").await, Some(owner));
        assert_eq!(bindings.owner_for("unknown").await, None);
    }

    #[tokio::test]
    async fn bind_prunes_idle_binding() {
        let bindings = McpSessionBindings::new();
        let owner = user_owner();
        let base = Instant::now();
        bindings.bind_at("old".into(), owner, base).await;
        // Binding a fresh session past the idle TTL prunes the aged one.
        let aged = base + SESSION_IDLE_TTL + Duration::from_secs(1);
        bindings.bind_at("new".into(), owner, aged).await;
        assert_eq!(bindings.owner_for_at("old", aged).await, None);
        assert_eq!(bindings.owner_for_at("new", aged).await, Some(owner));
    }

    #[tokio::test]
    async fn read_past_ttl_expires_binding() {
        let bindings = McpSessionBindings::new();
        let owner = user_owner();
        let base = Instant::now();
        bindings.bind_at("s".into(), owner, base).await;
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
        bindings.bind_at("s1".into(), owner, base).await;
        bindings
            .bind_at("s2".into(), owner, base + Duration::from_secs(1))
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
            .bind_at("s3".into(), owner, base + Duration::from_secs(3))
            .await;
        let later = base + Duration::from_secs(4);
        assert_eq!(bindings.owner_for_at("s2", later).await, None);
        assert_eq!(bindings.owner_for_at("s1", later).await, Some(owner));
        assert_eq!(bindings.owner_for_at("s3", later).await, Some(owner));
    }
}

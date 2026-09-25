//! Per-owner `notifications/tools/list_changed` delivery.
//!
//! A caller's tool list is projected from its owner and token, so a host
//! that changes what an owner may call (a pack turned on, a binding
//! withdrawn) must tell that owner's connected clients, and only them:
//! which tools exist is itself tenant information. There is one kind of
//! listener per MCP lifecycle:
//!
//! - a session opened with `initialize` (revisions up to `2025-11-25`): the
//!   session's peer, registered at the handshake;
//! - a `subscriptions/listen` stream (`2026-07-28`) that asked for
//!   `toolsListChanged`: its sink, registered while the stream is open.
//!
//! Registrations live in this process. A host running several replicas
//! calls [`ToolListNotifier::notify`] on every replica that learns of a
//! change.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use proxima_core::Owner;
use rmcp::service::{Peer, RoleServer, SubscriptionSink};

/// Tells one owner's connected MCP clients that their tool list changed.
///
/// Attach one to the tool host ([`crate::McpToolHost::with_tool_list_notifier`])
/// and keep a clone: the handler then advertises `tools.listChanged` and
/// registers each caller under its owner, and the host calls
/// [`Self::notify`] whenever that owner's tool list changes.
#[derive(Clone, Default)]
pub struct ToolListNotifier {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    next_id: AtomicU64,
    listeners: Mutex<HashMap<Owner, Vec<(u64, Listener)>>>,
}

#[derive(Clone)]
enum Listener {
    Session(Peer<RoleServer>),
    Subscription(SubscriptionSink),
}

impl Listener {
    /// A session whose transport closed. A subscription leaves through its
    /// [`Registration`] instead.
    fn is_gone(&self) -> bool {
        match self {
            Self::Session(peer) => peer.is_transport_closed(),
            Self::Subscription(_) => false,
        }
    }

    async fn send(&self) -> bool {
        match self {
            Self::Session(peer) => peer.notify_tool_list_changed().await.is_ok(),
            Self::Subscription(sink) => sink.notify_tool_list_changed().await.is_ok(),
        }
    }
}

impl std::fmt::Debug for ToolListNotifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolListNotifier").finish_non_exhaustive()
    }
}

impl ToolListNotifier {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Send `notifications/tools/list_changed` to every live listener of
    /// `owner` and to no one else. Returns how many it reached; a listener
    /// that can no longer be reached is dropped.
    pub async fn notify(&self, owner: &Owner) -> usize {
        let listeners = {
            let mut map = self.lock();
            let Some(entries) = map.get_mut(owner) else {
                return 0;
            };
            entries.retain(|(_, listener)| !listener.is_gone());
            entries.clone()
        };
        let mut reached = 0;
        for (id, listener) in listeners {
            if listener.send().await {
                reached += 1;
            } else {
                self.remove(owner, id);
            }
        }
        reached
    }

    pub(crate) fn register_session(&self, owner: Owner, peer: Peer<RoleServer>) {
        self.insert(owner, Listener::Session(peer));
    }

    /// Registered until the returned guard drops, which `listen` does when
    /// the stream ends.
    pub(crate) fn register_subscription(
        &self,
        owner: Owner,
        sink: SubscriptionSink,
    ) -> Registration {
        let id = self.insert(owner, Listener::Subscription(sink));
        Registration {
            notifier: self.clone(),
            owner,
            id,
        }
    }

    fn insert(&self, owner: Owner, listener: Listener) -> u64 {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let mut map = self.lock();
        let entries = map.entry(owner).or_default();
        // Sessions that closed since the last change would otherwise
        // accumulate until one arrives.
        entries.retain(|(_, listener)| !listener.is_gone());
        entries.push((id, listener));
        id
    }

    fn remove(&self, owner: &Owner, id: u64) {
        let mut map = self.lock();
        if let Some(entries) = map.get_mut(owner) {
            entries.retain(|(entry, _)| *entry != id);
            if entries.is_empty() {
                map.remove(owner);
            }
        }
    }

    /// No code holding this lock can panic mid-update, so a poisoned map
    /// is still consistent.
    fn lock(&self) -> MutexGuard<'_, HashMap<Owner, Vec<(u64, Listener)>>> {
        self.inner
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// One `subscriptions/listen` stream's registration; dropping it
/// deregisters the stream.
pub(crate) struct Registration {
    notifier: ToolListNotifier,
    owner: Owner,
    id: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.notifier.remove(&self.owner, self.id);
    }
}

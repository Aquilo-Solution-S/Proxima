use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use uuid::Uuid;

use crate::personality::{PersonalityInstanceId, WakeChainDepth};
use crate::{MemoryId, Owner};

#[derive(Debug, Clone)]
pub struct WakeTokenContext {
    pub invocation_id: Uuid,
    pub personality_instance_id: Uuid,
    pub wake_entry_id: Uuid,
    pub change_event_seq: Uuid,
    pub owner: Owner,
    pub palette: Vec<String>,
    pub model_id: String,
    pub max_rounds: u32,
    pub current_root_perspective_memory_id: MemoryId,
    pub triggering_event_memory_id: MemoryId,
    pub triggering_event_depth: WakeChainDepth,
    pub read_log: Arc<tokio::sync::Mutex<Vec<(MemoryId, WakeChainDepth)>>>,
}

impl WakeTokenContext {
    #[must_use]
    pub fn personality_instance_id(&self) -> PersonalityInstanceId {
        PersonalityInstanceId::new(self.personality_instance_id)
    }
}

#[derive(Debug)]
struct Entry {
    ctx: WakeTokenContext,
    idle_expires_at: Instant,
    max_expires_at: Instant,
}

#[derive(Debug)]
pub struct WakeTokenStore {
    idle_ttl: Duration,
    inner: Arc<RwLock<HashMap<Uuid, Entry>>>,
}

impl WakeTokenStore {
    pub fn new(idle_ttl: Duration) -> Self {
        Self {
            idle_ttl,
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn mint(&self, ctx: WakeTokenContext) -> Uuid {
        self.mint_with_max_lifetime(ctx, self.idle_ttl).await
    }

    pub async fn mint_with_max_lifetime(
        &self,
        ctx: WakeTokenContext,
        max_lifetime: Duration,
    ) -> Uuid {
        let token = Uuid::new_v4();
        let now = Instant::now();
        let max_expires_at = now + max_lifetime;
        let entry = Entry {
            ctx,
            idle_expires_at: (now + self.idle_ttl).min(max_expires_at),
            max_expires_at,
        };
        self.inner.write().await.insert(token, entry);
        token
    }

    pub async fn resolve(&self, token: Uuid) -> Option<WakeTokenContext> {
        let mut guard = self.inner.write().await;
        let entry = guard.get_mut(&token)?;
        let now = Instant::now();
        if entry.idle_expires_at <= now || entry.max_expires_at <= now {
            guard.remove(&token);
            return None;
        }
        entry.idle_expires_at = (now + self.idle_ttl).min(entry.max_expires_at);
        Some(entry.ctx.clone())
    }

    pub async fn revoke(&self, token: Uuid) {
        self.inner.write().await.remove(&token);
    }

    pub async fn sweep_expired(&self) -> usize {
        let now = Instant::now();
        let mut guard = self.inner.write().await;
        let before = guard.len();
        guard.retain(|_, e| e.idle_expires_at > now && e.max_expires_at > now);
        before - guard.len()
    }
}

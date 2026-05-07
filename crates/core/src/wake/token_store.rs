use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use uuid::Uuid;

use crate::Owner;

#[derive(Debug, Clone)]
pub struct WakeTokenContext {
    pub invocation_id: Uuid,
    pub personality_instance_id: Uuid,
    pub wake_entry_id: Uuid,
    pub owner: Owner,
    pub palette: Vec<String>,
    pub model_id: String,
    pub max_rounds: u32,
}

#[derive(Debug)]
struct Entry {
    ctx: WakeTokenContext,
    expires_at: Instant,
}

#[derive(Debug)]
pub struct WakeTokenStore {
    ttl: Duration,
    inner: Arc<RwLock<HashMap<Uuid, Entry>>>,
}

impl WakeTokenStore {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn mint(&self, ctx: WakeTokenContext) -> Uuid {
        let token = Uuid::new_v4();
        let entry = Entry {
            ctx,
            expires_at: Instant::now() + self.ttl,
        };
        self.inner.write().await.insert(token, entry);
        token
    }

    pub async fn resolve(&self, token: Uuid) -> Option<WakeTokenContext> {
        let guard = self.inner.read().await;
        let entry = guard.get(&token)?;
        if entry.expires_at <= Instant::now() {
            return None;
        }
        Some(entry.ctx.clone())
    }

    pub async fn revoke(&self, token: Uuid) {
        self.inner.write().await.remove(&token);
    }

    pub async fn sweep_expired(&self) -> usize {
        let now = Instant::now();
        let mut guard = self.inner.write().await;
        let before = guard.len();
        guard.retain(|_, e| e.expires_at > now);
        before - guard.len()
    }
}

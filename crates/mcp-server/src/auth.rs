use std::collections::HashMap;
use std::sync::Arc;

use proxima_core::Owner;
use proxima_core::wake::token_store::WakeTokenContext;
use proxima_core::wake::token_store::WakeTokenStore;
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct McpAuthContext {
    pub owner: Owner,
    pub scope: McpToolScope,
    pub model_id: Option<String>,
    pub wake: Option<WakeTokenContext>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpToolScope {
    All,
    Palette(Vec<String>),
}

impl McpToolScope {
    #[must_use]
    pub fn allows(&self, name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Palette(allowed) => allowed.iter().any(|tool| tool == name),
        }
    }
}

#[derive(Debug)]
pub struct McpAuthStore {
    wake_tokens: Arc<WakeTokenStore>,
    master_tokens: RwLock<HashMap<Uuid, Owner>>,
}

impl McpAuthStore {
    #[must_use]
    pub fn new(wake_tokens: Arc<WakeTokenStore>) -> Self {
        Self {
            wake_tokens,
            master_tokens: RwLock::new(HashMap::new()),
        }
    }

    pub async fn replace_local_master_token(&self, token: Uuid, owner: Owner) {
        let mut guard = self.master_tokens.write().await;
        guard.retain(|_, existing| existing != &owner);
        guard.insert(token, owner);
    }

    pub async fn resolve(&self, token: Uuid) -> Option<McpAuthContext> {
        if let Some(wake) = self.wake_tokens.resolve(token).await {
            return Some(McpAuthContext {
                owner: wake.owner.clone(),
                scope: McpToolScope::Palette(wake.palette.clone()),
                model_id: Some(wake.model_id.clone()),
                wake: Some(wake),
            });
        }

        let guard = self.master_tokens.read().await;
        guard.get(&token).cloned().map(|owner| McpAuthContext {
            owner,
            scope: McpToolScope::All,
            model_id: None,
            wake: None,
        })
    }
}

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
    pub master_token_id: Option<Uuid>,
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
                master_token_id: None,
            });
        }

        let guard = self.master_tokens.read().await;
        guard.get(&token).cloned().map(|owner| McpAuthContext {
            owner,
            scope: McpToolScope::All,
            model_id: None,
            wake: None,
            master_token_id: Some(token),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use proxima_core::{OrgId, Principal, UserId};

    fn fake_owner() -> Owner {
        Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        }
    }

    fn test_store() -> McpAuthStore {
        McpAuthStore::new(Arc::new(WakeTokenStore::new(Duration::from_secs(300))))
    }

    #[tokio::test]
    async fn resolve_master_token_carries_token_id() {
        let store = test_store();
        let owner = fake_owner();
        let token = Uuid::now_v7();
        store.replace_local_master_token(token, owner.clone()).await;

        let ctx = store.resolve(token).await.expect("resolved");
        assert_eq!(ctx.master_token_id, Some(token));
        assert!(ctx.wake.is_none());
        assert!(matches!(ctx.scope, McpToolScope::All));
    }

    #[tokio::test]
    async fn resolve_unknown_token_returns_none() {
        let store = test_store();
        let ctx = store.resolve(Uuid::now_v7()).await;
        assert!(ctx.is_none());
    }
}

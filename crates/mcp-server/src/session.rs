use std::collections::HashMap;
use std::sync::Arc;

use proxima_core::{Owner, parse_external_key};
use tokio::sync::RwLock;

#[derive(Clone, Default)]
pub struct McpSessionBindings {
    inner: Arc<RwLock<HashMap<String, Owner>>>,
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

    pub async fn bind(&self, session_id: impl Into<String>, owner: Owner) {
        self.inner.write().await.insert(session_id.into(), owner);
    }

    pub async fn owner_for(&self, session_id: &str) -> Option<Owner> {
        self.inner.read().await.get(session_id).copied()
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
}

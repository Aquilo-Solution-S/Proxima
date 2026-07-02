use std::collections::HashMap;
use std::sync::Arc;

use proxima_core::{GroupId, Owner, OwnerRef, UserId};
use tokio::sync::RwLock;
use uuid::Uuid;

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
    if raw == "world" {
        return Some(OwnerRef::World);
    }
    if let Some(id) = raw.strip_prefix("personal:") {
        return Uuid::parse_str(id)
            .ok()
            .map(|id| OwnerRef::Personal(UserId::new(id)));
    }
    raw.strip_prefix("group:").and_then(|id| {
        Uuid::parse_str(id)
            .ok()
            .map(|id| OwnerRef::Group(GroupId::new(id)))
    })
}

#[must_use]
pub fn owner_key(owner: Owner) -> String {
    match owner {
        OwnerRef::World => "world".to_string(),
        OwnerRef::Personal(user) => format!("personal:{}", user.into_inner()),
        OwnerRef::Group(group) => format!("group:{}", group.into_inner()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(parse_owner_key("personal:not-a-uuid"), None);
    }
}

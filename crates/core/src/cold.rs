//! Cold object store and persisted object-key derivation for forget/hydrate.

use uuid::Uuid;

use crate::StorageError;

/// Deterministic key for a persisted cold Memory record.
///
/// The key is exactly `cold/{t}`, and `t` is the Memory version id — unique
/// across the deployment, minted by the server, and immutable for the life
/// of the row. Stable across retries and hydrate → forget cycles, so
/// competing writers address one logical object rather than creating
/// attempt-specific objects.
///
/// **No owner component, and only one scheme.** An owner transfer moves
/// `cooled.owner_id` and nothing else: the key a transferred series was
/// written under stays correct, so the transfer performs no object-store
/// work at all. There is no second, owner-scoped derivation to fall back
/// to — keys written by an earlier scheme are not readable and are not
/// meant to be.
#[must_use]
pub fn cold_object_key(t: Uuid) -> String {
    format!("cold/{t}")
}

/// One object per Memory `t` under `cold/<t>`.
#[async_trait::async_trait]
pub trait ColdObjectStore: Send + Sync {
    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError>;
    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError>;
    async fn delete(&self, key: &str) -> Result<(), StorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OwnerRef, UserId};

    /// The cold key carries the Memory version id and nothing else. A
    /// transfer rewrites `cooled.owner_id`; if the key mentioned the owner
    /// the object would have to move with it, which is exactly the S3 work
    /// this scheme exists to avoid.
    #[test]
    fn cold_key_is_owner_free_and_golden() {
        let t = Uuid::parse_str("00000000-0000-0000-0000-000000000003").expect("uuid literal");
        let owner = OwnerRef::Personal(UserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid literal"),
        ));

        assert_eq!(
            cold_object_key(t),
            "cold/00000000-0000-0000-0000-000000000003"
        );
        assert!(!cold_object_key(t).contains(&owner.stable_key_uuid().to_string()));
    }
}

//! Cold object store and persisted object-key derivation for forget/hydrate
//! (UML §5c).

use uuid::Uuid;

use crate::{Owner, OwnerRefKind, StorageError};

const OWNER_S3_KEY_DOMAIN: &[u8] = b"proxima-owner-s3-key-v1\0";

/// Return the stable lowercase-hex owner component used by persisted object
/// keys.
///
/// The digest input is the exact byte sequence
/// `proxima-owner-s3-key-v1\0`, owner kind, `\0`, and the owner's stable UUID.
/// Keep this in core so every object-store implementation uses the same
/// persisted namespace.
#[must_use]
pub fn owner_hash_hex(owner: &Owner) -> String {
    let kind = OwnerRefKind::of(owner);
    let mut hasher = blake3::Hasher::new();
    hasher.update(OWNER_S3_KEY_DOMAIN);
    hasher.update(kind.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(owner.stable_key_uuid().as_bytes());
    hex::encode(hasher.finalize().as_bytes())
}

/// Prefix for one owner's cold Memory objects.
#[must_use]
pub fn cold_owner_prefix(owner_hash: &str) -> String {
    format!("cold/{owner_hash}/")
}

/// Deterministic key for a persisted cold Memory record.
///
/// The key is exactly `cold/{owner_hash}/{handle}/{t}`. It is stable across
/// retries and hydrate → forget cycles, so competing writers address one
/// logical object rather than creating attempt-specific objects.
#[must_use]
pub fn cold_object_key(owner_hash: &str, handle: Uuid, t: Uuid) -> String {
    format!("{}{handle}/{t}", cold_owner_prefix(owner_hash))
}

/// One object per Memory `t` under `cold/<owner_hash>/<handle>/<t>`.
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

    #[test]
    fn persisted_owner_hash_and_cold_key_are_golden() {
        let owner = OwnerRef::Personal(UserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid literal"),
        ));
        let owner_hash = owner_hash_hex(&owner);
        let handle = Uuid::parse_str("00000000-0000-0000-0000-000000000002").expect("uuid literal");
        let t = Uuid::parse_str("00000000-0000-0000-0000-000000000003").expect("uuid literal");

        assert_eq!(
            owner_hash,
            "c022815b2b51727207c5f3014833f1a5c09ae92edfb752c394c9caa3d96374ce"
        );
        assert_eq!(
            cold_object_key(&owner_hash, handle, t),
            "cold/c022815b2b51727207c5f3014833f1a5c09ae92edfb752c394c9caa3d96374ce/00000000-0000-0000-0000-000000000002/00000000-0000-0000-0000-000000000003"
        );
        assert_eq!(
            cold_owner_prefix(&owner_hash),
            "cold/c022815b2b51727207c5f3014833f1a5c09ae92edfb752c394c9caa3d96374ce/"
        );
    }
}

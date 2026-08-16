//! Where an owner's bytes live in S3, and how an `Owner` reaches its rows.
//!
//! Every key this crate writes and every prefix the erase purge scans is
//! derived here, so the write path and the purge cannot drift apart.

use proxima_core::{Owner, OwnerRefKind, UPLOADED_BLOB_SCHEMA_ID};
use uuid::Uuid;

pub(super) fn owner_hash_hex(owner: &Owner) -> String {
    let kind = OwnerRefKind::of(owner);
    let owner_key_id = owner.stable_key_uuid();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proxima-owner-s3-key-v1\0");
    hasher.update(kind.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(owner_key_id.as_bytes());
    hex::encode(hasher.finalize().as_bytes())
}

/// Prefix under which an owner's canonical (completed) blobs live. Single
/// source of truth for the `objects/<owner_hash>/` key space so the erase
/// purge and the write path can never drift.
pub(super) fn objects_owner_prefix(owner_hash: &str) -> String {
    format!("objects/{owner_hash}/")
}

/// Prefix under which an owner's in-flight (pending) uploads live.
pub(super) fn pending_owner_prefix(owner_hash: &str) -> String {
    format!("pending/{owner_hash}/")
}

pub(super) fn pending_object_key(owner_hash: &str, upload_id: Uuid) -> String {
    format!("{}{upload_id}", pending_owner_prefix(owner_hash))
}

pub(super) fn canonical_object_key(owner_hash: &str, blake3_hex: &str) -> String {
    format!(
        "{}{UPLOADED_BLOB_SCHEMA_ID}/{blake3_hex}",
        objects_owner_prefix(owner_hash)
    )
}

/// Forget/hydrate/erase: one object per Memory `t`.
#[must_use]
pub fn cold_owner_prefix(owner_hash: &str) -> String {
    format!("cold/{owner_hash}/")
}

#[must_use]
pub fn cold_object_key(owner_hash: &str, handle: Uuid, t: Uuid) -> String {
    format!("{}{handle}/{t}", cold_owner_prefix(owner_hash))
}

#[must_use]
pub fn owner_hash_hex_public(owner: &Owner) -> String {
    owner_hash_hex(owner)
}

#[cfg(test)]
mod tests {
    use proxima_core::{OwnerRef, UserId};

    use super::*;

    #[test]
    fn object_keys_do_not_embed_raw_owner_ids() {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let owner_kind = OwnerRefKind::of(&owner);
        let owner_key_id = owner.stable_key_uuid();
        let owner_hash = owner_hash_hex(&owner);
        let pending = pending_object_key(&owner_hash, Uuid::now_v7());
        let canonical = canonical_object_key(&owner_hash, &"a".repeat(64));

        assert_eq!(owner_hash.len(), 64);
        assert!(!pending.contains(owner_kind.as_str()));
        assert!(!pending.contains(&owner_key_id.to_string()));
        assert!(pending.starts_with("pending/"));
        assert!(canonical.contains(UPLOADED_BLOB_SCHEMA_ID));
        assert!(canonical.starts_with("objects/"));
    }

    /// The erase purge must target exactly the two owner-scoped prefixes that
    /// prepare/complete write under, derived from the same helpers (no
    /// hardcoded key format). The S3 round-trip itself is only exercised under
    /// `PROXIMA_S3_*` (see `blob_roundtrip_pg`); this pins the deterministic,
    /// network-free key/prefix derivation the purge relies on.
    #[test]
    fn purge_prefixes_are_owner_scoped_ancestors_of_written_keys() {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let owner_hash = owner_hash_hex(&owner);
        let objects = objects_owner_prefix(&owner_hash);
        let pending = pending_owner_prefix(&owner_hash);

        assert_eq!(objects, format!("objects/{owner_hash}/"));
        assert_eq!(pending, format!("pending/{owner_hash}/"));

        // Every key the write path emits sits under the prefix the purge scans.
        assert!(canonical_object_key(&owner_hash, &"a".repeat(64)).starts_with(&objects));
        assert!(pending_object_key(&owner_hash, Uuid::now_v7()).starts_with(&pending));
        let cold = cold_object_key(&owner_hash, Uuid::now_v7(), Uuid::now_v7());
        assert!(cold.starts_with(&cold_owner_prefix(&owner_hash)));
        assert!(!cold.contains(&owner.stable_key_uuid().to_string()));

        // A different owner yields disjoint prefixes, so a purge never reaches
        // another owner's objects.
        let other_hash = owner_hash_hex(&OwnerRef::Personal(UserId::new(Uuid::now_v7())));
        assert_ne!(objects, objects_owner_prefix(&other_hash));
        assert_ne!(pending, pending_owner_prefix(&other_hash));
    }

    #[test]
    fn owner_hash_is_owner_scoped() {
        let a = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let b = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        assert_ne!(owner_hash_hex(&a), owner_hash_hex(&b));
    }

    /// Pins the org-free S3 `owner_hash_hex` against drift. The BLAKE3 folds
    /// the domain tag ‖ principal kind/id — no org. A
    /// fixed principal must reproduce exactly this hex (and thus the same
    /// stored S3 object path) forever.
    #[test]
    fn owner_hash_hex_golden_is_org_free() {
        let owner = OwnerRef::Personal(UserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid literal"),
        ));
        assert_eq!(
            owner_hash_hex(&owner),
            "c022815b2b51727207c5f3014833f1a5c09ae92edfb752c394c9caa3d96374ce"
        );
    }
}

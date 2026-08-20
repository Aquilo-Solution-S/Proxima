//! Where an owner's cited bytes live in S3.
//!
//! **Keys carry no owner, and there is exactly one scheme.** A key is
//! minted once from the server-minted `blob_uploads.upload_id` that names
//! the row it belongs to, and never changes again — an owner transfer is an
//! `owner_id` update on the row and nothing else.
//!
//! That is also what makes the read gate safe without a prefix: a locator
//! is honoured only when it is byte-for-byte the key this store would mint
//! for THAT row's own id. `core/uploaded-blob-v1` is a registered
//! cited-object schema, so a locator row can be persisted under the
//! caller's own owner — owning the row is therefore not evidence of
//! anything. A forged row can only ever vouch for its own object, because
//! the key is derived from its primary key.
//!
//! There is no legacy branch. Keys written by the retired owner-scoped
//! scheme are not readable and are not meant to be; v0.0.8 is a breaking
//! release and existing deployments re-ingest.

use uuid::Uuid;

/// Bucket-wide prefix for completed cited blobs.
pub(super) const CANONICAL_OBJECT_PREFIX: &str = "objects/";

/// In-flight bytes for one upload. The upload id already names exactly one
/// `blob_uploads` row.
pub(super) fn pending_object_key(upload_id: Uuid) -> String {
    format!("pending/{upload_id}")
}

/// The committed object for one upload, derived from the row's own primary
/// key — which is what the read gate compares against.
pub(super) fn canonical_object_key(upload_id: Uuid) -> String {
    format!("{CANONICAL_OBJECT_PREFIX}{upload_id}")
}

/// Is `object_key` the locator this store minted for the upload row
/// `upload_id`?
///
/// ONE rule, shared by every surface that trusts a stored locator
/// (`read_url`, the verified read, and reconcile's foreign-locator count),
/// because they must agree on what "ours" means. `owner` is not consulted:
/// the row's own id decides, and the owner predicates in the SQL decide
/// who may reach the row at all.
pub(super) fn locator_was_minted_here(object_key: &str, upload_id: Uuid) -> bool {
    object_key == canonical_object_key(upload_id)
}

/// Forget/hydrate/erase: one object per Memory `t`; the derivation is owned by
/// `proxima_core` so storage-pg and blob-s3 cannot drift apart.
pub use proxima_core::cold_object_key;

#[cfg(test)]
mod tests {
    use proxima_core::{OwnerRef, OwnerRefKind, UserId};

    use super::*;

    /// The whole point of the scheme: nothing an owner is identified by
    /// appears in a key.
    #[test]
    fn minted_keys_carry_no_owner_component_at_all() {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let owner_kind = OwnerRefKind::of(&owner);
        let owner_key_id = owner.stable_key_uuid().to_string();
        let upload_id = Uuid::now_v7();
        let t = Uuid::now_v7();

        for key in [
            pending_object_key(upload_id),
            canonical_object_key(upload_id),
            cold_object_key(t),
        ] {
            assert!(!key.contains(&owner_key_id), "{key} leaks the owner id");
            assert!(
                !key.contains(owner_kind.as_str()),
                "{key} leaks the owner kind"
            );
        }

        assert_eq!(
            pending_object_key(upload_id),
            format!("pending/{upload_id}")
        );
        assert_eq!(
            canonical_object_key(upload_id),
            format!("objects/{upload_id}")
        );
        assert_eq!(CANONICAL_OBJECT_PREFIX, "objects/");
    }

    /// The read gate compares a stored locator against the key derived from
    /// the row's OWN upload id. Two rows therefore never share a key, so a
    /// forged row cannot name another row's object and pass.
    #[test]
    fn one_key_per_upload_row_is_what_makes_the_gate_unforgeable() {
        let mine = Uuid::now_v7();
        let yours = Uuid::now_v7();

        assert!(locator_was_minted_here(&canonical_object_key(mine), mine));
        assert!(!locator_was_minted_here(&canonical_object_key(yours), mine));
        assert!(!locator_was_minted_here(&pending_object_key(mine), mine));
        assert_ne!(canonical_object_key(mine), canonical_object_key(yours));
        // Deriving twice from the same id is stable — the key never moves.
        assert_eq!(canonical_object_key(mine), canonical_object_key(mine));
    }

    #[test]
    fn persisted_cold_keys_match_storage_pg_exactly() {
        let t = Uuid::parse_str("00000000-0000-0000-0000-000000000003").expect("uuid literal");

        assert_eq!(
            cold_object_key(t),
            proxima_storage_pg::verbs::forget::cold_object_key(t)
        );
        assert_eq!(
            cold_object_key(t),
            "cold/00000000-0000-0000-0000-000000000003"
        );
    }
}

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

/// Which upload row's id minted the object a row names.
///
/// A row that uploaded its own bytes minted its own object. A row created
/// by a cross-owner transfer mounts the object the source already has, and
/// carries the minting id explicitly — always the id of the row that did
/// the upload, never of the row it was mounted from, so a chain of mounts
/// still resolves to the one object that exists.
pub(super) fn minting_upload_id(upload_id: Uuid, mounted_from_upload_id: Option<Uuid>) -> Uuid {
    mounted_from_upload_id.unwrap_or(upload_id)
}

/// Is `object_key` the locator this store minted for this upload row?
///
/// ONE rule, shared by every surface that trusts a stored locator
/// (`read_url`, the verified read, and reconcile's foreign-locator count),
/// because they must agree on what "ours" means. `owner` is not consulted:
/// the row's own columns decide, and the owner predicates in the SQL decide
/// who may reach the row at all.
///
/// The mount column is a parameter rather than a lookup so that no caller
/// can reach this function without having read it — a caller that fetched
/// only `upload_id` would silently reject every mounted row, and a
/// verified-read path that rejects a legitimate row is a data-loss bug
/// wearing a security bug's clothes. It stays a derivation from the row's
/// own columns, which is the property the exact-equality test protects:
/// nothing here is a stored locator taken on trust.
pub(super) fn locator_was_minted_here(
    object_key: &str,
    upload_id: Uuid,
    mounted_from_upload_id: Option<Uuid>,
) -> bool {
    object_key == canonical_object_key(minting_upload_id(upload_id, mounted_from_upload_id))
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
    /// the row's OWN columns. An unmounted row can therefore only name its
    /// own object, so a forged row cannot name another row's object and
    /// pass.
    #[test]
    fn one_key_per_upload_row_is_what_makes_the_gate_unforgeable() {
        let mine = Uuid::now_v7();
        let yours = Uuid::now_v7();

        assert!(locator_was_minted_here(
            &canonical_object_key(mine),
            mine,
            None
        ));
        assert!(!locator_was_minted_here(
            &canonical_object_key(yours),
            mine,
            None
        ));
        assert!(!locator_was_minted_here(
            &pending_object_key(mine),
            mine,
            None
        ));
        assert_ne!(canonical_object_key(mine), canonical_object_key(yours));
        // Deriving twice from the same id is stable — the key never moves.
        assert_eq!(canonical_object_key(mine), canonical_object_key(mine));
    }

    /// A mount moves WHICH id the key derives from, and nothing else.
    ///
    /// The destination row of a cross-owner transfer has its own
    /// `upload_id` and names the source's object. Before the mount column
    /// existed there was no way to express that without either copying the
    /// bytes or storing a locator on trust; the column is the third way,
    /// and it stays a derivation.
    #[test]
    fn a_mounted_row_names_the_object_its_source_minted_and_nothing_else() {
        let source = Uuid::now_v7();
        let mounted = Uuid::now_v7();
        let stranger = Uuid::now_v7();
        let source_key = canonical_object_key(source);

        assert!(
            locator_was_minted_here(&source_key, mounted, Some(source)),
            "a mounted row must reach the object its source minted"
        );
        assert!(
            !locator_was_minted_here(&canonical_object_key(mounted), mounted, Some(source)),
            "a mounted row must NOT reach a key minted for its own id: no such object exists, \
             and honouring it would let a mount invent an object"
        );
        assert!(
            !locator_was_minted_here(&canonical_object_key(stranger), mounted, Some(source)),
            "the mount reaches exactly one object, not the whole prefix"
        );
        // The mount is what changes the answer. Same row, same key, no
        // mount column: refused.
        assert!(
            !locator_was_minted_here(&source_key, mounted, None),
            "without the mount the row is back to naming only its own object"
        );
    }

    /// MUTANT PIN. A gate weakened in any of the plausible ways must fail
    /// at least one case above.
    ///
    /// The cases and the mutants both live here because a mutant that no
    /// case kills is the finding: it means the assertions above constrain
    /// less than they appear to. `minted` is the real rule, expressed once;
    /// every mutant is a rule someone could reach for while "simplifying"
    /// it.
    #[test]
    fn every_weakening_of_the_gate_is_killed_by_a_case() {
        let source = Uuid::now_v7();
        let mounted = Uuid::now_v7();
        let key = canonical_object_key(source);

        // (locator, upload_id, mounted_from, expected)
        let cases: &[(String, Uuid, Option<Uuid>, bool)] = &[
            (canonical_object_key(mounted), mounted, None, true),
            (key.clone(), mounted, Some(source), true),
            (key.clone(), mounted, None, false),
            (canonical_object_key(mounted), mounted, Some(source), false),
            (format!("{key}x"), source, None, false),
            (pending_object_key(source), source, None, false),
            (String::new(), source, None, false),
        ];

        #[allow(clippy::type_complexity)]
        let mutants: &[(&str, fn(&str, Uuid, Option<Uuid>) -> bool)] = &[
            // Ignore the mount column: the pre-dedupe gate, which refuses
            // every mounted row and would silently break a transferred
            // citation.
            ("ignores the mount", |key, upload_id, _| {
                key == canonical_object_key(upload_id)
            }),
            // Trust the mount column when it is present, without deriving.
            (
                "honours any key on a mounted row",
                |key, upload_id, mount| mount.is_some() || key == canonical_object_key(upload_id),
            ),
            // Prefix instead of equality.
            ("prefix match", |key, upload_id, mount| {
                key.starts_with(&canonical_object_key(mount.unwrap_or(upload_id)))
            }),
            // Accept anything under the canonical prefix.
            ("prefix only", |key, _, _| {
                key.starts_with(CANONICAL_OBJECT_PREFIX)
            }),
            // Accept either id, rather than the derived one.
            ("either id", |key, upload_id, mount| {
                key == canonical_object_key(upload_id)
                    || mount.is_some_and(|m| key == canonical_object_key(m))
            }),
            // Always yes.
            ("open gate", |_, _, _| true),
        ];

        for (name, mutant) in mutants {
            let killed = cases.iter().any(|(key, upload_id, mount, expected)| {
                mutant(key, *upload_id, *mount) != *expected
            });
            assert!(
                killed,
                "the mutant that {name} passes every case above; the cases do not \
                 constrain the gate as tightly as they look"
            );
        }

        // The real rule passes all of them. Without this the assertions
        // above are satisfied by a gate nobody could ship.
        for (key, upload_id, mount, expected) in cases {
            assert_eq!(
                locator_was_minted_here(key, *upload_id, *mount),
                *expected,
                "{key:?} under upload {upload_id} mounted from {mount:?}"
            );
        }
    }

    /// Exact equality, and nothing weaker.
    ///
    /// Every candidate below carries the row's OWN `upload_id`, so nothing
    /// else in the system can save the gate here: not the owner predicate
    /// in the SQL (the row is the caller's), not the bucket check (same
    /// bucket), not the prefix (all under `objects/`). The only thing
    /// separating an honoured locator from a forged one is that the bytes
    /// match exactly.
    ///
    /// The first four are the reason this is `==` and not `starts_with`.
    /// A prefix test honours every one of them, and each names a DIFFERENT
    /// S3 object than the row does — so weakening the comparison hands out
    /// a presigned GET for bytes the row never claimed. The rest pin the
    /// other plausible slips: trimming, case folding, and any test that
    /// looks at only part of the key.
    #[test]
    fn only_the_exact_minted_key_is_honoured_for_a_row() {
        let upload_id = Uuid::now_v7();
        let exact = canonical_object_key(upload_id);

        // Control. Without it every assertion below is satisfied by a gate
        // that refuses unconditionally.
        assert!(
            locator_was_minted_here(&exact, upload_id, None),
            "the key this store mints for the row must be honoured"
        );

        for near_miss in [
            // Extensions of the exact key — a prefix test accepts all four.
            format!("{exact}/child"),
            format!("{exact}x"),
            format!("{exact} "),
            format!("{exact}\n"),
            // Decorations and truncations of it.
            format!(" {exact}"),
            format!("/{exact}"),
            format!("{CANONICAL_OBJECT_PREFIX}/{upload_id}"),
            exact.replace(CANONICAL_OBJECT_PREFIX, "Objects/"),
            exact.to_uppercase(),
            format!(
                "{CANONICAL_OBJECT_PREFIX}{}",
                upload_id.to_string().to_uppercase()
            ),
            // Real keys of other shapes that name the same id.
            pending_object_key(upload_id),
            cold_object_key(upload_id),
            upload_id.to_string(),
            String::new(),
        ] {
            assert!(
                !locator_was_minted_here(&near_miss, upload_id, None),
                "{near_miss:?} is not the key minted for this row and must be refused"
            );
        }
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

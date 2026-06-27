//! Entry-level access vocabulary: the persisted grant primitive that collapses
//! the old two-axis RBAC (`RoleSet` + per-token `MemorySpaceGrants`) into one
//! cross-principal access relation `(resource, relation, subject)`.
//!
//! The [`Relation`] lattice is **partial**: `owner ⊒ editor ⊒ viewer`, while
//! `admin`, `ingest`, and `member` are each incomparable with the read/write
//! chain and with each other. The decision engine ([`crate::engine`]) resolves a
//! caller's relations on a space or entry through the storage grant repository
//! and compares them with [`Relation::dominates`].
//!
//! See `docs/superpowers/specs/2026-06-27-entry-access-model-design.md`.

use crate::{MemoryId, Owner, PersonalityInstanceId, Principal};

/// The relation a grant confers, forming a partial domination lattice.
///
/// `owner` dominates the read/write chain (`editor`, `viewer`) but **not**
/// `admin`/`ingest`/`member` — least privilege is enforceable because the
/// chain is partial, not a single total order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(type_name = "proxima_core.grant_relation", rename_all = "lowercase")]
pub enum Relation {
    /// Everything on the space + grant-management + transfer. Not set by the
    /// ordinary grant verbs (identity-or-`init_space_owner` only).
    Owner,
    /// Space/personality **config** only (wake entries, personality config,
    /// read-scope) — NOT memory read/write, NOT grant-management.
    Admin,
    /// Read + search + write + publish. Dominates [`Relation::Viewer`].
    Editor,
    /// Read + search.
    Viewer,
    /// Append source Facts only — not read/write/publish.
    Ingest,
    /// Belongs to a `Group` space (space-only). Confers nothing alone; the
    /// group's own space bindings decide what its members may do.
    Member,
}

impl Relation {
    /// True iff holding `self` satisfies a requirement for `required`.
    ///
    /// Partial lattice: only `owner ⊒ editor ⊒ viewer` composes; every other
    /// relation is reflexive-only (incomparable with the rest).
    #[must_use]
    pub const fn dominates(self, required: Self) -> bool {
        use Relation::{Admin, Editor, Ingest, Member, Owner, Viewer};
        matches!(
            (self, required),
            // owner dominates the read/write chain (NOT admin/ingest/member);
            // editor dominates viewer; everything else is reflexive-only.
            (Owner, Owner | Editor | Viewer)
                | (Editor, Editor | Viewer)
                | (Admin, Admin)
                | (Viewer, Viewer)
                | (Ingest, Ingest)
                | (Member, Member)
        )
    }

    /// Stable denial message used in `Forbidden` errors.
    #[must_use]
    pub const fn denied_message(self) -> &'static str {
        match self {
            Self::Owner => "requires owner on this space",
            Self::Admin => "requires admin on this space",
            Self::Editor => "requires editor on this space",
            Self::Viewer => "requires viewer on this space",
            Self::Ingest => "requires ingest on this space",
            Self::Member => "requires membership of this space",
        }
    }

    /// Whether the ordinary grant verbs may set this relation. `owner` is
    /// reserved for `init_space_owner`/`add_owner` (DB-enforced too).
    #[must_use]
    pub const fn is_grantable(self) -> bool {
        !matches!(self, Self::Owner)
    }
}

/// The sole surviving per-token memory capability after the `RoleSet`/grant
/// collapse: whether the caller bypasses persisted grants entirely, or is
/// decided purely by them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessScope {
    /// Acts as `owner` on every principal in the identity's
    /// `accessible_principals` (master/dev/embedded/single-owner tokens).
    /// Short-circuits resolution to ALLOW, still bounded by owner visibility.
    Unrestricted,
    /// Access decided solely by persisted `access_grants` (space bindings +
    /// entry grants + owner rows).
    Granted,
}

/// Denormalized read fast-path flag on a memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "proxima_core.memory_visibility", rename_all = "lowercase")]
pub enum Visibility {
    /// Only space members may access.
    Private,
    /// Cache of "≥1 active entry-level grant exists" — not an allow source.
    Shared,
    /// World-readable marketplace entry — a read source-of-truth.
    Public,
}

/// Which resource a grant targets. `Space` rows carry no `resource_id`;
/// `Memory` rows carry the memory id and are existence/owner/liveness checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantResource {
    Space,
    Memory(MemoryId),
}

/// One active grant row resolved for an access decision or a "who can access"
/// listing. `subject_is_group` distinguishes a collective `group` subject
/// (members inherit) from a `principal` subject (the exact principal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessGrantRow {
    pub relation: Relation,
    pub subject: Principal,
    pub subject_is_group: bool,
}

/// The owner-space + visibility of a live entry, resolved inside the storage
/// boundary (never from client input). Absent/tombstoned entries resolve to
/// `None`, so a tombstoned public entry is never served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryAccessFacts {
    pub owner: Owner,
    pub visibility: Visibility,
}

/// A grant to insert. For a `Memory` resource, `space_owner` must equal the
/// target memory's owner (DB existence trigger enforces it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAccessGrant {
    pub space_owner: Owner,
    pub resource: GrantResource,
    pub relation: Relation,
    pub subject: Principal,
    pub subject_is_group: bool,
    pub granted_by: PersonalityInstanceId,
}

/// Selects active grants to revoke. `relation = None` revokes every relation
/// the subject holds on the resource (used by `unshare_entry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantSelector {
    pub space_owner: Owner,
    pub resource: GrantResource,
    pub relation: Option<Relation>,
    pub subject: Principal,
    pub subject_is_group: bool,
}

/// Result of `remove_space_owner` — the last-owner orphan guard makes this a
/// three-way outcome rather than a row count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveOwnerOutcome {
    Removed,
    /// Refused: removing this owner would leave the Group space ownerless.
    RefusedLastOwner,
    NotFound,
}

#[cfg(test)]
mod tests {
    use super::Relation::{Admin, Editor, Ingest, Member, Owner, Viewer};
    use super::*;

    const ALL: [Relation; 6] = [Owner, Admin, Editor, Viewer, Ingest, Member];

    #[test]
    fn dominates_is_reflexive() {
        for r in ALL {
            assert!(r.dominates(r), "{r:?} must dominate itself");
        }
    }

    #[test]
    fn owner_dominates_only_the_readwrite_chain() {
        // owner ⊒ editor ⊒ viewer
        assert!(Owner.dominates(Editor));
        assert!(Owner.dominates(Viewer));
        // owner is NOT a superset of the incomparable relations.
        assert!(!Owner.dominates(Admin));
        assert!(!Owner.dominates(Ingest));
        assert!(!Owner.dominates(Member));
    }

    #[test]
    fn editor_dominates_viewer_only() {
        assert!(Editor.dominates(Viewer));
        assert!(!Editor.dominates(Owner));
        assert!(!Editor.dominates(Admin));
        assert!(!Editor.dominates(Ingest));
        assert!(!Editor.dominates(Member));
    }

    #[test]
    fn viewer_dominates_nothing_above_itself() {
        for r in ALL {
            assert_eq!(
                Viewer.dominates(r),
                r == Viewer,
                "viewer must dominate only viewer, not {r:?}"
            );
        }
    }

    #[test]
    fn admin_ingest_member_are_mutually_incomparable_and_incomparable_with_the_chain() {
        for &r in &[Admin, Ingest, Member] {
            for other in ALL {
                // Each of admin/ingest/member dominates ONLY itself.
                assert_eq!(
                    r.dominates(other),
                    r == other,
                    "{r:?} must dominate only itself, not {other:?}"
                );
                // ...and nothing dominates them except themselves.
                assert_eq!(
                    other.dominates(r),
                    r == other,
                    "{other:?} must not dominate {r:?} unless equal"
                );
            }
        }
    }

    #[test]
    fn only_owner_is_ungrantable() {
        assert!(!Owner.is_grantable());
        for &r in &[Admin, Editor, Viewer, Ingest, Member] {
            assert!(r.is_grantable(), "{r:?} must be grantable");
        }
    }
}

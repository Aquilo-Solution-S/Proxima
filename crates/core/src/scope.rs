//! Flavor-owned lifecycle scopes — a scope the substrate can see because
//! the flavor DECLARED it.
//!
//! A repository, a book, a project, a revision: a flavor owns a registry
//! table of them, files rows under a bare `<scope>_id uuid` on its sidecars,
//! and erases one scope at a time. The substrate cannot infer any of that.
//! Every repo-scoped sidecar carries no foreign key into its registry, so
//! the row lock an erase takes on the registry row constrains nothing, and
//! the owner and source fences are one lane for every scope of that owner —
//! neither separates an erase of scope A from a concurrent write into A.
//! That gap is what lets a write commit a fresh admission after an erase has
//! computed and swept its footprint: a successful erase, and rows left
//! behind for a scope that no longer exists.
//!
//! The closure has two halves, and both are declarations:
//!
//! 1. A payload type says which scope its rows belong to
//!    ([`FactPayload::SCOPE_KIND`](crate::FactPayload::SCOPE_KIND) plus
//!    `scope_id`, read back as [`ScopeRef`]).
//! 2. A flavor contract says, once per [`ScopeKind`], where that scope's
//!    registry lives ([`ScopeDecl`]).
//!
//! From the pair, storage GENERATES both the fence key and the liveness
//! probe — it spells no table and no column of its own. Freeze refuses a
//! kind a payload uses and no contract declares, and a kind two contracts
//! declare, so neither half can go missing without failing the build.
//!
//! There is no runtime registration path, and never a generic scope erase:
//! what "one scope's rows" means is the flavor's knowledge, and the
//! substrate's job is only to make sure no admission of a scoped payload
//! slips past the fence that erase holds.

use uuid::Uuid;

/// The name of a flavor-owned lifecycle scope, e.g. `code-repo`.
///
/// `&'static str` rather than an enum: the closed vocabulary lives in the
/// linked flavors, not in core, and a core enum would have to be edited to
/// add a scope to an out-of-tree flavor. The string is namespaced by
/// convention (`<flavor>-<scope>`) and hashed into the fence key, so two
/// flavors that pick the same name share a lane — which freeze refuses as a
/// duplicate declaration rather than letting it become a silent collision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeKind(&'static str);

impl ScopeKind {
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for ScopeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// One scope a payload belongs to: which kind, and which row of it.
///
/// Read off a payload value, because the id is a payload field and the kind
/// is a property of the payload TYPE. A payload whose scope column is
/// nullable (the code flavor's cross-repository development perspective)
/// returns `None` for the rows that name no scope, and those rows are
/// unscoped in fact as well as in declaration.
///
/// Ordered so a batch's distinct scopes can be taken as one sorted,
/// deduplicated set: two writers whose batches overlap must not take the
/// same two keys in opposite orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeRef {
    pub kind: ScopeKind,
    pub id: Uuid,
}

impl ScopeRef {
    #[must_use]
    pub const fn new(kind: ScopeKind, id: Uuid) -> Self {
        Self { kind, id }
    }
}

impl std::fmt::Display for ScopeRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.kind, self.id)
    }
}

/// Where one [`ScopeKind`]'s registry lives, declared once by the flavor
/// that owns it.
///
/// Every name storage needs is here, and storage spells none of its own:
/// the liveness probe is generated as
/// `SELECT EXISTS(SELECT 1 FROM <registry_table>
///                 WHERE <owner_kind_column> = $1
///                   AND <owner_id_column> = $2
///                   AND <id_column> = $3)`,
/// which is why a renamed column is a declaration edit and not a second
/// place to keep in sync.
///
/// The owner is TWO columns because an [`crate::Owner`] is a kind and an
/// id, and both are in the key of every flavor registry in the tree. A
/// scope id that somehow appeared under two owners must not serialize them
/// — the same reason the owner portion is repeated in the source fence key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeDecl {
    pub kind: ScopeKind,
    /// Schema-qualified registry table, e.g. `proxima_code.repos`.
    pub registry_table: &'static str,
    /// Column holding the scope id.
    pub id_column: &'static str,
    /// Column holding the owner kind discriminant.
    pub owner_kind_column: &'static str,
    /// Column holding the owner id.
    pub owner_id_column: &'static str,
}

impl ScopeDecl {
    /// Names a declaration must satisfy before storage will splice it.
    ///
    /// Core cannot reach `PgIdent` (that is a backend concern), so it checks
    /// the shape every backend needs: a qualified table and three non-empty
    /// column names. The backend validates the identifiers again at the
    /// splice; this is the freeze-time refusal that names the DECLARATION
    /// rather than failing at the first write.
    #[must_use]
    pub fn shape_error(&self) -> Option<&'static str> {
        if self.kind.as_str().is_empty() {
            return Some("scope kind must not be empty");
        }
        if !self.registry_table.contains('.') {
            return Some("registry table must be schema-qualified");
        }
        for column in [self.id_column, self.owner_kind_column, self.owner_id_column] {
            if column.is_empty() {
                return Some("scope registry column names must not be empty");
            }
        }
        None
    }
}

/// The sorted, deduplicated scope set of a batch.
///
/// A batch may span several scopes; taking their keys in one canonical
/// order is what keeps two crossed batches from forming a fence cycle. The
/// same rule the owner fence follows for its multi-owner arm.
#[must_use]
pub fn scope_set(scopes: impl IntoIterator<Item = ScopeRef>) -> Vec<ScopeRef> {
    let mut set: Vec<ScopeRef> = scopes.into_iter().collect();
    set.sort_unstable();
    set.dedup();
    set
}

#[cfg(test)]
mod tests {
    use super::{ScopeDecl, ScopeKind, ScopeRef, scope_set};
    use uuid::Uuid;

    const KIND: ScopeKind = ScopeKind::new("test-scope");

    #[test]
    fn a_scope_set_is_sorted_and_deduplicated() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let set = scope_set([
            ScopeRef::new(KIND, b),
            ScopeRef::new(KIND, a),
            ScopeRef::new(KIND, b),
        ]);
        assert_eq!(set, vec![ScopeRef::new(KIND, a), ScopeRef::new(KIND, b)]);
    }

    #[test]
    fn a_declaration_must_qualify_its_registry_table() {
        let decl = ScopeDecl {
            kind: KIND,
            registry_table: "repos",
            id_column: "repo_id",
            owner_kind_column: "owner_kind",
            owner_id_column: "owner_id",
        };
        assert_eq!(
            decl.shape_error(),
            Some("registry table must be schema-qualified")
        );
        assert!(
            ScopeDecl {
                registry_table: "proxima_code.repos",
                ..decl
            }
            .shape_error()
            .is_none()
        );
    }

    #[test]
    fn a_declaration_must_name_every_column() {
        let decl = ScopeDecl {
            kind: KIND,
            registry_table: "proxima_code.repos",
            id_column: "",
            owner_kind_column: "owner_kind",
            owner_id_column: "owner_id",
        };
        assert_eq!(
            decl.shape_error(),
            Some("scope registry column names must not be empty")
        );
    }
}

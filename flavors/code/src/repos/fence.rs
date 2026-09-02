//! The repository lifecycle scope — its declaration, and the one question
//! a flavor-owned transaction still asks for itself.
//!
//! A repository is a lifecycle the substrate cannot infer. Every repo-scoped
//! sidecar carries a bare `repo_id uuid` with no foreign key into
//! `proxima_code.repos`, so the `FOR UPDATE` an erase takes on the repo row
//! constrains only what references that row — `repo_ingestion_runs` — and
//! nothing else. Two ingests of the same repository share the owner fence
//! and the one constant source fence (`proxima-code/local-git`) with every
//! other repository, so neither fence separates repository A's erase from
//! repository B's write, and neither serializes A's erase against A's own
//! write. That gap is what let an ingest commit a fresh admission after the
//! erase had computed and swept its footprint: a successful erase, and rows
//! left behind for a repository that no longer exists.
//!
//! [`CODE_REPO_SCOPE_DECL`] closes it by DECLARING the lifecycle rather than
//! by fencing each write. The declaration names the registry table, its id
//! column and its owner columns; the substrate generates both the fence key
//! (`proxima-scope-fence:code-repo:…`) and the liveness probe from those
//! names, and takes them on EVERY admission of a payload whose
//! `SCOPE_KIND` is [`CODE_REPO_SCOPE`] — whether the caller is this flavor's
//! ingest, one of its MCP tools, or a host writing `commit_summary_v1`
//! straight through [`proxima_core::Engine`]. There is no flavor-side
//! admission fence left to forget, and no way for a caller to opt out of
//! one.
//!
//! # Lock order
//!
//! ```text
//! owner fence -> source fence -> SCOPE FENCE -> handle / lifecycle `t` -> rows
//! ```
//!
//! [`super::erase::erase_repo`] takes the scope fence EXCLUSIVELY, with
//! `proxima::flavor::lock_scope_fence_exclusive_tx`, before it reads
//! anything — fence-before-select, so its footprint is exact by construction
//! rather than by re-checking afterwards. Different repositories take
//! different keys and never wait on each other.
//!
//! # What is still flavor-side
//!
//! [`super::runs`] writes `repo_ingestion_runs`, which is NOT a Memory
//! admission: the Engine never sees that row, so it cannot fence it. That
//! lane takes the same fence shared with
//! `proxima::flavor::lock_scope_fence_shared_tx` and asks
//! [`repo_registered_tx`] under it — the flavor-owned half of the same
//! guarantee, spelled once, here.

use proxima_core::{Owner, ScopeDecl, ScopeKind};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::records::RepoRegistryError;

/// The scope kind every repo-scoped payload names.
///
/// The string is the fence namespace's second segment and the freeze key
/// that binds a payload's `SCOPE_KIND` to [`CODE_REPO_SCOPE_DECL`]: a
/// payload naming a kind no linked flavor declares refuses the freeze, so
/// this constant is spelled once and referenced, never repeated as a
/// literal.
pub const CODE_REPO_SCOPE: ScopeKind = ScopeKind::new("code-repo");

/// The declaration itself, carried in
/// [`crate::contract::CODE_FLAVOR_CONTRACT`].
///
/// Every name here is a name storage will spell — the repo rule: the
/// substrate generates its probe FROM the declaration rather than from a
/// second copy of these strings living in `crates/storage-pg`. The owner
/// columns are the ones `super::erase`'s `REPO_EXISTS_SQL` filters on.
pub const CODE_REPO_SCOPE_DECL: ScopeDecl = ScopeDecl {
    kind: CODE_REPO_SCOPE,
    registry_table: "proxima_code.repos",
    id_column: "repo_id",
    owner_kind_column: "owner_kind",
    owner_id_column: "owner_id",
};

/// Is this repository still registered for this owner?
///
/// Deliberately not `FOR SHARE`: the fence, not a row lock, is what holds
/// the answer still, and a row lock here would put `repos` into the write
/// lane's order twice.
const REPO_REGISTERED_SQL: &str = "\
SELECT EXISTS(
    SELECT 1
      FROM proxima_code.repos
     WHERE owner_kind = $1 AND owner_id = $2 AND repo_id = $3
)";

/// Whether `repo_id` is still registered for `owner`, asked in a
/// flavor-owned transaction that already holds the scope fence.
///
/// # Errors
///
/// Database errors from the read.
pub(crate) async fn repo_registered_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<bool, RepoRegistryError> {
    let (kind, owner_id) = owner.columns();
    Ok(sqlx::query_scalar(REPO_REGISTERED_SQL)
        .bind(kind)
        .bind(owner_id)
        .bind(repo_id)
        .fetch_one(&mut **tx)
        .await?)
}

#[cfg(test)]
mod tests {
    use super::{CODE_REPO_SCOPE, CODE_REPO_SCOPE_DECL, REPO_REGISTERED_SQL};

    /// The declaration and the one hand-written statement left in this
    /// module must agree on every name. They are two readings of the same
    /// registry table — one the substrate generates its probe from, one the
    /// runs lane executes — and a drift between them is a fence asking
    /// about a table nobody writes.
    #[test]
    fn the_declaration_and_the_registration_probe_name_the_same_columns() {
        assert_eq!(CODE_REPO_SCOPE_DECL.kind, CODE_REPO_SCOPE);
        for name in [
            CODE_REPO_SCOPE_DECL.registry_table,
            CODE_REPO_SCOPE_DECL.id_column,
            CODE_REPO_SCOPE_DECL.owner_kind_column,
            CODE_REPO_SCOPE_DECL.owner_id_column,
        ] {
            assert!(
                REPO_REGISTERED_SQL.contains(name),
                "the registration probe does not name {name}"
            );
        }
    }

    /// A shape the substrate refuses at freeze is a shape this flavor must
    /// not ship: a schema-qualified table, and no empty column name.
    #[test]
    fn the_declaration_is_one_the_freeze_accepts() {
        assert!(CODE_REPO_SCOPE_DECL.shape_error().is_none());
    }
}

//! The flavor-declared lifecycle scopes, resolved into the two statements
//! storage needs.
//!
//! A [`ScopeDecl`] is names: a registry table, its id column, its two owner
//! columns. This module turns those names into a liveness probe and hands
//! back the fence target the admission path locks on. It SPELLS NOTHING OF
//! ITS OWN — every identifier in the generated statement comes off the
//! declaration and through [`PgIdent`], which is the repo's standing rule
//! for splicing a build-time name into SQL.
//!
//! Resolution happens once, at `PgStorage::with_flavors`, because the
//! alternative is regenerating one statement per scoped write. A
//! declaration that survives core's freeze but fails `PgIdent` (core insists
//! on a qualified table; `PgIdent` insists on a legal identifier) is kept as
//! its error rather than dropped: a dropped declaration is a write that
//! silently skips the fence, which is the whole failure this feature exists
//! to close.

use std::collections::BTreeMap;
use std::sync::Arc;

use proxima_core::scope::{ScopeDecl, ScopeKind, ScopeRef};
use proxima_core::{SidecarPayload, StorageError};

use crate::pg_ident::PgIdent;

/// One scope's fence key material plus the probe that answers whether its
/// registry row is still there.
///
/// Built from a declaration, carried by value into the admission because
/// the admission runs on a `&mut Transaction` and must not reach back into
/// the storage handle to look a declaration up mid-write.
#[derive(Debug, Clone)]
pub(crate) struct ScopeFenceTarget {
    pub(crate) kind: ScopeKind,
    pub(crate) id: uuid::Uuid,
    /// `SELECT EXISTS(...)`, generated from the declaration.
    pub(crate) probe_sql: Arc<str>,
}

/// Every linked flavor's scope declaration, resolved.
///
/// Defaults to empty, exactly as `PgStorage::surfaces` defaults to flavor
/// #0's: a storage built without `with_flavors` knows of no scope, and an
/// admission of a scoped payload against it is REFUSED rather than admitted
/// unfenced. Fail closed is the only safe reading of "the host did not tell
/// me where this scope's registry lives".
#[derive(Debug, Clone, Default)]
pub struct ScopeSurfaces {
    probes: BTreeMap<ScopeKind, Result<Arc<str>, String>>,
}

impl ScopeSurfaces {
    /// Resolve every declaration the frozen registry carries.
    ///
    /// Infallible by construction so it composes into the `with_*` builder
    /// chain; a declaration `PgIdent` refuses is stored as its message and
    /// surfaces at the first write into that scope, naming the declaration.
    #[must_use]
    pub fn for_registry(registry: &proxima_core::FlavorRegistryFrozen) -> Self {
        let mut probes = BTreeMap::new();
        for decl in registry.scopes() {
            probes.insert(decl.kind, generate_probe(decl));
        }
        Self { probes }
    }

    /// The fence targets for one write's payloads: the distinct scopes they
    /// declare, sorted and deduplicated.
    ///
    /// Sorted because a batch may span several scopes and two crossed
    /// batches taking the same keys in opposite orders would deadlock —
    /// the rule the multi-owner fence already follows. Deduplicated because
    /// N chunks of one repository are one lane, not N.
    ///
    /// # Errors
    ///
    /// [`StorageError::Internal`] when a payload names a scope kind this
    /// binary has no declaration for, or one whose declaration does not
    /// splice. Both are build-time faults, and both fail the write rather
    /// than admitting it unfenced.
    pub(crate) fn targets_for_payloads(
        &self,
        payloads: &[SidecarPayload],
    ) -> Result<Vec<ScopeFenceTarget>, StorageError> {
        self.targets(payloads.iter().filter_map(SidecarPayload::scope))
    }

    pub(crate) fn targets(
        &self,
        scopes: impl IntoIterator<Item = ScopeRef>,
    ) -> Result<Vec<ScopeFenceTarget>, StorageError> {
        let refs = proxima_core::scope::scope_set(scopes);
        let mut out = Vec::with_capacity(refs.len());
        for scope in refs {
            out.push(ScopeFenceTarget {
                kind: scope.kind,
                id: scope.id,
                probe_sql: Arc::clone(self.probe(scope.kind)?),
            });
        }
        Ok(out)
    }

    fn probe(&self, kind: ScopeKind) -> Result<&Arc<str>, StorageError> {
        match self.probes.get(&kind) {
            Some(Ok(sql)) => Ok(sql),
            Some(Err(message)) => Err(StorageError::Internal(format!(
                "lifecycle scope {kind} has an unspliceable declaration: {message}"
            ))),
            None => Err(StorageError::Internal(format!(
                "no linked flavor declares lifecycle scope {kind}; a scoped payload cannot be \
                 admitted without the declaration that names its registry"
            ))),
        }
    }

    /// The generated probe for `kind`, for the acceptance test that reads
    /// the statement rather than trusting the generator.
    #[must_use]
    pub fn probe_sql(&self, kind: ScopeKind) -> Option<&str> {
        match self.probes.get(&kind) {
            Some(Ok(sql)) => Some(sql.as_ref()),
            _ => None,
        }
    }
}

/// `SELECT EXISTS(SELECT 1 FROM <table> WHERE <kind> = $1 AND <owner> = $2
/// AND <id> = $3)`.
///
/// Deliberately NOT `FOR SHARE`: the fence, not a row lock, is what holds
/// the answer still for the life of the transaction, and a row lock here
/// would put the flavor's registry into the write lane's lock order a
/// second time.
fn generate_probe(decl: &ScopeDecl) -> Result<Arc<str>, String> {
    let table = PgIdent::table(decl.registry_table).map_err(|err| err.to_string())?;
    let owner_kind = PgIdent::column(decl.owner_kind_column).map_err(|err| err.to_string())?;
    let owner_id = PgIdent::column(decl.owner_id_column).map_err(|err| err.to_string())?;
    let id = PgIdent::column(decl.id_column).map_err(|err| err.to_string())?;
    Ok(Arc::from(format!(
        "SELECT EXISTS(\n    SELECT 1\n      FROM {}\n     WHERE {} = $1 AND {} = $2 AND {} = $3\n)",
        table.as_str(),
        owner_kind.as_str(),
        owner_id.as_str(),
        id.as_str(),
    )))
}

#[cfg(test)]
mod tests {
    use super::{ScopeSurfaces, generate_probe};
    use proxima_core::scope::{ScopeDecl, ScopeKind};

    const KIND: ScopeKind = ScopeKind::new("code-repo");

    const DECL: ScopeDecl = ScopeDecl {
        kind: KIND,
        registry_table: "proxima_code.repos",
        id_column: "repo_id",
        owner_kind_column: "owner_kind",
        owner_id_column: "owner_id",
    };

    /// Every name in the probe comes off the declaration. A generator that
    /// hardcoded `repo_id` would pass for the code flavor and silently
    /// probe the wrong column for the next one.
    #[test]
    fn the_probe_spells_only_what_the_declaration_names() {
        let sql = generate_probe(&DECL).expect("the shipped declaration splices");
        assert!(sql.contains("FROM proxima_code.repos"));
        assert!(sql.contains("owner_kind = $1"));
        assert!(sql.contains("owner_id = $2"));
        assert!(sql.contains("repo_id = $3"));

        let renamed = generate_probe(&ScopeDecl {
            id_column: "book_id",
            registry_table: "shelf.books",
            ..DECL
        })
        .expect("a renamed declaration splices");
        assert!(renamed.contains("FROM shelf.books"));
        assert!(renamed.contains("book_id = $3"));
        assert!(!renamed.contains("repo_id"));
    }

    #[test]
    fn an_unspliceable_declaration_is_kept_as_its_error() {
        let err = generate_probe(&ScopeDecl {
            registry_table: "proxima-code.repos",
            ..DECL
        })
        .expect_err("a hyphenated schema name is not an identifier");
        assert!(err.contains("proxima-code.repos"), "{err}");
    }

    /// An unknown kind is a refusal, never a silent pass. A storage that was
    /// never told where a scope's registry lives must not admit rows into it.
    #[test]
    fn an_undeclared_scope_refuses_rather_than_skipping_the_fence() {
        let surfaces = ScopeSurfaces::default();
        let err = surfaces
            .targets([proxima_core::scope::ScopeRef::new(
                KIND,
                uuid::Uuid::from_u128(1),
            )])
            .expect_err("an undeclared scope cannot be fenced");
        assert!(err.to_string().contains("code-repo"), "{err}");
    }
}

/// Proof that the frozen sidecar registry is the caller of a typed sidecar
/// insert.
///
/// **Invariant: a typed memory sidecar row is written only by
/// `PgSidecarRegistryFrozen::insert_memory_sidecar`**, which runs
/// the schema's generated search-projection `INSERT` from the same registry
/// entry, in the same transaction, one statement later. A sidecar row
/// without its projection row is a memory that is stored and unfindable, so
/// the write that would produce one must not be reachable on its own.
///
/// The split this type makes is between IMPLEMENTING and INVOKING. A flavor
/// implements [`super::PgMemorySidecar`] /
/// [`crate::verbs::fact_ingest::PgFactSidecar`] — the `pg_sidecar!` macro
/// writes the body — and names this type in the signature it implements. It
/// cannot construct one, because the field is crate-private, so it cannot
/// call what it implemented. Same `_private: ()` idiom as the core write
/// permits.
///
/// [`super::PgSidecarRegistryFrozen::rebuild_projection_for_table`] takes no
/// permit and needs none: rebuild re-derives projection rows FROM sidecar
/// rows, so invoking it can only restore the invariant, never break it.
///
/// A downstream crate cannot mint one:
///
/// ```compile_fail
/// // `SidecarInsertPermit`'s constructor is crate-private, so no caller
/// // outside `proxima-storage-pg` can produce the argument a typed sidecar
/// // insert demands.
/// let _permit = proxima_storage_pg::sidecars::SidecarInsertPermit::new();
/// ```
#[derive(Debug, Clone, Copy)]
pub struct SidecarInsertPermit {
    _private: (),
}

impl SidecarInsertPermit {
    /// Mint the permit. Crate-private on purpose — see the type doc.
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self { _private: () }
    }
}

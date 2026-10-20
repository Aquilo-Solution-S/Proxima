//! Base tables no flavor contract declares, each with the reason why.
//!
//! The erase partition, the export bundle and the transfer sweep all
//! enumerate DECLARED surfaces, so none of them can ask whether a table was
//! forgotten. The catalog test asks it from the other side: every base table
//! in `proxima_core` is a declared surface or an entry here, and every entry
//! here names a table that exists and is not declared (#478).
//!
//! Migration bookkeeping (`_sqlx_migrations`) is excluded structurally.

/// `(table, why)`: a store-wide relation that is about no owner, so no
/// owner verb reaches it.
pub const STORE_WIDE_TABLES: &[(&str, &str)] = &[
    (
        "proxima_core.export_erase_fence",
        "Never holds a row. Owner export and erase lock it to exclude each other (0049, #493); \
         only its table lock is used.",
    ),
    (
        "proxima_core.maintenance_lease",
        "Which process holds each store-wide maintenance singleton and until when (0049, #493). \
         Coordination state about no owner.",
    ),
    (
        "proxima_core.store_identity",
        "The deployment's immutable numeric routing identity. This singleton is platform \
         configuration shared by every owner, with no owner data to export, transfer or erase.",
    ),
    (
        "proxima_core.owner_rls_epoch",
        "Platform-owned RLS epoch state used to invalidate owner-scope claims; it is migration \
         and runtime metadata, not owner data or a flavor surface.",
    ),
    (
        "proxima_core.closed_handle",
        "A dormant tombstone domain: no verb writes it, only tests do. The pin trigger (0035) \
         refuses a new pin to a listed handle, so a row records that a handle may never be \
         pinned again, a fact about the handle space rather than about any owner. Dropping the \
         table is a separate decision.",
    ),
    (
        "proxima_core.flavor_surface",
        "The registry's own stamp domain: which tables a memory row may stamp. Schema metadata \
         written by migrations, identical for every owner, and the relation the surface \
         declarations are checked AGAINST.",
    ),
    (
        "proxima_core.owner_follows",
        "Which flavor tables are Follows sidecars: schema metadata the host writes from its \
         registry at migration, identical for every owner, and the relation the installer reads \
         to give those tables the follows owner RLS.",
    ),
    (
        "proxima_core.lexical_languages",
        "The FK domain of permitted regconfig values. Schema metadata written by migrations, \
         identical for every owner.",
    ),
    (
        "proxima_core.lexical_default",
        "The deployment's default lexical configuration: one row, an operator setting, owned \
         by nobody.",
    ),
    (
        "proxima_core.installation",
        "This deployment's own identity: one row minted at install, write-once, identical for \
         every owner and about none of them. Erasing it would orphan every broker message \
         already stamped with it.",
    ),
    (
        "proxima_core.publication_consumer",
        "The deployment's registry of outbox consumers: operator configuration, \
         platform-written, identical for every owner and about none of them.",
    ),
];

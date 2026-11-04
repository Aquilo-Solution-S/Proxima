//! Spiice-scoped erase — typed surface only.
//!
//! [`crate::UnitOfWork::erase_own_series`] hard-erases whole series of ONE
//! spiice's own memory schemas under ONE owner (docs/data-and-erasure.md
//! erase). The storage body lives in `proxiima-storage-pg`.

use crate::error::ProtocolError;
use crate::verbs::query::SidecarAtom;
use crate::{MemoryId, OwnerId, SchemaId};

/// Series one call selects: the most seeds a paged selection takes, and the
/// most series an `Ids` list may close over. The series that reference a
/// seed are not counted; [`MAX_ERASE_VERSIONS_PER_CALL`] bounds them.
pub const MAX_ERASE_SERIES_PER_CALL: usize = 256;

/// Versions one call may erase: a page.
///
/// The bound that protects the database: the lifecycle lock and the
/// erase-witness trigger each take one lock per erased row, so an unbounded
/// set can exhaust the shared lock table. A selection other than
/// [`SeriesSelection::Ids`] whose reference closure is larger is erased
/// across calls, referrers first, never refused; re-erasing an erased series
/// is a no-op, so calling again is safe.
pub const MAX_ERASE_VERSIONS_PER_CALL: usize = 1024;

/// What one [`crate::UnitOfWork::erase_own_series`] call selects.
///
/// Every selected admission is expanded to its WHOLE series, live and forgotten:
/// the erase never prunes a series' history. Every variant but `Ids` names
/// one own schema, selects only `owner`'s series of it, and pages: a call
/// erases the next page of the selection, oldest series first, and sets
/// [`SeriesEraseReceipt::more_remaining`] while any of it waits.
///
/// A page is the next [`MAX_ERASE_VERSIONS_PER_CALL`] versions of the
/// selection's reference closure, referrers first: a series whose rows
/// reference another goes before it, and a selected series goes after every
/// series that references it. Each page is one commit, and none leaves a row
/// referencing an erased version. The next call selects the same oldest
/// series again, with a closure smaller by what was erased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeriesSelection {
    /// Explicit memory ids, any version of a series. An id of no admission
    /// (already erased, or never admitted) selects nothing. The ids and
    /// their whole closure are one page: over either cap
    /// ([`MAX_ERASE_SERIES_PER_CALL`] series, [`MAX_ERASE_VERSIONS_PER_CALL`]
    /// versions) the call is refused before any delete.
    Ids(Vec<MemoryId>),
    /// Every series of `schema` whose NEWEST version was admitted before
    /// `cutoff`, oldest first, in pages. Admission time is the version's
    /// `UUIDv8` `t` at millisecond precision: the substrate stamps it, so a
    /// payload can neither back-date nor future-date it.
    AdmittedBefore {
        schema: SchemaId,
        cutoff: time::OffsetDateTime,
    },
    /// Every series of `schema` with a hot version whose row in the
    /// schema's sidecar matches every `(column, value)` pair, oldest match
    /// first, in pages. The predicates are `AND`-joined equality over
    /// the sidecar's own columns — the
    /// [`crate::storage_ports::SidecarSessionRead`] shape — at least one.
    /// Any matching version selects its series: the unit is the series, not
    /// the version. A forgotten version keeps its sidecar row (#488), so it
    /// matches like a live one.
    SidecarEquals {
        schema: SchemaId,
        predicates: Vec<(String, SidecarAtom)>,
    },
    /// Every series of `schema` whose NEWEST version's `column` — a
    /// `timestamptz` column of the schema's sidecar — is older than
    /// `cutoff`, oldest value first, in pages. The clock is the one the
    /// payload declares, so a backfilled event ages by when it happened; a
    /// series whose newest version carries a later value keeps every
    /// version. A newest version that holds `NULL` there has no value to
    /// compare and keeps its series; a forgotten one is compared like a live
    /// one, since it keeps its sidecar row (#488).
    DeclaredBefore {
        schema: SchemaId,
        column: String,
        cutoff: time::OffsetDateTime,
    },
    /// Every series of `schema` whose NEWEST version is older than `cutoff`
    /// by the memory's own clock: its `occurred_at` for a Fact, else its
    /// admission time (as [`Self::AdmittedBefore`] reads it). Oldest first,
    /// in pages. A backfilled Fact ages by when it happened, and an
    /// Abstraction or Perspective by when it was admitted. This is the
    /// clock an owner's `retention` row ages a schema by.
    OccurredBefore {
        schema: SchemaId,
        cutoff: time::OffsetDateTime,
    },
}

impl SeriesSelection {
    /// The own schema the selection names; `None` for `Ids`.
    #[must_use]
    pub const fn schema(&self) -> Option<&SchemaId> {
        match self {
            Self::Ids(_) => None,
            Self::AdmittedBefore { schema, .. }
            | Self::SidecarEquals { schema, .. }
            | Self::DeclaredBefore { schema, .. }
            | Self::OccurredBefore { schema, .. } => Some(schema),
        }
    }
}

/// Whether the erase commits or reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EraseMode {
    Erase,
    /// The same path, rolled back: the receipt reports what `Erase` would
    /// destroy and the database is unchanged.
    DryRun,
}

/// What one spiice-scoped erase destroyed, or under [`EraseMode::DryRun`]
/// would destroy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesEraseReceipt {
    pub spiice_id: &'static str,
    pub owner: OwnerId,
    pub mode: EraseMode,
    /// Every version erased, ascending by `t`.
    pub versions: Vec<MemoryId>,
    pub series_erased: u64,
    pub versions_erased: u64,
    /// Of `series_erased`, the series that joined because one of their rows
    /// references the selection through a foreign key (a reference to erased
    /// data is erased with it).
    pub referencing_series: u64,
    /// Cited blobs no remaining admission cites.
    pub blobs_removed: u64,
    /// Objects enqueued in `cold_purge_pending`; destroyed after commit.
    pub cold_objects_pending: u64,
    /// Memories outside the erase whose `origins[]` or `refs[]` name an
    /// erased `t`. The graph is diminished, not refused.
    pub dangling_pins: u64,
    /// A selection other than [`SeriesSelection::Ids`] erased one page and
    /// has more left, in this closure or in a later series: call again.
    pub more_remaining: bool,
}

/// Why a selection was refused. Nothing was deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeriesEraseRefusalKind {
    /// A version is not one of the named spiice's memory schemas: a core
    /// schema, or another spiice's.
    ForeignSchema,
    /// A selected id, a version of a selected series, or a row referencing
    /// the erase set belongs to another owner.
    CrossOwner,
    /// A row of a table that is not a registered memory sidecar references
    /// the erase set, so its erase has no series to join.
    UnerasableReference,
    /// An [`SeriesSelection::Ids`] list closes over more series or versions
    /// than one call may erase ([`MAX_ERASE_SERIES_PER_CALL`],
    /// [`MAX_ERASE_VERSIONS_PER_CALL`]), or any selection reaches one
    /// indivisible unit over the version cap: a single series, or a ring of
    /// series that reference one another. A paged selection is otherwise
    /// never refused for its size, and is refused before the first page.
    OverCap,
}

impl SeriesEraseRefusalKind {
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::ForeignSchema => "selection reaches a schema the spiice does not declare",
            Self::CrossOwner => "selection reaches rows of another owner",
            Self::UnerasableReference => {
                "selection is referenced from a table that is not a memory sidecar"
            }
            Self::OverCap => "selection exceeds the per-call erase cap",
        }
    }
}

/// A refused selection and what refused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesEraseRefusal {
    pub kind: SeriesEraseRefusalKind,
    /// One entry per offender: `t=<id>`, `t=<id> schema=<schema>`,
    /// `<table>.<column> t=<id>`, or the counts over the cap.
    pub offending: Vec<String>,
}

impl std::fmt::Display for SeriesEraseRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.message(), self.offending.join(", "))
    }
}

/// [`crate::UnitOfWork::erase_own_series`] failed.
#[derive(Debug, Clone)]
pub enum SeriesEraseError {
    /// The selection is outside the spiice's scope or the cap.
    Refused(SeriesEraseRefusal),
    /// A transient conflict (deadlock, lock wait timeout, a series replaced
    /// mid-erase). Nothing was deleted; run the whole unit again.
    Retryable(String),
    /// Authorization, declaration, or storage failure.
    Protocol(ProtocolError),
}

impl std::fmt::Display for SeriesEraseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refusal) => refusal.fmt(f),
            Self::Retryable(message) => write!(f, "retryable erase conflict: {message}"),
            Self::Protocol(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for SeriesEraseError {}

impl From<ProtocolError> for SeriesEraseError {
    fn from(err: ProtocolError) -> Self {
        Self::Protocol(err)
    }
}

impl From<SeriesEraseError> for ProtocolError {
    fn from(err: SeriesEraseError) -> Self {
        match err {
            SeriesEraseError::Refused(refusal) => match refusal.kind {
                SeriesEraseRefusalKind::CrossOwner => Self::forbidden(refusal.to_string()),
                SeriesEraseRefusalKind::ForeignSchema
                | SeriesEraseRefusalKind::UnerasableReference
                | SeriesEraseRefusalKind::OverCap => {
                    Self::invalid_argument("selection", refusal.to_string())
                }
            },
            SeriesEraseError::Retryable(message) => {
                Self::internal(format!("retryable erase conflict: {message}"))
            }
            SeriesEraseError::Protocol(err) => err,
        }
    }
}

/// The backend half of one erase: what the engine resolved and authorized.
#[derive(Debug, Clone, Copy)]
pub struct SeriesEraseRequest<'a> {
    /// The named spiice's memory schema ids. Every erased version's series
    /// must carry one of them.
    pub own_schemas: &'a [String],
    pub selection: &'a SeriesSelection,
    /// The selection schema's sidecar table, off the spiice contract. Set
    /// for [`SeriesSelection::SidecarEquals`] and
    /// [`SeriesSelection::DeclaredBefore`], the selections that read it.
    pub sidecar_table: Option<&'a str>,
    pub max_series: usize,
    pub max_versions: usize,
}

/// What the backend erased, before commit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeriesEraseReport {
    pub versions: Vec<MemoryId>,
    pub series_erased: u64,
    pub referencing_series: u64,
    pub blobs_removed: u64,
    pub cold_objects_pending: u64,
    pub dangling_pins: u64,
    pub more_remaining: bool,
}

/// The backend's answer: erased (uncommitted), or refused with nothing
/// deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeriesEraseOutcome {
    Erased(SeriesEraseReport),
    Refused(SeriesEraseRefusal),
}

#[cfg(test)]
mod tests {
    use super::{SeriesEraseError, SeriesEraseRefusal, SeriesEraseRefusalKind};
    use crate::error::{ErrorCode, ProtocolError};

    #[test]
    fn a_refusal_names_every_offender_and_maps_to_a_protocol_code() {
        let refusal = SeriesEraseRefusal {
            kind: SeriesEraseRefusalKind::CrossOwner,
            offending: vec!["t=a".into(), "proxiima_x.y.z t=b".into()],
        };
        assert_eq!(
            refusal.to_string(),
            "selection reaches rows of another owner: t=a, proxiima_x.y.z t=b"
        );
        let err: ProtocolError = SeriesEraseError::Refused(refusal).into();
        assert_eq!(err.code, ErrorCode::Forbidden);

        let over: ProtocolError = SeriesEraseError::Refused(SeriesEraseRefusal {
            kind: SeriesEraseRefusalKind::OverCap,
            offending: vec!["series=300 > 256".into()],
        })
        .into();
        assert_eq!(over.code, ErrorCode::InvalidArgument);
    }
}

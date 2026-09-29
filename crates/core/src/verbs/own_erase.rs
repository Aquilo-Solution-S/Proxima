//! Flavor-scoped erase — typed surface only.
//!
//! [`crate::UnitOfWork::erase_own_series`] hard-erases whole series of ONE
//! flavor's own memory schemas under ONE owner (docs/13 §Flavor-scoped
//! erase). The storage body lives in `proxima-storage-pg`.

use crate::error::ProtocolError;
use crate::verbs::query::SidecarAtom;
use crate::{MemoryId, Owner, SchemaId};

/// Series one call may erase, the selection's reference closure included.
pub const MAX_ERASE_SERIES_PER_CALL: usize = 256;

/// Versions one call may erase.
///
/// The bound that protects the database: the lifecycle lock and the
/// erase-witness trigger each take one lock per erased row, so an unbounded
/// set can exhaust the shared lock table. Callers page; re-erasing an erased
/// series is a no-op, so paging is safe.
pub const MAX_ERASE_VERSIONS_PER_CALL: usize = 1024;

/// What one [`crate::UnitOfWork::erase_own_series`] call selects.
///
/// Every selected admission is expanded to its WHOLE series, hot and cooled:
/// the erase never prunes a series' history. Every variant but `Ids` names
/// one own schema, selects only `owner`'s series of it, and pages: a call
/// stops at the cap and sets [`SeriesEraseReceipt::more_remaining`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeriesSelection {
    /// Explicit memory ids, any version of a series. An id of no admission
    /// (already erased, or never admitted) selects nothing. Over the cap the
    /// call is refused before any delete.
    Ids(Vec<MemoryId>),
    /// Every series of `schema` whose NEWEST version was admitted before
    /// `cutoff`, oldest first, up to the cap. Admission time is the version's
    /// `UUIDv7` `t` at millisecond precision: the substrate stamps it, so a
    /// payload can neither back-date nor future-date it.
    AdmittedBefore {
        schema: SchemaId,
        cutoff: time::OffsetDateTime,
    },
    /// Every series of `schema` with a hot version whose row in the
    /// schema's sidecar matches every `(column, value)` pair, oldest match
    /// first, up to the cap. The predicates are `AND`-joined equality over
    /// the sidecar's own columns — the
    /// [`crate::storage_ports::SidecarSessionRead`] shape — at least one.
    /// Any matching version selects its series: the unit is the series, not
    /// the version. A cooled version's row lives in its cold object only, so
    /// it matches nothing.
    SidecarEquals {
        schema: SchemaId,
        predicates: Vec<(String, SidecarAtom)>,
    },
    /// Every series of `schema` whose NEWEST version's `column` — a
    /// `timestamptz` column of the schema's sidecar — is older than
    /// `cutoff`, oldest value first, up to the cap. The clock is the one the
    /// payload declares, so a backfilled event ages by when it happened; a
    /// series whose newest version carries a later value keeps every
    /// version. A newest version that is cooled, or holds `NULL` there, has
    /// no value to compare and keeps its series.
    DeclaredBefore {
        schema: SchemaId,
        column: String,
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
            | Self::DeclaredBefore { schema, .. } => Some(schema),
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

/// What one flavor-scoped erase destroyed, or under [`EraseMode::DryRun`]
/// would destroy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesEraseReceipt {
    pub flavor_id: &'static str,
    pub owner: Owner,
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
    /// Cold objects enqueued in `cold_purge_pending`; destroyed after commit.
    pub cold_objects_pending: u64,
    /// Memories outside the erase whose `origins[]` or `refs[]` name an
    /// erased `t`. The graph is diminished, not refused.
    pub dangling_pins: u64,
    /// A selection other than [`SeriesSelection::Ids`] stopped at the cap
    /// with matching series left: call again.
    pub more_remaining: bool,
}

/// Why a selection was refused. Nothing was deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeriesEraseRefusalKind {
    /// A version is not one of the named flavor's memory schemas: a core
    /// schema, or another flavor's.
    ForeignSchema,
    /// A selected id, a version of a selected series, or a row referencing
    /// the erase set belongs to another owner.
    CrossOwner,
    /// A row of a table that is not a registered memory sidecar references
    /// the erase set, so its erase has no series to join.
    UnerasableReference,
    /// The selection closes over more series or versions than one call may
    /// erase ([`MAX_ERASE_SERIES_PER_CALL`], [`MAX_ERASE_VERSIONS_PER_CALL`]).
    OverCap,
}

impl SeriesEraseRefusalKind {
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::ForeignSchema => "selection reaches a schema the flavor does not declare",
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
    /// The selection is outside the flavor's scope or the cap.
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
    /// The named flavor's memory schema ids. Every erased version's series
    /// must carry one of them.
    pub own_schemas: &'a [String],
    pub selection: &'a SeriesSelection,
    /// The selection schema's sidecar table, off the flavor contract. Set
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
            offending: vec!["t=a".into(), "proxima_x.y.z t=b".into()],
        };
        assert_eq!(
            refusal.to_string(),
            "selection reaches rows of another owner: t=a, proxima_x.y.z t=b"
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
